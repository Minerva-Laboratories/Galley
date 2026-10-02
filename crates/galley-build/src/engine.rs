//! An engine turns a project into a PDF. Tectonic is the default; TeX Live engines use `latexmk`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::sandbox::{GUEST_WORK, Mount, Mounts, Sandbox, Spec};
use crate::{Error, Result};

const GUEST_BIN: &str = "/galley/bin/tectonic";
const GUEST_TEX_BUILD: &str = "/tmp/galley-build";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    #[default]
    Tectonic,
    PdfLatex,
    XeLatex,
    LuaLatex,
    Latex,
}

impl EngineKind {
    pub const ALL: [Self; 5] = [
        Self::Tectonic,
        Self::PdfLatex,
        Self::XeLatex,
        Self::LuaLatex,
        Self::Latex,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::Tectonic => "tectonic",
            Self::PdfLatex => "pdflatex",
            Self::XeLatex => "xelatex",
            Self::LuaLatex => "lualatex",
            Self::Latex => "latex",
        }
    }
    pub fn executable(self) -> &'static str {
        self.id()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineAvailability {
    pub engine: EngineKind,
    pub available: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TexLive {
    pub kind: EngineKind,
    pub latexmk: PathBuf,
    pub binary: PathBuf,
    pub path_dir: Option<PathBuf>,
    pub version: Option<String>,
}

impl TexLive {
    pub fn locate(kind: EngineKind, path_dir: Option<&str>, docker: bool) -> Option<Self> {
        if kind == EngineKind::Tectonic {
            return None;
        }
        let dir = path_dir.filter(|p| !p.is_empty()).map(PathBuf::from);
        let find = |name: &str| {
            if docker {
                Some(
                    dir.as_ref()
                        .map(|d| d.join(name))
                        .unwrap_or_else(|| PathBuf::from(name)),
                )
            } else {
                match &dir {
                    Some(d) => Some(d.join(name)).filter(|p| p.is_file()),
                    None => which(name),
                }
            }
        };
        let latexmk = find("latexmk")?;
        let binary = find(kind.executable())?;
        let path_dir = dir.or_else(|| {
            (!docker)
                .then(|| latexmk.parent().map(Path::to_path_buf))
                .flatten()
        });
        Some(Self {
            kind,
            latexmk,
            binary,
            path_dir,
            version: None,
        })
    }

    pub fn sandbox_mounts(&self, sandbox: &Sandbox) -> Vec<Mount> {
        if sandbox.kind != crate::SandboxKind::Bwrap {
            return Vec::new();
        }
        let Some(dir) = &self.path_dir else {
            return Vec::new();
        };
        // /usr, /bin, and the known system configuration/font trees are already mounted.
        // Query kpathsea for custom roots rather than exposing a broad parent such as /opt.
        let kpse = dir.join("kpsewhich");
        let query = |variable: &str| -> Option<String> {
            let output = std::process::Command::new(&kpse)
                .arg(format!("-var-value={variable}"))
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        dir.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        };
        let fallback = dir
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "bin"))
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.clone());
        let mut roots = vec![query("TEXMFROOT").map(PathBuf::from).unwrap_or(fallback)];
        for variable in [
            "TEXMFDIST",
            "TEXMFLOCAL",
            "TEXMFSYSCONFIG",
            "TEXMFSYSVAR",
            "OSFONTDIR",
        ] {
            if let Some(value) = query(variable) {
                for part in value.split([',', ';', ':']) {
                    let clean = part
                        .trim()
                        .trim_matches(['{', '}', '!'])
                        .trim_end_matches("//");
                    roots.push(PathBuf::from(clean));
                }
            }
        }
        roots.sort_by_key(|p| p.components().count());
        let mut kept: Vec<PathBuf> = Vec::new();
        for root in roots {
            if !root.is_absolute() || !root.is_dir() || root.components().count() < 3 {
                continue;
            }
            if [
                "/usr",
                "/bin",
                "/sbin",
                "/etc/texmf",
                "/etc/fonts",
                "/var/lib/texmf",
                "/var/cache/fontconfig",
            ]
            .iter()
            .any(|base| root.starts_with(base))
            {
                continue;
            }
            if kept.iter().any(|old| root.starts_with(old)) {
                continue;
            }
            kept.push(root);
        }
        kept.into_iter()
            .map(|root| Mount {
                host: root.clone(),
                guest: root,
                writable: false,
            })
            .collect()
    }

    pub fn compile(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        main_file: &str,
        draft: bool,
    ) -> Result<CompileSpec> {
        let root = project_dir.join(".galley/build");
        let out_dir = root.join("engines").join(self.kind.id());
        std::fs::create_dir_all(&out_dir)?;
        let build_alias = self.build_alias(sandbox, project_dir);
        let map = sandbox.mounts(project_dir, &build_alias);
        let (input, stem) = if draft {
            let class = documentclass(
                &std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default(),
            )
            .unwrap_or_else(|| "article".into());
            let wrapper = format!(
                "\\PassOptionsToClass{{draft}}{{{class}}}\n\\input{{{}}}\n",
                main_file.strip_suffix(".tex").unwrap_or(main_file)
            );
            std::fs::write(root.join("galley-draft.tex"), wrapper)?;
            (root.join("galley-draft.tex"), "galley-draft".to_string())
        } else {
            let stem = Path::new(main_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::Engine(format!("{main_file} is not a valid main file")))?
                .to_string();
            (project_dir.join(main_file), stem)
        };
        self.build_spec(sandbox, project_dir, &out_dir, &map, &input, stem)
    }

    pub fn compile_generated(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        build_file: &str,
        stem: &str,
    ) -> Result<CompileSpec> {
        let root = project_dir.join(".galley/build");
        // TikZ's external library names the figure PDFs relative to the wrapper in the shared
        // build directory. Keep these jobs there; ordinary and comparison jobs stay isolated.
        let out_dir = if stem == "galley-fig" || stem.starts_with("galley-fig-figure") {
            root.clone()
        } else {
            root.join("engines").join(self.kind.id())
        };
        std::fs::create_dir_all(&out_dir)?;
        let build_alias = self.build_alias(sandbox, project_dir);
        let map = sandbox.mounts(project_dir, &build_alias);
        self.build_spec(
            sandbox,
            project_dir,
            &out_dir,
            &map,
            &root.join(build_file),
            stem.to_string(),
        )
    }

    fn build_spec(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        out_dir: &Path,
        map: &Mounts,
        input: &Path,
        stem: String,
    ) -> Result<CompileSpec> {
        let mode = match self.kind {
            EngineKind::PdfLatex => "-pdf",
            EngineKind::XeLatex => "-xelatex",
            EngineKind::LuaLatex => "-lualatex",
            EngineKind::Latex => "-pdfps",
            EngineKind::Tectonic => unreachable!(),
        };
        let guest_out = map.guest(out_dir);
        let svg_root = project_dir.join(crate::svg::ROOT);
        let svg_figures = svg_root.join("svg-inkscape");
        let has_svg = self.kind != EngineKind::Latex && svg_root.is_dir();
        let mut args = vec![
            "-norc".into(),
            mode.into(),
            "-interaction=nonstopmode".into(),
            "-file-line-error".into(),
            "-synctex=1".into(),
            "-halt-on-error".into(),
            format!("-outdir={}", guest_out.display()),
            map.guest(input).to_string_lossy().into_owned(),
        ];
        // latexmk consults neither system nor project rc files. Force each TeX program's shell
        // escape off even when a local TeX configuration would otherwise enable it.
        let tex = self
            .binary
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(self.kind.id());
        args.insert(
            2,
            format!(
                "-{}={} -no-shell-escape %O {}",
                match self.kind {
                    EngineKind::PdfLatex => "pdflatex",
                    EngineKind::XeLatex => "xelatex",
                    EngineKind::LuaLatex => "lualatex",
                    EngineKind::Latex => "latex",
                    _ => unreachable!(),
                },
                tex,
                if has_svg { "%P" } else { "%S" }
            ),
        );
        if has_svg {
            // The svg package's default `./svg-inkscape/` probe bypasses TEXINPUTS. Latexmk's
            // %P executes this fixed preamble before the source while retaining the source's
            // original job name and our explicit no-shell-escape engine command.
            args.insert(
                3,
                "-pretex=\\PassOptionsToPackage{inkscapepath={.galley/svg/svg-inkscape/}}{svg}"
                    .into(),
            );
        }
        let build_root = project_dir.join(".galley/build");
        let guest_root = map.guest(&build_root);
        // Enumerate source directories instead of using `/work//`, whose recursive search can
        // load stale TeX files from the separate Tectonic cache under `.galley/build`.
        let source_dirs = project_source_dirs(project_dir)
            .into_iter()
            .map(|dir| map.guest(&dir).to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut tex_inputs = source_dirs.join(":");
        if stem == crate::figures::WRAPPER_STEM || stem.starts_with("galley-fig-figure") {
            tex_inputs.push(':');
            tex_inputs.push_str(&guest_root.to_string_lossy());
        }
        if has_svg {
            tex_inputs.push(':');
            tex_inputs.push_str(&map.guest(&svg_root).to_string_lossy());
            tex_inputs.push(':');
            tex_inputs.push_str(&map.guest(&svg_figures).to_string_lossy());
        }
        tex_inputs.push(':');
        let mut env = vec![
            ("HOME".into(), guest_out.to_string_lossy().into_owned()),
            (
                "TEXMFVAR".into(),
                guest_out.join("texmf-var").to_string_lossy().into_owned(),
            ),
            (
                "TEXMFCONFIG".into(),
                guest_out
                    .join("texmf-config")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("SOURCE_DATE_EPOCH".into(), "0".into()),
            ("TEXINPUTS".into(), tex_inputs),
            ("BIBINPUTS".into(), format!("{}:", source_dirs.join(":"))),
        ];
        if has_svg {
            env.push((
                "GINPUTS".into(),
                format!("{}:", map.guest(&svg_figures).display()),
            ));
        }
        if let Some(dir) = &self.path_dir {
            let old = if sandbox.kind == crate::SandboxKind::Docker {
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()
            } else {
                std::env::var("PATH").unwrap_or_default()
            };
            env.push(("PATH".into(), format!("{}:{old}", dir.display())));
        }
        let masked = if sandbox.kind == crate::SandboxKind::None {
            Vec::new()
        } else {
            [".git", ".galley/docs", ".galley/agents"]
                .iter()
                .filter(|r| project_dir.join(r).is_dir())
                .map(|r| PathBuf::from(format!("{GUEST_WORK}/{r}")))
                .collect()
        };
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "galley-build-{}-{}-{}",
            self.kind.id(),
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut mounts = self.sandbox_mounts(sandbox);
        mounts.extend(self.build_alias(sandbox, project_dir));
        Ok(CompileSpec {
            spec: Spec {
                project_dir: project_dir.to_path_buf(),
                out_dir: project_dir.join(".galley/build"),
                mounts,
                masked,
                env,
                network: false,
                program: self.latexmk.clone(),
                args,
                name,
            },
            output_dir: out_dir.to_path_buf(),
            stem,
        })
    }

    fn build_alias(&self, sandbox: &Sandbox, project_dir: &Path) -> Vec<Mount> {
        // Ubuntu's Ghostscript AppArmor profile can read/write PDF and PostScript in /tmp, but
        // denies the same files below /work. Use a second mount of the existing writable build
        // directory for all Bwrap TeX Live output, so latexmk's DVI -> PS -> PDF chain works
        // without weakening Ghostscript's confinement or broadening project write access.
        if sandbox.kind == crate::SandboxKind::Bwrap {
            vec![Mount {
                host: project_dir.join(".galley/build"),
                guest: PathBuf::from(GUEST_TEX_BUILD),
                writable: true,
            }]
        } else {
            Vec::new()
        }
    }
}

fn project_source_dirs(project_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![project_dir.to_path_buf()];
    let mut todo = vec![project_dir.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.')
                || ["node_modules", "target"].iter().any(|skip| name == *skip)
            {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let path = entry.path();
                dirs.push(path.clone());
                todo.push(path);
            }
        }
    }
    dirs.sort();
    dirs
}

#[derive(Debug, Clone)]
pub struct Tectonic {
    pub binary: PathBuf,
    pub cache_dir: PathBuf,
}

/// One compile invocation, ready for the sandbox.
pub struct CompileSpec {
    pub spec: Spec,
    pub output_dir: PathBuf,
    /// The stem of the output file inside `.galley/build`, such as `main` or `galley-draft`.
    pub stem: String,
}

impl Tectonic {
    pub(crate) fn local_cache_dir(project_dir: &Path) -> PathBuf {
        project_dir.join(".galley/build/tectonic-cache")
    }

    /// Seed each project's writable cache from the user's existing cache. Tectonic may update
    /// its bundle index even during an offline compile, so the shared cache must never be mounted
    /// writable into a build sandbox.
    fn prepare_cache(&self, project_dir: &Path) -> Result<PathBuf> {
        let local = Self::local_cache_dir(project_dir);
        if local.is_dir() {
            return Ok(local);
        }
        std::fs::create_dir_all(&local)?;
        if self.cache_dir.is_dir() {
            copy_cache_tree(&self.cache_dir, &local)?;
        }
        Ok(local)
    }
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
    /// the engine can pull missing bundle files into the project cache. runner.rs says when.
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
        self.prepare_cache(project_dir)?;
        let mounts = vec![Mount {
            host: self.binary.clone(),
            guest: PathBuf::from(GUEST_BIN),
            writable: false,
        }];
        let map: Mounts = sandbox.mounts(project_dir, &mounts);

        let (input, stem) = if draft {
            let class = documentclass(
                &std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default(),
            )
            .unwrap_or_else(|| "article".to_string());
            let main_no_ext = main_file.strip_suffix(".tex").unwrap_or(main_file);
            let wrapper =
                format!("\\PassOptionsToClass{{draft}}{{{class}}}\n\\input{{{main_no_ext}}}\n");
            std::fs::write(out_dir.join("galley-draft.tex"), wrapper)?;
            (
                map.guest(&out_dir.join("galley-draft.tex"))
                    .to_string_lossy()
                    .into_owned(),
                "galley-draft".to_string(),
            )
        } else {
            let stem = Path::new(main_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::Engine(format!("{main_file} is not a valid main file")))?
                .to_string();
            (
                map.guest(&project_dir.join(main_file))
                    .to_string_lossy()
                    .into_owned(),
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
        self.prepare_cache(project_dir)?;
        let mounts = vec![Mount {
            host: self.binary.clone(),
            guest: PathBuf::from(GUEST_BIN),
            writable: false,
        }];
        let map: Mounts = sandbox.mounts(project_dir, &mounts);
        let input = map
            .guest(&out_dir.join(build_file))
            .to_string_lossy()
            .into_owned();
        self.build_spec(
            sandbox,
            project_dir,
            &out_dir,
            &map,
            input,
            stem.to_string(),
            fetch,
        )
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
        let mounts = vec![Mount {
            host: self.binary.clone(),
            guest: PathBuf::from(GUEST_BIN),
            writable: false,
        }];

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
            args.extend([
                "-Z".to_string(),
                format!("search-path={}", map.guest(&svg_root).display()),
            ]);
        }
        if !fetch {
            args.push("--only-cached".into());
        }
        args.extend([
            "-o".to_string(),
            guest_out.to_string_lossy().into_owned(),
            input,
        ]);

        let guest_cache = map.guest(&Self::local_cache_dir(project_dir));
        let env = vec![
            (
                "TECTONIC_CACHE_DIR".to_string(),
                guest_cache.to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".to_string(),
                guest_cache.to_string_lossy().into_owned(),
            ),
            ("HOME".to_string(), "/tmp".to_string()),
            ("SOURCE_DATE_EPOCH".to_string(), "0".to_string()),
        ];
        let program = map.guest(&self.binary);
        // The name is unique for each run. `docker run --rm` removes the previous container
        // asynchronously, so the same name makes a quick rebuild fail with "name already in use".
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "galley-build-{}-{}-{}",
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("p"),
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
            output_dir: out_dir.to_path_buf(),
            stem,
        })
    }
}

fn copy_cache_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_cache_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
            let mut permissions = std::fs::metadata(&target)?.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(false);
            std::fs::set_permissions(&target, permissions)?;
        }
    }
    Ok(())
}

/// The class named in `\documentclass[...]{class}`.
pub fn documentclass(main: &str) -> Option<String> {
    let re = regex::Regex::new(r"\\documentclass\s*(?:\[[^\]]*\])?\s*\{([^}]+)\}").ok()?;
    re.captures(main).map(|c| c[1].trim().to_string())
}

pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().map(|dir| dir.join(&p)).unwrap_or(p)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_documentclass() {
        assert_eq!(
            documentclass("\\documentclass[11pt]{article}").as_deref(),
            Some("article")
        );
        assert_eq!(
            documentclass("% x\n\\documentclass{ neurips_2026 }").as_deref(),
            Some("neurips_2026")
        );
        assert_eq!(documentclass("hello"), None);
    }

    #[test]
    fn engine_ids_are_stable_in_json() {
        for engine in EngineKind::ALL {
            let json = serde_json::to_string(&engine).unwrap();
            assert_eq!(json, format!("\"{}\"", engine.id()));
            assert_eq!(serde_json::from_str::<EngineKind>(&json).unwrap(), engine);
        }
        assert_eq!(EngineKind::default(), EngineKind::Tectonic);
        assert!(serde_json::from_str::<EngineKind>("\"texlive\"").is_err());
    }

    #[test]
    fn texlive_specs_isolate_outputs_and_never_mount_the_host_tree_into_docker() {
        let tree = tempfile::tempdir().unwrap();
        let bin = tree.path().join("bin/x86_64-linux");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["latexmk", "pdflatex"] {
            std::fs::write(bin.join(name), "").unwrap();
        }
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.tex"), "\\documentclass{article}").unwrap();
        std::fs::create_dir_all(project.path().join(crate::svg::ROOT)).unwrap();
        std::fs::create_dir_all(project.path().join("chapters")).unwrap();
        let bwrap = Sandbox {
            kind: crate::SandboxKind::Bwrap,
            docker_image: String::new(),
            memory_mb: 512,
            cpus: 1.0,
            systemd_scope: false,
        };
        let engine = TexLive::locate(EngineKind::PdfLatex, bin.to_str(), false).unwrap();
        let spec = engine
            .compile(&bwrap, project.path(), "main.tex", false)
            .unwrap();
        assert!(!spec.spec.network);
        assert_eq!(spec.spec.out_dir, project.path().join(".galley/build"));
        assert_eq!(
            spec.output_dir,
            project.path().join(".galley/build/engines/pdflatex")
        );
        assert!(spec.spec.args.iter().any(|a| a == "-norc"));
        assert!(
            spec.spec
                .args
                .iter()
                .any(|a| a.contains("pdflatex -no-shell-escape"))
        );
        assert_eq!(spec.spec.mounts.len(), 2);
        assert_eq!(spec.spec.mounts[0].host, tree.path());
        assert!(!spec.spec.mounts[0].writable);
        assert_eq!(
            spec.spec.mounts[1].host,
            project.path().join(".galley/build")
        );
        assert_eq!(spec.spec.mounts[1].guest, PathBuf::from(GUEST_TEX_BUILD));
        assert!(spec.spec.mounts[1].writable);
        assert!(spec.spec.args.iter().any(|a| a.contains(GUEST_TEX_BUILD)));
        assert!(spec.spec.args.iter().any(|a| a.contains("%O %P")));
        assert!(spec.spec.args.iter().any(|a| a.contains(
            "-pretex=\\PassOptionsToPackage{inkscapepath={.galley/svg/svg-inkscape/}}{svg}"
        )));
        assert!(spec.spec.env.iter().any(|(k, v)| {
            k == "TEXINPUTS" && v.contains("/work/.galley/svg:") && !v.contains("//")
        }));
        assert!(spec.spec.env.iter().any(|(k, v)| {
            k == "TEXINPUTS" && v.contains("/work/chapters:") && !v.contains("tectonic-cache")
        }));
        assert!(
            spec.spec
                .env
                .iter()
                .any(|(k, v)| { k == "TEXINPUTS" && !v.contains(GUEST_TEX_BUILD) })
        );
        assert!(
            spec.spec.env.iter().any(|(k, v)| {
                k == "TEXINPUTS" && v.contains("/work/.galley/svg/svg-inkscape:")
            })
        );
        assert!(
            spec.spec
                .env
                .iter()
                .any(|(k, v)| { k == "GINPUTS" && v == "/work/.galley/svg/svg-inkscape:" })
        );
        let draft = engine
            .compile(&bwrap, project.path(), "main.tex", true)
            .unwrap();
        assert!(
            draft
                .spec
                .env
                .iter()
                .any(|(k, v)| { k == "TEXINPUTS" && !v.contains(GUEST_TEX_BUILD) })
        );
        let figure = engine
            .compile_generated(&bwrap, project.path(), "galley-fig.tex", "galley-fig")
            .unwrap();
        assert!(
            figure
                .spec
                .env
                .iter()
                .any(|(k, v)| { k == "TEXINPUTS" && v.contains(GUEST_TEX_BUILD) })
        );

        let docker = Sandbox {
            kind: crate::SandboxKind::Docker,
            ..bwrap
        };
        let image_bin = "/image/texlive/bin";
        let in_image = TexLive::locate(EngineKind::PdfLatex, Some(image_bin), true).unwrap();
        let spec = in_image
            .compile(&docker, project.path(), "main.tex", false)
            .unwrap();
        assert!(spec.spec.mounts.is_empty());
        assert_eq!(
            spec.spec.program,
            PathBuf::from("/image/texlive/bin/latexmk")
        );
        assert!(
            spec.spec
                .env
                .iter()
                .any(|(k, v)| k == "PATH" && v.starts_with(image_bin))
        );
    }

    #[test]
    fn tectonic_seeds_only_a_project_local_writable_cache() {
        let tree = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let shared = tree.path().join("cache");
        std::fs::create_dir_all(shared.join("tectonic/bundles")).unwrap();
        std::fs::write(shared.join("tectonic/bundles/index"), "cached").unwrap();
        let mut permissions = std::fs::metadata(shared.join("tectonic/bundles/index"))
            .unwrap()
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(shared.join("tectonic/bundles/index"), permissions).unwrap();
        let binary = tree.path().join("tectonic");
        std::fs::write(&binary, "").unwrap();
        std::fs::write(project.path().join("main.tex"), "\\documentclass{article}").unwrap();
        let engine = Tectonic {
            binary,
            cache_dir: shared.clone(),
        };
        let sandbox = Sandbox {
            kind: crate::SandboxKind::Bwrap,
            docker_image: String::new(),
            memory_mb: 512,
            cpus: 1.0,
            systemd_scope: false,
        };
        let spec = engine
            .compile(&sandbox, project.path(), "main.tex", false, false, 30)
            .unwrap();
        let local = Tectonic::local_cache_dir(project.path());
        assert_eq!(
            std::fs::read_to_string(local.join("tectonic/bundles/index")).unwrap(),
            "cached"
        );
        assert!(
            !std::fs::metadata(local.join("tectonic/bundles/index"))
                .unwrap()
                .permissions()
                .readonly()
        );
        assert_eq!(spec.spec.mounts.len(), 1);
        assert_eq!(spec.spec.mounts[0].host, engine.binary);
        for key in ["TECTONIC_CACHE_DIR", "XDG_CACHE_HOME"] {
            assert!(
                spec.spec
                    .env
                    .iter()
                    .any(|(k, v)| { k == key && v == "/work/.galley/build/tectonic-cache" })
            );
        }
        assert_eq!(
            std::fs::read_to_string(shared.join("tectonic/bundles/index")).unwrap(),
            "cached"
        );
    }
}
