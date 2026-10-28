//! An engine turns a project into a PDF. Tectonic is the default; TeX Live engines use `latexmk`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::sandbox::{GUEST_WORK, Mount, Mounts, Sandbox, Spec};
use crate::zones::{self, Cleanup, GUEST_BUNDLE, GUEST_CACHE, GUEST_FIG, GUEST_IN, GUEST_OUT, Leaf};
use crate::{Error, Result};

const GUEST_BIN: &str = "/galley/bin/tectonic";

/// The figure wrapper and the figure jobs write into the figure cache and read each other's files.
fn is_figure_job(stem: &str) -> bool {
    stem == crate::figures::WRAPPER_STEM || stem.starts_with("galley-fig-figure")
}

/// The folder a generated job writes: the figure cache for figure jobs, a fresh one otherwise.
fn leaf_for(project_dir: &Path, stem: &str) -> Result<(Leaf, &'static str)> {
    Ok(if is_figure_job(stem) {
        (Leaf::fig(project_dir)?, GUEST_FIG)
    } else {
        (Leaf::run(project_dir)?, GUEST_OUT)
    })
}

/// What every compile of a project mounts besides the project: Galley's inputs read-only, the
/// converted SVG figures read-only (the rest of `.galley` is hidden), and the folder it writes.
fn common_mounts(project_dir: &Path, leaf: &Leaf, guest_out: &str) -> Vec<Mount> {
    let mut mounts = vec![Mount {
        host: zones::inputs_dir(project_dir),
        guest: PathBuf::from(GUEST_IN),
        writable: false,
    }];
    let svg = project_dir.join(crate::svg::ROOT);
    if svg.is_dir() {
        mounts.push(Mount {
            guest: PathBuf::from(GUEST_WORK).join(crate::svg::ROOT),
            host: svg,
            writable: false,
        });
    }
    mounts.push(Mount {
        host: leaf.path().to_path_buf(),
        guest: PathBuf::from(guest_out),
        writable: true,
    });
    mounts
}

/// Hidden behind an empty tmpfs: the git internals, and Galley's own folder with every scratch
/// folder and the published PDFs. A tmpfs needs a mount point that exists under the read-only
/// project, so only folders that are there are masked.
fn masked(sandbox: &Sandbox, project_dir: &Path) -> Vec<PathBuf> {
    if sandbox.kind == crate::SandboxKind::None {
        return Vec::new();
    }
    [".git", ".galley"]
        .iter()
        .filter(|rel| project_dir.join(rel).is_dir())
        .map(|rel| PathBuf::from(format!("{GUEST_WORK}/{rel}")))
        .collect()
}

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
        let leaf = Leaf::run(project_dir)?;
        let (input, stem) = if draft {
            let class = documentclass(
                &std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default(),
            )
            .unwrap_or_else(|| "article".into());
            let wrapper = format!(
                "\\PassOptionsToClass{{draft}}{{{class}}}\n\\input{{{}}}\n",
                main_file.strip_suffix(".tex").unwrap_or(main_file)
            );
            (
                zones::write_input(project_dir, "galley-draft.tex", wrapper)?,
                "galley-draft".to_string(),
            )
        } else {
            let stem = Path::new(main_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::Engine(format!("{main_file} is not a valid main file")))?
                .to_string();
            (project_dir.join(main_file), stem)
        };
        self.build_spec(sandbox, project_dir, leaf, GUEST_OUT, &input, stem)
    }

    /// Compile a file Galley wrote into `.galley/in`, such as a figure job or the latexdiff output.
    pub fn compile_generated(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        build_file: &str,
        stem: &str,
    ) -> Result<CompileSpec> {
        let (leaf, guest_out) = leaf_for(project_dir, stem)?;
        self.build_spec(
            sandbox,
            project_dir,
            leaf,
            guest_out,
            &zones::inputs_dir(project_dir).join(build_file),
            stem.to_string(),
        )
    }

    fn build_spec(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        leaf: Leaf,
        guest_out: &str,
        input: &Path,
        stem: String,
    ) -> Result<CompileSpec> {
        let mut mounts = self.sandbox_mounts(sandbox);
        mounts.extend(common_mounts(project_dir, &leaf, guest_out));
        let map = sandbox.mounts(project_dir, &mounts);
        let mode = match self.kind {
            EngineKind::PdfLatex => "-pdf",
            EngineKind::XeLatex => "-xelatex",
            EngineKind::LuaLatex => "-lualatex",
            EngineKind::Latex => "-pdfps",
            EngineKind::Tectonic => unreachable!(),
        };
        let guest_out = map.guest(leaf.path());
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
        // LuaTeX also carries a socket library. A compile has no network, and no document needs it.
        let no_socket = if self.kind == EngineKind::LuaLatex { " -nosocket" } else { "" };
        args.insert(
            2,
            format!(
                "-{}={tex}{no_socket} -no-shell-escape %O {}",
                match self.kind {
                    EngineKind::PdfLatex => "pdflatex",
                    EngineKind::XeLatex => "xelatex",
                    EngineKind::LuaLatex => "lualatex",
                    EngineKind::Latex => "latex",
                    _ => unreachable!(),
                },
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
        // Enumerate source directories instead of using `/work//`, which would also search the
        // hidden `.galley` folder.
        let source_dirs = project_source_dirs(project_dir)
            .into_iter()
            .map(|dir| map.guest(&dir).to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut tex_inputs = source_dirs.join(":");
        if is_figure_job(&stem) {
            // The figure jobs read the wrapper from Galley's inputs, and the main pass reads the
            // figure PDFs from the figure cache it writes.
            tex_inputs.push(':');
            tex_inputs.push_str(&map.guest(&zones::inputs_dir(project_dir)).to_string_lossy());
            tex_inputs.push(':');
            tex_inputs.push_str(&guest_out.to_string_lossy());
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
        let masked = masked(sandbox, project_dir);
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "galley-build-{}-{}-{}",
            self.kind.id(),
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        Ok(CompileSpec {
            spec: Spec {
                project_dir: project_dir.to_path_buf(),
                mounts,
                masked,
                env,
                network: false,
                program: self.latexmk.clone(),
                args,
                name,
            },
            leaf,
            stem,
            inputs: Vec::new(),
        })
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
    /// The engine folder, `engine-<KEY>`: the binary, the bundle and the formats built for them.
    pub root: PathBuf,
}

/// One compile invocation, ready for the sandbox.
pub struct CompileSpec {
    pub spec: Spec,
    /// The folder the compile writes and its outputs are read back from.
    pub leaf: Leaf,
    /// The stem of the output files, such as `main` or `galley-draft`.
    pub stem: String,
    /// Inputs that exist only for this run, deleted once it is over.
    pub inputs: Vec<Cleanup>,
}

/// Where the image puts the engine. A self-hosted install puts it in `<data>/tectonic`.
pub const IMAGE_ENGINES: &str = "/opt/galley/tectonic";

impl Tectonic {
    pub fn binary(&self) -> PathBuf {
        self.root.join("tectonic")
    }

    pub fn bundle(&self) -> PathBuf {
        self.root.join("bundle")
    }

    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// The engine key (`<bundle>-<binary>`), from the folder name.
    pub fn key(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().trim_start_matches("engine-").to_string())
            .unwrap_or_default()
    }

    /// Whether a cache holds a finished format file. Tectonic writes a format to a temporary
    /// `format_*` file in `formats/` and renames it to `*.fmt`, so a run stopped mid-write leaves
    /// only the temporary file, which does not count.
    pub fn has_formats(cache: &Path) -> bool {
        std::fs::read_dir(cache.join("formats")).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                e.file_name().to_string_lossy().ends_with(".fmt")
                    && std::fs::symlink_metadata(e.path()).is_ok_and(|m| m.is_file() && m.len() > 0)
            })
        })
    }

    /// The mounts of a compile: the binary, the bundle and the formats, all read-only. A compile
    /// can never change what the next one reads.
    fn mounts(&self, project_dir: &Path, leaf: &Leaf, guest_out: &str) -> Vec<Mount> {
        let mut mounts = vec![Mount {
            host: self.binary(),
            guest: PathBuf::from(GUEST_BIN),
            writable: false,
        }];
        mounts.extend(common_mounts(project_dir, leaf, guest_out));
        mounts.push(Mount {
            host: self.bundle(),
            guest: PathBuf::from(GUEST_BUNDLE),
            writable: false,
        });
        mounts.push(Mount {
            host: self.cache(),
            guest: PathBuf::from(GUEST_CACHE),
            writable: false,
        });
        mounts
    }

    /// Find the pinned engine for this machine: the image's, then the one `galley engine
    /// install` put in `<data_dir>/tectonic`. Only a complete `engine-<KEY>` folder counts.
    pub fn locate(data_dir: &Path) -> Option<Tectonic> {
        let pins = crate::install::pins().ok()?;
        [PathBuf::from(IMAGE_ENGINES), Self::install_dir(data_dir)]
            .into_iter()
            .map(|dir| dir.join(pins.folder()))
            .find(|root| crate::install::is_complete(root, &pins))
            .map(|root| Tectonic { root })
    }

    pub fn install_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("tectonic")
    }

    /// Build the sandbox spec that compiles `main_file`. It always runs offline, from the
    /// engine's own bundle.
    pub fn compile(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        main_file: &str,
        draft: bool,
        _timeout_s: u64,
    ) -> Result<CompileSpec> {
        let leaf = Leaf::run(project_dir)?;
        let (input, stem) = if draft {
            let class = documentclass(
                &std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default(),
            )
            .unwrap_or_else(|| "article".to_string());
            let main_no_ext = main_file.strip_suffix(".tex").unwrap_or(main_file);
            let wrapper =
                format!("\\PassOptionsToClass{{draft}}{{{class}}}\n\\input{{{main_no_ext}}}\n");
            (
                zones::write_input(project_dir, "galley-draft.tex", wrapper)?,
                "galley-draft".to_string(),
            )
        } else {
            let stem = Path::new(main_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| Error::Engine(format!("{main_file} is not a valid main file")))?
                .to_string();
            (project_dir.join(main_file), stem)
        };
        self.build_spec(sandbox, project_dir, leaf, GUEST_OUT, &input, stem)
    }

    /// Compile a file Galley wrote into `.galley/in`, such as a figure job or the latexdiff output.
    /// Its `search-path` still points at the project, so `\input` and the figures resolve.
    pub fn compile_generated(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        build_file: &str,
        stem: &str,
    ) -> Result<CompileSpec> {
        let (leaf, guest_out) = leaf_for(project_dir, stem)?;
        self.build_spec(
            sandbox,
            project_dir,
            leaf,
            guest_out,
            &zones::inputs_dir(project_dir).join(build_file),
            stem.to_string(),
        )
    }

    /// Assemble the sandboxed Tectonic command for a resolved input and stem.
    fn build_spec(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        leaf: Leaf,
        guest_out: &str,
        input: &Path,
        stem: String,
    ) -> Result<CompileSpec> {
        let mounts = self.mounts(project_dir, &leaf, guest_out);
        let map: Mounts = sandbox.mounts(project_dir, &mounts);
        let guest_out = map.guest(leaf.path());
        let guest_work = map.guest(project_dir);

        // Shell escape with \write18 is opt-in in Tectonic. Galley never passes `-Z shell-escape`,
        // so shell escape stays off. The sandbox is the real boundary: it has no network and a
        // read-only project (spec §6.2). `search-path` lets a wrapper in `.galley/in` resolve
        // \input{main} and the figures. Galley does not pass `--untrusted`, because that flag
        // disables search-path.
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
        if is_figure_job(&stem) {
            // The figure jobs read the wrapper from Galley's inputs, and the main pass reads the
            // figure PDFs from the figure cache it writes.
            for dir in [map.guest(&zones::inputs_dir(project_dir)), guest_out.clone()] {
                args.extend(["-Z".to_string(), format!("search-path={}", dir.display())]);
            }
        }
        // Every package comes from the local bundle; nothing is ever downloaded.
        args.extend([
            "-b".to_string(),
            map.guest(&self.bundle()).to_string_lossy().into_owned(),
            "--only-cached".to_string(),
            "-o".to_string(),
            guest_out.to_string_lossy().into_owned(),
            map.guest(input).to_string_lossy().into_owned(),
        ]);

        // The formats are read from the engine's cache, which is read-only. Anything else Tectonic
        // might want to keep goes to the run's own folder: a private tmpfs in bwrap and docker, the
        // run's scratch folder without a sandbox (never the host's shared /tmp).
        let private = if sandbox.kind == crate::SandboxKind::None {
            guest_out.to_string_lossy().into_owned()
        } else {
            "/tmp".to_string()
        };
        let env = vec![
            (
                "TECTONIC_CACHE_DIR".to_string(),
                map.guest(&self.cache()).to_string_lossy().into_owned(),
            ),
            ("XDG_CACHE_HOME".to_string(), private.clone()),
            ("HOME".to_string(), private),
            ("SOURCE_DATE_EPOCH".to_string(), "0".to_string()),
        ];
        let program = map.guest(&self.binary());
        // The name is unique for each run. `docker run --rm` removes the previous container
        // asynchronously, so the same name makes a quick rebuild fail with "name already in use".
        let name = zones::unique(&format!(
            "galley-build-{}",
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("p")
        ));
        Ok(CompileSpec {
            spec: Spec {
                project_dir: project_dir.to_path_buf(),
                mounts,
                masked: masked(sandbox, project_dir),
                env,
                network: false,
                program,
                args,
                name,
            },
            leaf,
            stem,
            inputs: Vec::new(),
        })
    }
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
        assert!(crate::zones::is_scratch(spec.leaf.path()));
        assert!(spec.spec.args.iter().any(|a| a == "-norc"));
        assert!(
            spec.spec
                .args
                .iter()
                .any(|a| a.contains("pdflatex -no-shell-escape"))
        );
        // The TeX tree, Galley's inputs, the SVG folder, and the run's own folder. Only the last
        // is writable, and nothing is `.galley/build`.
        let writable: Vec<_> = spec.spec.mounts.iter().filter(|m| m.writable).collect();
        assert_eq!(writable.len(), 1);
        assert_eq!(writable[0].host, spec.leaf.path());
        assert_eq!(writable[0].guest, PathBuf::from(GUEST_OUT));
        assert_eq!(spec.spec.mounts[0].host, tree.path());
        assert!(spec.spec.mounts.iter().all(|m| !m.host.ends_with(".galley/build")));
        assert!(spec.spec.mounts.iter().any(|m| m.guest == Path::new("/work/.galley/svg") && !m.writable));
        assert!(spec.spec.masked.contains(&PathBuf::from("/work/.galley")));
        assert!(spec.spec.check_mounts().is_ok());
        assert!(spec.spec.args.iter().any(|a| a == &format!("-outdir={GUEST_OUT}")));
        assert!(spec.spec.args.iter().any(|a| a.contains("%O %P")));
        assert!(spec.spec.args.iter().any(|a| a.contains(
            "-pretex=\\PassOptionsToPackage{inkscapepath={.galley/svg/svg-inkscape/}}{svg}"
        )));
        let tex_inputs = |spec: &CompileSpec| {
            spec.spec.env.iter().find(|(k, _)| k == "TEXINPUTS").map(|(_, v)| v.clone()).unwrap()
        };
        let main = tex_inputs(&spec);
        assert!(main.contains("/work/.galley/svg:") && !main.contains("//"), "{main}");
        assert!(main.contains("/work/chapters:") && !main.contains("tectonic-cache"), "{main}");
        assert!(main.contains("/work/.galley/svg/svg-inkscape:"), "{main}");
        assert!(!main.contains(GUEST_IN) && !main.contains(GUEST_FIG), "{main}");
        assert!(
            spec.spec
                .env
                .iter()
                .any(|(k, v)| { k == "GINPUTS" && v == "/work/.galley/svg/svg-inkscape:" })
        );
        let draft = engine
            .compile(&bwrap, project.path(), "main.tex", true)
            .unwrap();
        assert!(!tex_inputs(&draft).contains(GUEST_FIG));
        assert!(draft.spec.args.iter().any(|a| a == &format!("{GUEST_IN}/galley-draft.tex")));
        assert!(crate::zones::inputs_dir(project.path()).join("galley-draft.tex").is_file());
        let figure = engine
            .compile_generated(&bwrap, project.path(), "galley-fig.tex", "galley-fig")
            .unwrap();
        let fig = tex_inputs(&figure);
        assert!(fig.contains(GUEST_IN) && fig.contains(GUEST_FIG), "{fig}");
        assert_eq!(figure.leaf.path(), crate::zones::fig_dir(project.path()));
        assert!(figure.spec.args.iter().any(|a| a == &format!("-outdir={GUEST_FIG}")));
        // Two runs never share a folder, so one cannot read or spoil the other's output.
        let again = engine.compile(&bwrap, project.path(), "main.tex", false).unwrap();
        assert_ne!(again.leaf.path(), spec.leaf.path());

        let docker = Sandbox {
            kind: crate::SandboxKind::Docker,
            ..bwrap
        };
        let image_bin = "/image/texlive/bin";
        let in_image = TexLive::locate(EngineKind::PdfLatex, Some(image_bin), true).unwrap();
        let spec = in_image
            .compile(&docker, project.path(), "main.tex", false)
            .unwrap();
        // Docker gets no host TeX tree, only Galley's inputs, the SVG folder and the run folder.
        assert!(spec.spec.mounts.iter().all(|m| m.host.starts_with(project.path())));
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

    /// An engine folder with an empty binary, for specs that are only inspected.
    fn fake_engine(tree: &Path) -> Tectonic {
        let root = tree.join("engine-0000000000000000-0000000000000000");
        std::fs::create_dir_all(root.join("bundle")).unwrap();
        std::fs::create_dir_all(root.join("cache/formats")).unwrap();
        std::fs::write(root.join("tectonic"), "").unwrap();
        Tectonic { root }
    }

    fn bwrap() -> Sandbox {
        Sandbox {
            kind: crate::SandboxKind::Bwrap,
            docker_image: String::new(),
            memory_mb: 512,
            cpus: 1.0,
            systemd_scope: false,
        }
    }

    #[test]
    fn every_tectonic_run_reads_the_engine_read_only_and_offline() {
        let tree = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.tex"), "\\documentclass{article}").unwrap();
        let engine = fake_engine(tree.path());
        let sandbox = bwrap();
        for spec in [
            engine.compile(&sandbox, project.path(), "main.tex", false, 30).unwrap(),
            engine.compile(&sandbox, project.path(), "main.tex", true, 30).unwrap(),
            engine.compile_generated(&sandbox, project.path(), "galley-diff.tex", "galley-diff").unwrap(),
            engine.compile_generated(&sandbox, project.path(), "galley-fig.tex", "galley-fig").unwrap(),
        ] {
            assert!(!spec.spec.network);
            let args = &spec.spec.args;
            assert!(args.iter().any(|a| a == "--only-cached"));
            let b = args.iter().position(|a| a == "-b").expect("-b");
            assert_eq!(args[b + 1], GUEST_BUNDLE);
            assert_eq!(spec.spec.mounts[0].host, engine.binary());
            let writable: Vec<_> = spec.spec.mounts.iter().filter(|m| m.writable).map(|m| m.host.clone()).collect();
            assert_eq!(writable, vec![spec.leaf.path().to_path_buf()]);
            for (host, guest) in [(engine.bundle(), GUEST_BUNDLE), (engine.cache(), GUEST_CACHE)] {
                assert!(spec.spec.mounts.iter().any(|m| m.host == host && m.guest == Path::new(guest) && !m.writable));
            }
            assert!(spec.spec.env.iter().any(|(k, v)| k == "TECTONIC_CACHE_DIR" && v == GUEST_CACHE));
            assert!(spec.spec.env.iter().any(|(k, v)| k == "XDG_CACHE_HOME" && v == "/tmp"));
            assert!(spec.spec.check_mounts().is_ok());
        }
        // No per-project package cache appears.
        assert!(!project.path().join(".galley/scratch/tectonic-cache").exists());
    }

    #[test]
    fn without_a_sandbox_tectonic_keeps_nothing_in_the_shared_tmp() {
        let tree = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.tex"), "\\documentclass{article}").unwrap();
        let engine = fake_engine(tree.path());
        let sandbox = Sandbox { kind: crate::SandboxKind::None, ..bwrap() };
        let spec = engine.compile(&sandbox, project.path(), "main.tex", false, 30).unwrap();
        let leaf = spec.leaf.path().to_string_lossy().into_owned();
        for key in ["XDG_CACHE_HOME", "HOME"] {
            assert!(spec.spec.env.iter().any(|(k, v)| k == key && *v == leaf), "{key}");
        }
        assert!(spec.spec.args.iter().any(|a| *a == engine.bundle().to_string_lossy()));
    }

    #[test]
    fn locate_takes_only_a_complete_pinned_engine() {
        let data = tempfile::tempdir().unwrap();
        let dir = Tectonic::install_dir(data.path());
        std::fs::create_dir_all(&dir).unwrap();
        // A lone binary from the old on-demand download is not an engine.
        std::fs::write(dir.join("tectonic"), "").unwrap();
        assert!(Tectonic::locate(data.path()).is_none() || Path::new(IMAGE_ENGINES).is_dir());
        let Ok(pins) = crate::install::pins() else { return };
        let root = dir.join(pins.folder());
        std::fs::create_dir_all(root.join("bundle")).unwrap();
        std::fs::create_dir_all(root.join("cache/formats")).unwrap();
        std::fs::write(root.join("tectonic"), "").unwrap();
        std::fs::write(root.join("bundle/SHA256SUM"), "another bundle").unwrap();
        std::fs::write(root.join("cache/formats/latex.fmt"), "fmt").unwrap();
        if !Path::new(IMAGE_ENGINES).is_dir() {
            assert!(Tectonic::locate(data.path()).is_none(), "the bundle digest differs");
        }
        std::fs::write(root.join("bundle/SHA256SUM"), &pins.bundle_digest).unwrap();
        let found = Tectonic::locate(data.path()).unwrap();
        assert_eq!(found.key(), pins.key);
    }

    #[test]
    fn tectonic_figure_jobs_find_the_wrapper_and_the_figures() {
        let tree = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let engine = fake_engine(tree.path());
        let sandbox = bwrap();
        let spec = engine.compile_generated(&sandbox, project.path(), "galley-fig.tex", "galley-fig").unwrap();
        for path in [GUEST_IN, GUEST_FIG] {
            assert!(spec.spec.args.iter().any(|a| a == &format!("search-path={path}")), "{:?}", spec.spec.args);
        }
        assert!(spec.spec.args.iter().any(|a| a == &format!("{GUEST_IN}/galley-fig.tex")));
        let plain = engine.compile(&sandbox, project.path(), "main.tex", false, 30).unwrap();
        assert!(!plain.spec.args.iter().any(|a| a.contains(GUEST_FIG)));
    }

    #[test]
    fn lualatex_has_no_socket_library() {
        let tree = tempfile::tempdir().unwrap();
        let bin = tree.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["latexmk", "lualatex"] {
            std::fs::write(bin.join(name), "").unwrap();
        }
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.tex"), "\\documentclass{article}").unwrap();
        let sandbox = Sandbox {
            kind: crate::SandboxKind::Bwrap,
            docker_image: String::new(),
            memory_mb: 512,
            cpus: 1.0,
            systemd_scope: false,
        };
        let engine = TexLive::locate(EngineKind::LuaLatex, bin.to_str(), false).unwrap();
        let spec = engine.compile(&sandbox, project.path(), "main.tex", false).unwrap();
        assert!(spec.spec.args.iter().any(|a| a.contains("lualatex -nosocket -no-shell-escape")), "{:?}", spec.spec.args);
    }
}
