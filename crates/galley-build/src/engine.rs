//! An engine turns a project into a PDF. Tectonic is the default. The interface stays generic, so
//! that TeX Live with `latexmk` and Typst can be added later.

use std::path::{Path, PathBuf};

use crate::sandbox::{Mount, Mounts, Sandbox, Spec, GUEST_WORK};
use crate::{Error, Result};

const GUEST_BIN: &str = "/galley/bin/tectonic";
const GUEST_CACHE: &str = "/galley/cache";

#[derive(Debug, Clone)]
pub struct Tectonic {
    pub binary: PathBuf,
    pub cache_dir: PathBuf,
}

/// One compile invocation, ready for the sandbox.
pub struct CompileSpec {
    pub spec: Spec,
    /// The stem of the output file inside `.galley/build`, such as `main` or `galley-draft`.
    pub stem: String,
}

impl Tectonic {
    /// Find Tectonic. Look at the configured path, then `<data_dir>/tectonic/tectonic`, then `$PATH`.
    pub fn locate(data_dir: &Path, configured: Option<&str>) -> Option<Tectonic> {
        let cache_dir = data_dir.join("tectonic").join("cache");
        let candidates = [
            configured.filter(|s| !s.is_empty()).map(PathBuf::from),
            Some(data_dir.join("tectonic").join("tectonic")),
            which("tectonic"),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_file())
            .map(|binary| Tectonic { binary, cache_dir })
    }

    pub fn install_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("tectonic")
    }

    /// Build the sandbox spec that compiles `main_file`. `fetch` permits network access, so that
    /// the engine can pull missing bundle files into the shared cache. runner.rs says when.
    pub fn compile(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        main_file: &str,
        draft: bool,
        fetch: bool,
        _timeout_s: u64,
    ) -> Result<CompileSpec> {
        let out_dir = project_dir.join(".galley").join("build");
        std::fs::create_dir_all(&out_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;

        let mounts = vec![
            Mount {
                host: self.binary.clone(),
                guest: PathBuf::from(GUEST_BIN),
                writable: false,
            },
            Mount {
                host: self.cache_dir.clone(),
                guest: PathBuf::from(GUEST_CACHE),
                writable: fetch,
            },
        ];
        let map: Mounts = sandbox.mounts(project_dir, &mounts);

        let (input, stem) = if draft {
            let class = documentclass(&std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default())
                .unwrap_or_else(|| "article".to_string());
            let main_no_ext = main_file.strip_suffix(".tex").unwrap_or(main_file);
            let wrapper = format!(
                "\\PassOptionsToClass{{draft}}{{{class}}}\n\\input{{{main_no_ext}}}\n"
            );
            std::fs::write(out_dir.join("galley-draft.tex"), wrapper)?;
            (
                map.guest(&out_dir.join("galley-draft.tex")).to_string_lossy().into_owned(),
                "galley-draft".to_string(),
            )
        } else {
            let stem = Path::new(main_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::Engine(format!("{main_file} is not a valid main file")))?
                .to_string();
            (
                map.guest(&project_dir.join(main_file)).to_string_lossy().into_owned(),
                stem,
            )
        };
        self.build_spec(sandbox, project_dir, &out_dir, &map, input, stem, fetch)
    }

    /// Compile a file that is already in `.galley/build`, such as the latexdiff output. Its
    /// `search-path` still points at the project, so `\input` and the figures resolve.
    pub fn compile_generated(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        build_file: &str,
        stem: &str,
        fetch: bool,
    ) -> Result<CompileSpec> {
        let out_dir = project_dir.join(".galley").join("build");
        std::fs::create_dir_all(&out_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        let mounts = vec![
            Mount { host: self.binary.clone(), guest: PathBuf::from(GUEST_BIN), writable: false },
            Mount { host: self.cache_dir.clone(), guest: PathBuf::from(GUEST_CACHE), writable: fetch },
        ];
        let map: Mounts = sandbox.mounts(project_dir, &mounts);
        let input = map.guest(&out_dir.join(build_file)).to_string_lossy().into_owned();
        self.build_spec(sandbox, project_dir, &out_dir, &map, input, stem.to_string(), fetch)
    }

    /// Assemble the sandboxed Tectonic command for a resolved input and stem.
    #[allow(clippy::too_many_arguments)]
    fn build_spec(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        out_dir: &Path,
        map: &Mounts,
        input: String,
        stem: String,
        fetch: bool,
    ) -> Result<CompileSpec> {
        let guest_out = map.guest(out_dir);
        let guest_work = map.guest(project_dir);
        let mounts = vec![
            Mount { host: self.binary.clone(), guest: PathBuf::from(GUEST_BIN), writable: false },
            Mount { host: self.cache_dir.clone(), guest: PathBuf::from(GUEST_CACHE), writable: fetch },
        ];

        // Shell escape with \write18 is opt-in in Tectonic. Galley never passes `-Z shell-escape`,
        // so shell escape stays off. The sandbox is the real boundary: it has no network and a
        // read-only project (spec §6.2). `search-path` lets the draft wrapper in .galley/build
        // resolve \input{main} and the figures. Galley does not pass `--untrusted`, because that
        // flag disables search-path.
        let mut args: Vec<String> = [
            "-X",
            "compile",
            "-Z",
            "continue-on-errors",
            "-Z",
            &format!("search-path={}", guest_work.display()),
            "--keep-logs",
            "--keep-intermediates",
            "--synctex",
        ]
        .map(String::from)
        .to_vec();
        // An SVG figure converted by crate::svg resolves as `./svg-inkscape/…` through this path.
        let svg_root = project_dir.join(crate::svg::ROOT);
        if svg_root.is_dir() {
            args.extend(["-Z".to_string(), format!("search-path={}", map.guest(&svg_root).display())]);
        }
        if !fetch {
            args.push("--only-cached".into());
        }
        args.extend(["-o".to_string(), guest_out.to_string_lossy().into_owned(), input]);

        let env = vec![
            ("TECTONIC_CACHE_DIR".to_string(), map.guest(&self.cache_dir).to_string_lossy().into_owned()),
            ("HOME".to_string(), "/tmp".to_string()),
            ("SOURCE_DATE_EPOCH".to_string(), "0".to_string()),
        ];
        let program = map.guest(&self.binary);
        // The name is unique for each run. `docker run --rm` removes the previous container
        // asynchronously, so the same name makes a quick rebuild fail with "name already in use".
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "galley-build-{}-{}-{}",
            project_dir.file_name().and_then(|n| n.to_str()).unwrap_or("p"),
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        // A tmpfs needs a mountpoint that exists under the read-only project. Mask only the
        // directories that are there.
        let masked = if sandbox.kind == crate::SandboxKind::None {
            Vec::new()
        } else {
            [".git", ".galley/docs", ".galley/agents"]
                .iter()
                .filter(|rel| project_dir.join(rel).is_dir())
                .map(|rel| PathBuf::from(format!("{GUEST_WORK}/{rel}")))
                .collect()
        };
        Ok(CompileSpec {
            spec: Spec {
                project_dir: project_dir.to_path_buf(),
                out_dir: out_dir.to_path_buf(),
                mounts,
                masked,
                env,
                network: fetch,
                program,
                args,
                name,
            },
            stem,
        })
    }
}

/// The class named in `\documentclass[...]{class}`.
pub fn documentclass(main: &str) -> Option<String> {
    let re = regex::Regex::new(r"\\documentclass\s*(?:\[[^\]]*\])?\s*\{([^}]+)\}").ok()?;
    re.captures(main).map(|c| c[1].trim().to_string())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_documentclass() {
        assert_eq!(documentclass("\\documentclass[11pt]{article}").as_deref(), Some("article"));
        assert_eq!(documentclass("% x\n\\documentclass{ neurips_2026 }").as_deref(), Some("neurips_2026"));
        assert_eq!(documentclass("hello"), None);
    }
}
