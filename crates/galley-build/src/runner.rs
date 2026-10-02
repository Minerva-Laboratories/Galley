//! The build queue for one project. The newest request wins. Each job runs the flush hook, then a
//! sandboxed compile, then an optional package fetch pass, then the log parse, then the artifacts.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::engine::{EngineAvailability, EngineKind, Tectonic, TexLive};
use crate::figures::{self, FigCache, FigureStats};
use crate::hints::{Hints, ProjectView};
use crate::log::{Diagnostic, Level, RawDiag, parse_log};
use crate::sandbox::Sandbox;
use crate::synctex::SyncTex;
use crate::{Result, install};

#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildRequest {
    pub engine: EngineKind,
    pub draft: bool,
    /// The file to compile. The build uses the project's main file when this is None. This lets
    /// the editor build the document that the user reads.
    pub file: Option<String>,
    /// Lint rules switched off for this project (SPEC §13.5).
    pub lint_disabled: Vec<String>,
    /// Use the persistent figure cache when the project is eligible (SPEC §13.1).
    pub figure_cache: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    Ok,
    Failed,
    Timeout,
    /// The engine could not run at all (missing binary, sandbox failure).
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    pub total_ms: u64,
    pub compile_ms: u64,
    pub fetch_ms: u64,
    /// Time spent compiling figures on their own during this build.
    pub figure_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub id: u64,
    pub status: BuildStatus,
    pub errors: Vec<Diagnostic>,
    pub error_count: usize,
    pub warning_count: usize,
    pub profile: Profile,
    /// Pages in the PDF, from the engine's log. SPEC §13.8 uses it for the page limit.
    pub pages: Option<u32>,
    /// Figure cache outcome. It is absent for projects without TikZ pictures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figures: Option<FigureStats>,
    /// Figures to generate after this build, while nothing else is queued.
    #[serde(skip)]
    pub pending_figures: Vec<String>,
    pub fetched_packages: bool,
    /// A PDF from this build.
    pub pdf_fresh: bool,
    /// Some PDF exists (this build's, or the last good one).
    pub pdf_available: bool,
    /// A newer request was queued while this one ran.
    pub stale: bool,
    pub draft: bool,
    pub main_file: String,
    pub engine: String,
    #[serde(default)]
    pub engine_version: Option<String>,
    #[serde(default)]
    pub pdf_engine: Option<String>,
    #[serde(default)]
    pub pdf_engine_version: Option<String>,
    pub sandbox: String,
    pub finished_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildEvent {
    BuildStarted {
        id: u64,
        draft: bool,
        engine: EngineKind,
    },
    BuildProgress {
        id: u64,
        message: String,
    },
    BuildSuperseded {
        id: u64,
    },
    /// Boxed: a result carries every diagnostic, and events are cloned per subscriber.
    BuildFinished(Box<BuildResult>),
}

pub const LAST_GOOD_PDF: &str = "last-good.pdf";
pub const LAST_GOOD_SYNCTEX: &str = "last-good.synctex.gz";
pub const ERRORS_JSON: &str = "errors.json";
pub const LAST_LOG: &str = "last.log";
pub const LAST_DIFF_PDF: &str = "last-diff.pdf";
pub const LAST_RESULT_JSON: &str = "last-result.json";
pub const PDF_PRODUCER_JSON: &str = "pdf-producer.json";

struct RunControl<'a> {
    latest: &'a (dyn Fn() -> bool + Send + Sync),
    publication_gate: Option<&'a tokio::sync::Mutex<()>>,
}

#[derive(Serialize, Deserialize)]
struct PdfProducer {
    engine: String,
    version: Option<String>,
}

fn read_producer(out_dir: &Path) -> Option<PdfProducer> {
    if !out_dir.join(LAST_GOOD_PDF).is_file() {
        return None;
    }
    std::fs::read(out_dir.join(PDF_PRODUCER_JSON))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn publish_pdf(
    out_dir: &Path,
    pdf: &Path,
    synctex: Option<&Path>,
    result: &BuildResult,
) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let published = out_dir.join("published");
    std::fs::create_dir_all(&published)?;
    let current = out_dir.join("current");
    if current.symlink_metadata().is_err() && out_dir.join(LAST_GOOD_PDF).is_file() {
        let legacy = published.join("legacy");
        std::fs::create_dir_all(&legacy)?;
        std::fs::copy(out_dir.join(LAST_GOOD_PDF), legacy.join("paper.pdf"))?;
        if out_dir.join(LAST_GOOD_SYNCTEX).is_file() {
            std::fs::copy(
                out_dir.join(LAST_GOOD_SYNCTEX),
                legacy.join("paper.synctex.gz"),
            )?;
        }
        if out_dir.join(PDF_PRODUCER_JSON).is_file() {
            std::fs::copy(
                out_dir.join(PDF_PRODUCER_JSON),
                legacy.join("producer.json"),
            )?;
        }
        let _ = std::fs::remove_file(&current);
        symlink("published/legacy", &current)?;
    }
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let generation = format!(
        "{}-{}-{}-{epoch}",
        result.id,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let target = published.join(&generation);
    std::fs::create_dir(&target)?;
    std::fs::copy(pdf, target.join("paper.pdf"))?;
    if let Some(st) = synctex {
        std::fs::copy(st, target.join("paper.synctex.gz"))?;
    }
    let producer = PdfProducer {
        engine: result.engine.clone(),
        version: result.engine_version.clone(),
    };
    std::fs::write(
        target.join("producer.json"),
        serde_json::to_vec_pretty(&producer)?,
    )?;

    // The public paths always follow one symlink. Once this layout exists, a single rename
    // switches the PDF, SyncTeX, and producer metadata together.
    for (alias, target) in [
        (LAST_GOOD_PDF, "current/paper.pdf"),
        (LAST_GOOD_SYNCTEX, "current/paper.synctex.gz"),
        (PDF_PRODUCER_JSON, "current/producer.json"),
    ] {
        let tmp = out_dir.join(format!(".{alias}.next"));
        let _ = std::fs::remove_file(&tmp);
        symlink(target, &tmp)?;
        std::fs::rename(tmp, out_dir.join(alias))?;
    }
    let next = out_dir.join(".current.next");
    let _ = std::fs::remove_file(&next);
    symlink(format!("published/{generation}"), &next)?;
    std::fs::rename(next, current)?;
    // Keep a few prior generations so a reader that followed the old pointer can finish.
    if let Ok(entries) = std::fs::read_dir(&published) {
        let mut dirs: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
        dirs.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
        for entry in dirs.into_iter().skip(4) {
            if entry.path() != target {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(())
}

fn persist_result(out_dir: &Path, result: &BuildResult) {
    if let Ok(json) = serde_json::to_vec_pretty(result) {
        let _ = std::fs::create_dir_all(out_dir);
        let tmp = out_dir.join(".last-result.json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, out_dir.join(LAST_RESULT_JSON));
        }
    }
}

fn parse_file_line_errors(output: &str) -> Vec<RawDiag> {
    let re = regex::Regex::new(
        r"(?m)^([^\s:][^:\n]*\.(?:tex|bib|sty|cls|def|cfg|ltx|clo|bbx|cbx|bst)):(\d+):\s*(.+)$",
    )
    .expect("file-line regex");
    re.captures_iter(output)
        .filter_map(|c| {
            let line = c[2].parse().ok()?;
            let message = c[3].trim().to_string();
            if message.is_empty() {
                return None;
            }
            Some(RawDiag {
                level: Level::Error,
                file: Some(c[1].trim_start_matches("./").to_string()),
                line: Some(line),
                message: message.clone(),
                context: String::new(),
                raw: c[0].to_string(),
            })
        })
        .collect()
}

fn auxiliary_failure(output: &str) -> Option<String> {
    const TOOLS: [&str; 5] = ["bibtex", "biber", "dvips", "ps2pdf", "xdvipdfmx"];
    if output.contains("ps2pdf") {
        if let Some(line) = output
            .lines()
            .find(|line| line.contains("Ghostscript") && line.contains("Could not open"))
        {
            return Some(format!("ps2pdf (Ghostscript) failed: {}", line.trim()));
        }
    }
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let tool = TOOLS.into_iter().find(|tool| lower.contains(tool))?;
        if [
            "not found",
            "cannot find",
            "could not",
            "couldn't",
            "failed",
            "error",
            "not recognized",
            "return code",
            "permission denied",
        ]
        .iter()
        .any(|word| lower.contains(word))
        {
            Some(format!("{tool} failed or is unavailable: {}", line.trim()))
        } else {
            None
        }
    })
}

pub struct Builder {
    pub sandbox: Sandbox,
    pub hints: Hints,
    pub timeout: Duration,
    engine: RwLock<Option<Tectonic>>,
    data_dir: PathBuf,
    configured_path: Option<String>,
    texlive_path: Option<String>,
    /// Compiles running at once across the whole server. One compile can take most of a small
    /// machine's memory, so the rest wait for a slot instead of all starting and being killed.
    slots: tokio::sync::Semaphore,
    /// Tickets handed to builds that had to wait, and how many of those have started. The semaphore
    /// is first in, first out, so the difference is how many builds are ahead of a given ticket.
    tickets: std::sync::atomic::AtomicU64,
    served: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub(crate) enum ResolvedEngine {
    Tectonic(Tectonic),
    TexLive(TexLive),
}

impl ResolvedEngine {
    pub(crate) fn version(&self) -> Option<String> {
        match self {
            Self::Tectonic(e) => binary_version(&e.binary),
            Self::TexLive(e) => e.version.clone().or_else(|| binary_version(&e.binary)),
        }
    }
    fn identity(&self) -> String {
        match self {
            Self::Tectonic(e) => format!(
                "tectonic:{}:{}",
                self.version().unwrap_or_default(),
                engine_id(e)
            ),
            Self::TexLive(e) => format!(
                "{}:{}",
                e.kind.id(),
                self.version()
                    .unwrap_or_else(|| e.binary.display().to_string())
            ),
        }
    }
    pub(crate) fn cold(&self, project_dir: &Path) -> bool {
        matches!(self, Self::Tectonic(e) if cache_cold(e, project_dir))
    }
    pub(crate) fn compile(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        main_file: &str,
        draft: bool,
        fetch: bool,
        timeout_s: u64,
    ) -> Result<crate::engine::CompileSpec> {
        match self {
            Self::Tectonic(e) => {
                e.compile(sandbox, project_dir, main_file, draft, fetch, timeout_s)
            }
            Self::TexLive(e) => e.compile(sandbox, project_dir, main_file, draft),
        }
    }
    pub(crate) fn compile_generated(
        &self,
        sandbox: &Sandbox,
        project_dir: &Path,
        file: &str,
        stem: &str,
        fetch: bool,
    ) -> Result<crate::engine::CompileSpec> {
        match self {
            Self::Tectonic(e) => e.compile_generated(sandbox, project_dir, file, stem, fetch),
            Self::TexLive(e) => e.compile_generated(sandbox, project_dir, file, stem),
        }
    }
}

fn binary_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    let text = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    String::from_utf8_lossy(text)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Builder {
    pub fn new(
        sandbox: Sandbox,
        hints: Hints,
        timeout: Duration,
        data_dir: &Path,
        configured_path: Option<String>,
        max_concurrent: usize,
    ) -> Builder {
        let engine = Tectonic::locate(data_dir, configured_path.as_deref());
        Builder {
            sandbox,
            hints,
            timeout,
            engine: RwLock::new(engine),
            data_dir: data_dir.to_path_buf(),
            configured_path,
            texlive_path: None,
            slots: tokio::sync::Semaphore::new(max_concurrent.max(1)),
            tickets: std::sync::atomic::AtomicU64::new(0),
            served: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn with_texlive_path(mut self, path: Option<String>) -> Self {
        self.texlive_path = path;
        self
    }

    pub async fn engine_availability(&self) -> Vec<EngineAvailability> {
        let mut out = Vec::new();
        let mut probes = HashMap::new();
        for kind in EngineKind::ALL {
            out.push(self.availability_for(kind, true, &mut probes).await);
        }
        out
    }

    /// Check only the selected engine's required tools. Builds do not need to probe every
    /// compiler or optional bibliography program before they can start.
    pub async fn check_engine(&self, kind: EngineKind) -> EngineAvailability {
        self.availability_for(kind, false, &mut HashMap::new())
            .await
    }

    async fn availability_for(
        &self,
        kind: EngineKind,
        include_optional: bool,
        probes: &mut HashMap<(PathBuf, bool, Option<PathBuf>), Option<String>>,
    ) -> EngineAvailability {
        let (available, version, reason) = if kind == EngineKind::Tectonic {
            match Tectonic::locate(&self.data_dir, self.configured_path.as_deref()) {
                Some(e) => match self.probe_cached(&e.binary, true, None, probes).await {
                    Some(version) => (true, Some(version), None),
                    None => (
                        false,
                        None,
                        Some("Tectonic cannot run in the selected sandbox".into()),
                    ),
                },
                None => (
                    false,
                    None,
                    Some("Tectonic is not installed yet; the first build can install it".into()),
                ),
            }
        } else {
            let docker = self.sandbox.kind == crate::SandboxKind::Docker;
            match TexLive::locate(kind, self.texlive_path.as_deref(), docker) {
                None => (
                    false,
                    None,
                    Some(format!(
                        "Install latexmk and {}, or set [build] texlive_path",
                        kind.id()
                    )),
                ),
                Some(e) => {
                    let mut missing = Vec::new();
                    let mut version = None;
                    let converters: Vec<&str> = match kind {
                        EngineKind::Latex => vec!["dvips", "ps2pdf", "gs"],
                        EngineKind::XeLatex => vec!["xdvipdfmx"],
                        _ => Vec::new(),
                    };
                    for name in ["latexmk", kind.id(), "kpsewhich"]
                        .into_iter()
                        .chain(converters)
                    {
                        let path = if name == "latexmk" {
                            e.latexmk.clone()
                        } else if name == kind.id() {
                            e.binary.clone()
                        } else if docker {
                            e.path_dir
                                .as_ref()
                                .map(|d| d.join(name))
                                .unwrap_or_else(|| PathBuf::from(name))
                        } else {
                            e.path_dir
                                .as_ref()
                                .map(|d| d.join(name))
                                .filter(|p| p.is_file())
                                .or_else(|| crate::engine::which(name))
                                .unwrap_or_else(|| PathBuf::from(name))
                        };
                        let mut probed = self
                            .probe_cached(&path, false, e.path_dir.as_deref(), probes)
                            .await;
                        if probed.is_none()
                            && docker
                            && name != "latexmk"
                            && name != kind.id()
                            && path != Path::new(name)
                        {
                            probed = self
                                .probe_cached(Path::new(name), false, None, probes)
                                .await;
                        }
                        match probed {
                            Some(v) if name == kind.id() => version = Some(v),
                            Some(_) => {}
                            None => missing.push(name),
                        }
                    }
                    if missing.is_empty() {
                        let mut optional = Vec::new();
                        for &name in if include_optional {
                            &["bibtex", "biber"][..]
                        } else {
                            &[][..]
                        } {
                            let path = if docker {
                                e.path_dir
                                    .as_ref()
                                    .map(|d| d.join(name))
                                    .unwrap_or_else(|| PathBuf::from(name))
                            } else {
                                e.path_dir
                                    .as_ref()
                                    .map(|d| d.join(name))
                                    .filter(|p| p.is_file())
                                    .or_else(|| crate::engine::which(name))
                                    .unwrap_or_else(|| PathBuf::from(name))
                            };
                            let mut probed = self
                                .probe_cached(&path, false, e.path_dir.as_deref(), probes)
                                .await;
                            if probed.is_none() && docker && path != Path::new(name) {
                                probed = self
                                    .probe_cached(Path::new(name), false, None, probes)
                                    .await;
                            }
                            if probed.is_none() {
                                optional.push(name);
                            }
                        }
                        let reason = if optional.is_empty() {
                            None
                        } else {
                            Some(format!(
                                "{} unavailable; documents that require those bibliography tools will fail",
                                optional.join(", ")
                            ))
                        };
                        (true, version, reason)
                    } else {
                        (
                            false,
                            None,
                            Some(format!(
                                "{} unavailable in the selected sandbox; install it in the host or Docker image",
                                missing.join(", ")
                            )),
                        )
                    }
                }
            }
        };
        EngineAvailability {
            engine: kind,
            available,
            version,
            reason,
        }
    }

    async fn probe_program(
        &self,
        program: &Path,
        tectonic: bool,
        custom_bin: Option<&Path>,
    ) -> Option<String> {
        static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("galley-probe-{}-{seq}", std::process::id()));
        let out_dir = dir.join(".galley/build");
        if std::fs::create_dir_all(&out_dir).is_err() {
            return None;
        }
        let mut mounts = Vec::new();
        let mut run_program = program.to_path_buf();
        if tectonic && self.sandbox.kind != crate::SandboxKind::None {
            run_program = PathBuf::from("/galley/bin/tectonic");
            mounts.push(crate::sandbox::Mount {
                host: program.to_path_buf(),
                guest: run_program.clone(),
                writable: false,
            });
        } else if self.sandbox.kind == crate::SandboxKind::Bwrap {
            if let Some(bin) = custom_bin {
                let probe = TexLive {
                    kind: EngineKind::PdfLatex,
                    latexmk: program.to_path_buf(),
                    binary: program.to_path_buf(),
                    path_dir: Some(bin.to_path_buf()),
                    version: None,
                };
                mounts.extend(probe.sandbox_mounts(&self.sandbox));
            }
        }
        if self.sandbox.kind == crate::SandboxKind::Docker && !tectonic && !program.is_absolute() {
            run_program = PathBuf::from(program.file_name().unwrap_or_default());
        }
        let env = custom_bin
            .map(|bin| {
                let old = if self.sandbox.kind == crate::SandboxKind::Docker {
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()
                } else {
                    std::env::var("PATH").unwrap_or_default()
                };
                vec![("PATH".into(), format!("{}:{old}", bin.display()))]
            })
            .unwrap_or_default();
        let spec = crate::sandbox::Spec {
            project_dir: dir.clone(),
            out_dir,
            mounts,
            masked: Vec::new(),
            env,
            network: false,
            program: run_program,
            args: vec!["--version".into()],
            name: format!("galley-probe-{}-{seq}", std::process::id()),
        };
        let mut command = self.sandbox.command(&spec);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            }
        };
        let pid = child.id();
        let attempted =
            tokio::time::timeout(Duration::from_secs(8), child.wait_with_output()).await;
        if attempted.is_err() {
            self.sandbox.kill_group(pid).await;
            self.sandbox.kill(&spec).await;
        }
        let _ = std::fs::remove_dir_all(dir);
        let output = attempted.ok()?.ok()?;
        if !output.status.success()
            && program.file_name().and_then(|n| n.to_str()) != Some("ps2pdf")
        {
            return None;
        }
        let text = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        Some(
            String::from_utf8_lossy(text)
                .lines()
                .next()
                .unwrap_or("available")
                .trim()
                .to_string(),
        )
    }

    async fn probe_cached(
        &self,
        program: &Path,
        tectonic: bool,
        custom_bin: Option<&Path>,
        probes: &mut HashMap<(PathBuf, bool, Option<PathBuf>), Option<String>>,
    ) -> Option<String> {
        let key = (
            program.to_path_buf(),
            tectonic,
            custom_bin.map(Path::to_path_buf),
        );
        if let Some(result) = probes.get(&key) {
            return result.clone();
        }
        let result = self.probe_program(program, tectonic, custom_bin).await;
        probes.insert(key, result.clone());
        result
    }

    pub(crate) async fn resolve_engine(
        &self,
        kind: EngineKind,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<ResolvedEngine> {
        if kind == EngineKind::Tectonic {
            return self
                .ensure_engine(progress)
                .await
                .map(ResolvedEngine::Tectonic);
        }
        let mut e = TexLive::locate(
            kind,
            self.texlive_path.as_deref(),
            self.sandbox.kind == crate::SandboxKind::Docker,
        )
        .ok_or_else(|| {
            crate::Error::Engine(format!(
                "{} is unavailable: install latexmk and {}, or set [build] texlive_path",
                kind.id(),
                kind.id()
            ))
        })?;
        let availability = self.check_engine(kind).await;
        if !availability.available {
            return Err(crate::Error::Engine(
                availability
                    .reason
                    .unwrap_or_else(|| format!("{} unavailable", kind.id())),
            ));
        }
        e.version = availability.version;
        Ok(ResolvedEngine::TexLive(e))
    }

    pub async fn engine_path(&self) -> Option<PathBuf> {
        self.engine.read().await.as_ref().map(|e| e.binary.clone())
    }

    /// Install Tectonic on first use. `progress` feeds the build bar.
    pub(crate) async fn ensure_engine(
        &self,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<Tectonic> {
        if let Some(e) = self.engine.read().await.as_ref() {
            return Ok(e.clone());
        }
        let mut slot = self.engine.write().await;
        if let Some(e) = slot.as_ref() {
            return Ok(e.clone());
        }
        if let Some(found) = Tectonic::locate(&self.data_dir, self.configured_path.as_deref()) {
            *slot = Some(found.clone());
            return Ok(found);
        }
        install::install_tectonic(&Tectonic::install_dir(&self.data_dir), progress).await?;
        let found = Tectonic::locate(&self.data_dir, None).ok_or_else(|| {
            crate::Error::Engine("Tectonic was downloaded but cannot be found".into())
        })?;
        *slot = Some(found.clone());
        Ok(found)
    }

    /// Wait for a free compile slot, telling the author where they are in the queue. A number that
    /// counts down reads as progress, and a bare "waiting" reads as a hang.
    async fn wait_for_slot(
        &self,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Option<tokio::sync::SemaphorePermit<'_>> {
        use std::sync::atomic::Ordering::SeqCst;
        let ticket = self.tickets.fetch_add(1, SeqCst);
        let acquire = self.slots.acquire();
        tokio::pin!(acquire);
        let mut tick = tokio::time::interval(Duration::from_millis(700));
        let mut shown = u64::MAX;
        let permit = loop {
            tokio::select! {
                p = &mut acquire => break p.ok(),
                _ = tick.tick() => {
                    let ahead = ticket.saturating_sub(self.served.load(SeqCst)) + 1;
                    if ahead != shown {
                        shown = ahead;
                        progress(if ahead == 1 {
                            "Next in line. Starting in a moment…".to_string()
                        } else {
                            let n = ahead - 1;
                            format!("In line: {n} build{} ahead of yours…", if n == 1 { "" } else { "s" })
                        });
                    }
                }
            }
        };
        self.served.fetch_add(1, SeqCst);
        permit
    }

    /// Compile `main_file` inside `project_dir`. Bad input never causes a panic. Every failure
    /// becomes a `BuildResult` that the UI can show.
    pub async fn run(
        &self,
        id: u64,
        project_dir: &Path,
        main_file: &str,
        req: &BuildRequest,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> BuildResult {
        let latest = || true;
        self.run_checked(
            id,
            project_dir,
            main_file,
            req,
            progress,
            RunControl {
                latest: &latest,
                publication_gate: None,
            },
        )
        .await
    }

    async fn run_checked(
        &self,
        id: u64,
        project_dir: &Path,
        main_file: &str,
        req: &BuildRequest,
        progress: &(dyn Fn(String) + Send + Sync),
        control: RunControl<'_>,
    ) -> BuildResult {
        // Wait for a slot before timing starts, so the profile shows compile time and not queue time.
        let _slot = match self.slots.try_acquire() {
            Ok(permit) => Some(permit),
            Err(_) => self.wait_for_slot(progress).await,
        };
        let started = Instant::now();
        let deadline = started + self.timeout;
        let out_dir = project_dir.join(".galley").join("build");
        let mut result = BuildResult {
            id,
            status: BuildStatus::Error,
            errors: Vec::new(),
            error_count: 0,
            warning_count: 0,
            profile: Profile::default(),
            pages: None,
            figures: None,
            pending_figures: Vec::new(),
            fetched_packages: false,
            pdf_fresh: false,
            pdf_available: out_dir.join(LAST_GOOD_PDF).exists(),
            stale: false,
            draft: req.draft,
            main_file: main_file.to_string(),
            engine: req.engine.id().into(),
            engine_version: None,
            pdf_engine: read_producer(&out_dir).map(|p| p.engine),
            pdf_engine_version: read_producer(&out_dir).and_then(|p| p.version),
            sandbox: self.sandbox.kind.to_string(),
            finished_at: Utc::now(),
            message: None,
        };

        if !project_dir.join(main_file).is_file() {
            result.message = Some(format!(
                "{main_file} does not exist. Create it, or pick another main file in project settings."
            ));
            return self.finish(result, started, &out_dir);
        }
        let engine = match self.resolve_engine(req.engine, progress).await {
            Ok(e) => e,
            Err(e) => {
                result.message = Some(e.to_string());
                return self.finish(result, started, &out_dir);
            }
        };
        result.engine_version = engine.version();

        let main_text = std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default();
        let files = list_text_files(project_dir);
        let svg_problems = if req.engine != EngineKind::Latex {
            let dir = project_dir.to_path_buf();
            tokio::task::spawn_blocking(move || crate::svg::prepare(&dir))
                .await
                .unwrap_or_default()
        } else {
            main_text
                .lines()
                .position(|line| line.contains("\\includesvg"))
                .map(|line| Diagnostic {
                    level: Level::Lint,
                    file: Some(main_file.to_string()),
                    line: Some((line + 1) as u32),
                    code: "svg-dvi-unsupported".into(),
                    message:
                        "SVG auto-conversion is unavailable with latex (DVI); use EPS or PS figures"
                            .into(),
                    hint: None,
                    explain: None,
                    fix: None,
                    raw: String::new(),
                })
                .into_iter()
                .collect()
        };

        // A project with no Tectonic bundle needs one network-enabled build to fill its writable
        // local cache. Existing shared cache contents are copied there before the first compile.
        let cold = engine.cold(project_dir);
        progress(
            if cold {
                "Fetching packages…"
            } else {
                "Compiling…"
            }
            .into(),
        );
        let t0 = Instant::now();

        // The figure cache runs first when it can. It returns a run to publish, or the reason it
        // could not. In the second case the plain compile below takes over.
        let tex: Vec<(String, String)> = files
            .iter()
            .filter(|f| f.ends_with(".tex"))
            .filter_map(|f| {
                std::fs::read_to_string(project_dir.join(f))
                    .ok()
                    .map(|t| (f.clone(), t))
            })
            .collect();
        let eligibility = if req.draft {
            Err(String::new())
        } else if req.engine == EngineKind::Latex {
            Err("PDF figure caching is unavailable for the latex DVI workflow".into())
        } else {
            figures::eligibility(&main_text, &tex)
        };
        let has_pictures =
            !matches!(&eligibility, Err(why) if why.is_empty() || why == figures::NO_PICTURES);
        let mut use_cache = false;
        if has_pictures {
            result.figures = match (&eligibility, req.figure_cache) {
                (_, false) => Some(FigureStats::off("turned off for this project")),
                (Err(why), true) => Some(FigureStats::off(why)),
                (Ok(()), true) if cold => {
                    Some(FigureStats::off("the first build fetches packages"))
                }
                (Ok(()), true) => {
                    use_cache = true;
                    None
                }
            };
        }
        let mut cached_run = None;
        if use_cache {
            let pass = self
                .figure_pass(
                    &engine,
                    project_dir,
                    main_file,
                    &main_text,
                    progress,
                    deadline,
                )
                .await;
            result.profile.figure_ms = pass.figure_ms;
            result.figures = Some(pass.stats);
            result.pending_figures = pass.pending;
            cached_run = pass.run;
        }

        if Instant::now() >= deadline {
            result.status = BuildStatus::Timeout;
            result.message = Some(format!(
                "The build was stopped after {} s",
                self.timeout.as_secs()
            ));
            return self.finish(result, started, &out_dir);
        }

        let mut run = match cached_run {
            Some(r) => r,
            None => {
                let tp = Instant::now();
                let r = match self
                    .compile_once(&engine, project_dir, main_file, req.draft, cold, deadline)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        result.message = Some(e.to_string());
                        return self.finish(result, started, &out_dir);
                    }
                };
                if use_cache && !r.timed_out {
                    let mut cache = FigCache::load(&out_dir);
                    cache.plain_ms = tp.elapsed().as_millis() as u64;
                    cache.save(&out_dir);
                }
                r
            }
        };
        result.fetched_packages = cold;
        result.profile.compile_ms = t0.elapsed().as_millis() as u64;

        if run.timed_out {
            result.status = BuildStatus::Timeout;
            result.message = Some(format!(
                "The build was stopped after {} s. Look for an infinite loop or a huge figure, or raise [build] timeout_s.",
                self.timeout.as_secs()
            ));
            return self.finish(result, started, &out_dir);
        }

        // Missing bundle files mean packages this project uses are not cached yet: run once more
        // with network access so the engine fills the project-local cache.
        if req.engine == EngineKind::Tectonic && needs_fetch(&run.raw, project_dir) {
            progress("Fetching packages…".into());
            let t1 = Instant::now();
            match self
                .compile_once(&engine, project_dir, main_file, req.draft, true, deadline)
                .await
            {
                Ok(r2) => {
                    run = r2;
                    result.fetched_packages = true;
                    result.profile.fetch_ms = t1.elapsed().as_millis() as u64;
                }
                Err(e) => {
                    result.message = Some(e.to_string());
                    return self.finish(result, started, &out_dir);
                }
            }
            if run.timed_out {
                result.status = BuildStatus::Timeout;
                result.message = Some(
                    "Fetching packages took too long. Check the network and build again.".into(),
                );
                return self.finish(result, started, &out_dir);
            }
        }

        result.pages = parse_pages(&run.log);
        if !run.exit_ok {
            result.message = auxiliary_failure(&run.stderr);
        }
        let view = ProjectView {
            main_file,
            main_text: &main_text,
        };
        let mut diags: Vec<Diagnostic> = run
            .raw
            .into_iter()
            .map(|mut d| {
                d.file = d
                    .file
                    .as_deref()
                    .map(|f| resolve_file(f, project_dir, main_file, &files));
                self.hints.annotate(d, &view)
            })
            .collect();
        if diags.is_empty() && !run.exit_ok {
            // The engine needs the CA bundle to reach the package bundle. Without it Tectonic
            // panics before it names the file it wanted, so say what is actually wrong.
            let no_certs = run.stderr.contains("No CA certificates were loaded");
            diags.push(Diagnostic {
                level: Level::Error,
                file: None,
                line: None,
                code: if no_certs {
                    "engine-no-certs".into()
                } else {
                    "engine-failed".into()
                },
                message: if no_certs {
                    "The engine could not read the system certificates".into()
                } else {
                    auxiliary_failure(&run.stderr)
                        .or(run.stderr_summary.clone())
                        .unwrap_or_else(|| "The engine exited with an error".into())
                },
                hint: Some(if no_certs {
                    "Install ca-certificates on the host, then build again. Galley mounts the host \
                     bundle into the sandbox, and the engine needs it to download packages."
                        .into()
                } else {
                    "The log has no LaTeX error; the raw output below may explain it.".to_string()
                }),
                explain: None,
                fix: None,
                raw: run.stderr.clone(),
            });
        }
        result.error_count = diags.iter().filter(|d| d.level == Level::Error).count();
        result.warning_count = diags.iter().filter(|d| d.level == Level::Warning).count();
        // Lint after the compiler's own diagnostics. The lint never changes the counts or the status.
        let texts: Vec<(String, String)> = files
            .iter()
            .filter(|f| f.ends_with(".tex") || f.ends_with(".bib"))
            .filter_map(|f| {
                std::fs::read_to_string(project_dir.join(f))
                    .ok()
                    .map(|t| (f.clone(), t))
            })
            .collect();
        diags.extend(svg_problems);
        diags.extend(crate::lint::lint(&texts, &req.lint_disabled));
        // The bibliography checks come from the paper index, which holds the citations of the
        // document. The lint rule named `uncited` reports that issue, so nothing is reported twice.
        let paper = galley_index::scan(project_dir, main_file);
        for f in galley_index::bib::audit(&paper) {
            if f.issue == galley_index::bib::Issue::Uncited
                || req.lint_disabled.iter().any(|d| d == f.issue.code())
            {
                continue;
            }
            diags.push(Diagnostic {
                level: Level::Lint,
                file: Some(f.file),
                line: Some(f.line),
                code: f.issue.code().to_string(),
                message: f.message,
                hint: f.hint,
                explain: None,
                fix: None,
                raw: String::new(),
            });
        }
        result.errors = diags;

        if Instant::now() >= deadline {
            result.status = BuildStatus::Timeout;
            result.message = Some(format!(
                "The build was stopped after {} s",
                self.timeout.as_secs()
            ));
            return self.finish(result, started, &out_dir);
        }

        let pdf = run.output_dir.join(format!("{}.pdf", run.stem));
        if run.exit_ok && result.error_count == 0 && pdf.exists() {
            let _gate = if let Some(gate) = control.publication_gate {
                Some(gate.lock().await)
            } else {
                None
            };
            if !(control.latest)() {
                result.status = BuildStatus::Failed;
                result.stale = true;
                result.message = Some("A newer build was requested".into());
                let _ = std::fs::write(out_dir.join(LAST_LOG), &run.log);
                return self.finish(result, started, &out_dir);
            }
            let st = run.output_dir.join(format!("{}.synctex.gz", run.stem));
            match publish_pdf(
                &out_dir,
                &pdf,
                st.is_file().then_some(st.as_path()),
                &result,
            ) {
                Ok(()) => {
                    result.status = BuildStatus::Ok;
                    result.pdf_fresh = true;
                    result.pdf_available = true;
                    result.pdf_engine = Some(result.engine.clone());
                    result.pdf_engine_version = result.engine_version.clone();
                }
                Err(e) => {
                    result.status = BuildStatus::Failed;
                    result.message =
                        Some(format!("The PDF was produced but could not be saved: {e}"));
                }
            }
        } else {
            result.status = BuildStatus::Failed;
            if !(control.latest)() {
                result.stale = true;
                result.message = Some("A newer build was requested".into());
            }
        }
        let _ = std::fs::write(out_dir.join(LAST_LOG), &run.log);
        self.finish(result, started, &out_dir)
    }

    /// Figure cache step (SPEC §13.1). Returns a run to publish only when every figure PDF is
    /// present and built from the current picture code.
    async fn figure_pass(
        &self,
        engine: &ResolvedEngine,
        project_dir: &Path,
        main_file: &str,
        main_text: &str,
        progress: &(dyn Fn(String) + Send + Sync),
        deadline: Instant,
    ) -> FigurePass {
        let out_dir = project_dir.join(".galley").join("build");
        let mut cache = FigCache::load(&out_dir);
        let key = figures::project_key(project_dir, main_text, &engine.identity());
        if cache.key != key {
            // Wrapper job names are shared across engines. Remove its aux, bibliography,
            // latexmk state, and PDFs before another engine can read them.
            figures::clear(&out_dir);
            cache.figures.clear();
            cache.failed.clear();
            cache.key = key;
        }
        let plain = |cache: &FigCache, stats: FigureStats, pending: Vec<String>, figure_ms: u64| {
            cache.save(&out_dir);
            FigurePass {
                run: None,
                stats,
                pending,
                figure_ms,
            }
        };
        let bypass = |why: &str| FigureStats {
            mode: "plain".into(),
            reason: Some(why.to_string()),
            ..Default::default()
        };

        let t = Instant::now();
        let Ok(mut run) = self
            .wrapper_once(engine, project_dir, main_file, deadline, false)
            .await
        else {
            return plain(
                &cache,
                bypass("the cached pass could not start"),
                Vec::new(),
                0,
            );
        };
        if matches!(engine, ResolvedEngine::Tectonic(_)) && needs_fetch(&run.raw, project_dir) {
            if let Ok(fetched) = self
                .wrapper_once(engine, project_dir, main_file, deadline, true)
                .await
            {
                run = fetched;
            }
        }
        cache.wrapper_ms = t.elapsed().as_millis() as u64;
        if run.timed_out || needs_fetch(&run.raw, project_dir) {
            return plain(
                &cache,
                bypass("a required figure package or font is unavailable in the engine cache"),
                Vec::new(),
                0,
            );
        }
        let names = figures::figure_list(&out_dir);
        if names.is_empty() {
            let reason = run
                .raw
                .iter()
                .find(|d| d.level == Level::Error)
                .map(|d| d.message.as_str())
                .unwrap_or("TikZ is not loaded by the document");
            return plain(&cache, bypass(reason), Vec::new(), 0);
        }
        let todo = figures::stale(&out_dir, &cache, &names);
        let blocked = todo
            .iter()
            .filter(|n| {
                cache
                    .failed
                    .get(*n)
                    .is_some_and(|m| Some(m) == figures::current_md5(&out_dir, n).as_ref())
            })
            .count();
        let total = names.len();
        let stats =
            |cached: usize, pending: usize, mode: &str, reason: Option<String>| FigureStats {
                mode: mode.into(),
                total,
                cached,
                pending,
                reason,
            };
        if blocked > 0 {
            return plain(
                &cache,
                stats(total - todo.len(), 0, "plain", Some(own_failures(blocked))),
                Vec::new(),
                0,
            );
        }
        if todo.is_empty() {
            cache.save(&out_dir);
            return FigurePass {
                run: Some(run),
                stats: stats(total, 0, "cached", None),
                pending: Vec::new(),
                figure_ms: 0,
            };
        }

        // Regenerate now only when that is faster than a plain compile. Use the times of earlier builds.
        let par = figure_parallelism();
        let rounds = todo.len().div_ceil(par) as u64;
        let now = if cache.plain_ms > 0 && cache.figure_ms > 0 {
            rounds * cache.figure_ms + cache.wrapper_ms < cache.plain_ms
        } else {
            todo.len() <= par
        };
        if !now {
            let pending = todo.clone();
            return plain(
                &cache,
                stats(total - todo.len(), todo.len(), "plain", None),
                pending,
                0,
            );
        }

        progress(format!(
            "Caching {} figure{}…",
            todo.len(),
            if todo.len() == 1 { "" } else { "s" }
        ));
        let t = Instant::now();
        let failed = self
            .make_figures(engine, project_dir, &todo, &mut cache, Some(deadline))
            .await;
        let figure_ms = t.elapsed().as_millis() as u64;
        if failed > 0 {
            return plain(
                &cache,
                stats(total - todo.len(), 0, "plain", Some(own_failures(failed))),
                Vec::new(),
                figure_ms,
            );
        }
        progress("Compiling…".into());
        let Ok(run) = self
            .wrapper_once(engine, project_dir, main_file, deadline, false)
            .await
        else {
            return plain(
                &cache,
                bypass("the cached pass could not start"),
                Vec::new(),
                figure_ms,
            );
        };
        let names = figures::figure_list(&out_dir);
        let left = figures::stale(&out_dir, &cache, &names);
        if run.timed_out || !left.is_empty() {
            let why = Some("figures changed during the build".to_string());
            return plain(
                &cache,
                stats(names.len() - left.len(), 0, "plain", why),
                Vec::new(),
                figure_ms,
            );
        }
        cache.save(&out_dir);
        FigurePass {
            run: Some(run),
            stats: stats(names.len(), 0, "cached", None),
            pending: Vec::new(),
            figure_ms,
        }
    }

    async fn wrapper_once(
        &self,
        engine: &ResolvedEngine,
        project_dir: &Path,
        main_file: &str,
        deadline: Instant,
        fetch: bool,
    ) -> Result<RunOutput> {
        let out_dir = project_dir.join(".galley").join("build");
        std::fs::create_dir_all(&out_dir)?;
        let file = format!("{}.tex", figures::WRAPPER_STEM);
        std::fs::write(out_dir.join(&file), figures::wrapper_source(main_file))?;
        let spec = engine.compile_generated(
            &self.sandbox,
            project_dir,
            &file,
            figures::WRAPPER_STEM,
            fetch,
        )?;
        self.run_spec_limited(spec, deadline.saturating_duration_since(Instant::now()))
            .await
    }

    /// Compile `names` on their own, `figure_parallelism()` at a time, recording which picture
    /// code each PDF was built from. Returns how many failed.
    async fn make_figures(
        &self,
        engine: &ResolvedEngine,
        project_dir: &Path,
        names: &[String],
        cache: &mut FigCache,
        deadline: Option<Instant>,
    ) -> usize {
        let out_dir = project_dir.join(".galley").join("build");
        let mut failed = 0;
        let mut rounds = 0u64;
        let t = Instant::now();
        for chunk in names.chunks(figure_parallelism()) {
            rounds += 1;
            let mut jobs = Vec::new();
            for n in chunk {
                // The key a PDF is built from is the one the main pass just wrote.
                let md5 = figures::current_md5(&out_dir, n).unwrap_or_default();
                let file = format!("{n}.tex");
                let spec = std::fs::write(out_dir.join(&file), figures::figure_source())
                    .map_err(crate::Error::from)
                    .and_then(|_| {
                        engine.compile_generated(&self.sandbox, project_dir, &file, n, false)
                    });
                jobs.push(async move {
                    let ok = match spec {
                        Ok(spec) => self
                            .run_spec_limited(
                                spec,
                                deadline
                                    .map(|d| d.saturating_duration_since(Instant::now()))
                                    .unwrap_or(self.timeout),
                            )
                            .await
                            .map(|r| r.exit_ok && !r.timed_out)
                            .unwrap_or(false),
                        Err(_) => false,
                    };
                    (n.clone(), md5, ok)
                });
            }
            for (n, md5, ok) in futures_util::future::join_all(jobs).await {
                let pdf = out_dir.join(format!("{n}.pdf"));
                if ok && pdf.is_file() && !md5.is_empty() {
                    cache.failed.remove(&n);
                    cache.figures.insert(n, md5);
                } else {
                    failed += 1;
                    let _ = std::fs::remove_file(&pdf);
                    cache.figures.remove(&n);
                    cache.failed.insert(n, md5);
                }
            }
        }
        if let Some(per_round) = (t.elapsed().as_millis() as u64).checked_div(rounds) {
            cache.figure_ms = per_round;
        }
        cache.save(&out_dir);
        failed
    }

    /// Generate figures a plain build left for later. `stop` is polled between rounds, so a new
    /// build request never waits for more than one round.
    pub async fn warm_figures<F, Fut>(
        &self,
        project_dir: &Path,
        engine_kind: EngineKind,
        names: Vec<String>,
        stop: F,
    ) -> usize
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let Ok(engine) = self.resolve_engine(engine_kind, &|_| {}).await else {
            return 0;
        };
        let out_dir = project_dir.join(".galley").join("build");
        let mut cache = FigCache::load(&out_dir);
        let mut made = 0;
        for chunk in names.chunks(figure_parallelism()) {
            if stop().await {
                break;
            }
            let failed = self
                .make_figures(&engine, project_dir, chunk, &mut cache, None)
                .await;
            made += chunk.len() - failed;
        }
        made
    }

    /// Clear the figure cache. This removes every figure PDF and the cache state.
    pub fn clear_figures(&self, project_dir: &Path) -> usize {
        figures::clear(&project_dir.join(".galley").join("build"))
    }

    fn finish(&self, mut result: BuildResult, started: Instant, out_dir: &Path) -> BuildResult {
        result.profile.total_ms = started.elapsed().as_millis() as u64;
        result.finished_at = Utc::now();
        if let Ok(json) = serde_json::to_vec_pretty(&result.errors) {
            let _ = std::fs::create_dir_all(out_dir);
            let _ = std::fs::write(out_dir.join(ERRORS_JSON), json);
        }
        persist_result(out_dir, &result);
        result
    }

    async fn compile_once(
        &self,
        engine: &ResolvedEngine,
        project_dir: &Path,
        main_file: &str,
        draft: bool,
        fetch: bool,
        deadline: Instant,
    ) -> Result<RunOutput> {
        let compile = engine.compile(
            &self.sandbox,
            project_dir,
            main_file,
            draft,
            fetch,
            self.timeout.as_secs(),
        )?;
        self.run_spec_limited(compile, deadline.saturating_duration_since(Instant::now()))
            .await
    }

    /// Run one prepared compile spec. The normal build and the latexdiff compare both use it.
    pub(crate) async fn run_spec(&self, compile: crate::engine::CompileSpec) -> Result<RunOutput> {
        self.run_spec_limited(compile, self.timeout).await
    }

    async fn run_spec_limited(
        &self,
        compile: crate::engine::CompileSpec,
        limit: Duration,
    ) -> Result<RunOutput> {
        let out_dir = compile.output_dir.clone();
        if out_dir != compile.spec.out_dir {
            let _ = std::fs::remove_dir_all(&out_dir);
            std::fs::create_dir_all(&out_dir)?;
        }
        let log_path = out_dir.join(format!("{}.log", compile.stem));
        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(out_dir.join(format!("{}.pdf", compile.stem)));

        let mut cmd = self.sandbox.command(&compile.spec);
        let child = cmd.spawn().map_err(|e| {
            crate::Error::Engine(format!(
                "could not start the {} sandbox: {e}. Run `galley doctor`, or set [build] sandbox in galley.toml.",
                self.sandbox.kind
            ))
        })?;
        let pid = child.id();
        let (exit_ok, timed_out, stderr) =
            match tokio::time::timeout(limit, child.wait_with_output()).await {
                Ok(Ok(out)) => (
                    out.status.success(),
                    false,
                    format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    ),
                ),
                Ok(Err(e)) => {
                    return Err(crate::Error::Engine(format!(
                        "the engine could not be run: {e}"
                    )));
                }
                Err(_) => {
                    self.sandbox.kill_group(pid).await;
                    self.sandbox.kill(&compile.spec).await;
                    (false, true, String::new())
                }
            };
        let log = std::fs::read_to_string(&log_path).unwrap_or_else(|_| stderr.clone());
        let mut raw = parse_log(&log);
        for diag in parse_file_line_errors(&stderr)
            .into_iter()
            .chain(parse_file_line_errors(&log))
        {
            if !raw
                .iter()
                .any(|d| d.file == diag.file && d.line == diag.line && d.message == diag.message)
            {
                raw.push(diag);
            }
        }
        let stderr_summary = stderr
            .lines()
            .find_map(|l| l.strip_prefix("error: "))
            .map(|s| s.trim_end_matches('.').to_string())
            .or_else(|| {
                stderr.lines().rev().find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    (["error", "failed", "not found", "missing", "could not"]
                        .iter()
                        .any(|word| lower.contains(word)))
                    .then(|| line.trim().to_string())
                })
            });
        Ok(RunOutput {
            stem: compile.stem,
            output_dir: out_dir,
            exit_ok,
            timed_out,
            log,
            stderr,
            stderr_summary,
            raw,
        })
    }

    /// Tells whether `latexdiff` is available. The UI hides PDF compare when it is not.
    pub fn latexdiff_available(&self) -> bool {
        latexdiff_bin().is_some()
    }

    /// Make a marked-up PDF that shows the changes between two versions of a `.tex` file. First run
    /// latexdiff on the two texts. latexdiff runs on the host because it is a pure text transform.
    /// Then compile its output with the sandboxed engine. The PDF lands at
    /// `.galley/build/last-diff.pdf`. This returns an error if latexdiff is not installed, or if
    /// the marked-up document does not compile.
    pub async fn latexdiff(
        &self,
        project_dir: &Path,
        old_text: &str,
        new_text: &str,
        engine_kind: EngineKind,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<()> {
        let bin = latexdiff_bin().ok_or(crate::Error::LatexdiffUnavailable)?;
        let engine = self.resolve_engine(engine_kind, progress).await?;
        let out_dir = project_dir.join(".galley").join("build");
        std::fs::create_dir_all(&out_dir)?;
        let old_path = out_dir.join("galley-diff-old.tex");
        let new_path = out_dir.join("galley-diff-new.tex");
        std::fs::write(&old_path, old_text)?;
        std::fs::write(&new_path, new_text)?;

        progress("Marking up changes…".into());
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&bin)
                .arg(&old_path)
                .arg(&new_path)
                .output()
        })
        .await
        .map_err(|e| crate::Error::Engine(format!("latexdiff task failed: {e}")))?
        .map_err(|e| crate::Error::Engine(format!("could not run latexdiff: {e}")))?;
        if !output.status.success() {
            let msg = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Engine(format!(
                "latexdiff failed: {}",
                msg.lines().next().unwrap_or("unknown error")
            )));
        }
        std::fs::write(out_dir.join("galley-diff.tex"), &output.stdout)?;

        progress("Compiling the comparison…".into());
        let cold = engine.cold(project_dir);
        let spec = engine.compile_generated(
            &self.sandbox,
            project_dir,
            "galley-diff.tex",
            "galley-diff",
            cold,
        )?;
        let mut run = self.run_spec(spec).await?;
        if engine_kind == EngineKind::Tectonic && needs_fetch(&run.raw, project_dir) {
            let spec = engine.compile_generated(
                &self.sandbox,
                project_dir,
                "galley-diff.tex",
                "galley-diff",
                true,
            )?;
            run = self.run_spec(spec).await?;
        }
        let pdf = run.output_dir.join("galley-diff.pdf");
        if run.exit_ok
            && !run.timed_out
            && run.raw.iter().all(|d| d.level != Level::Error)
            && pdf.is_file()
        {
            std::fs::copy(&pdf, out_dir.join(LAST_DIFF_PDF))?;
            Ok(())
        } else {
            Err(crate::Error::Engine(
                "the comparison did not produce a PDF; the marked-up document may not compile"
                    .into(),
            ))
        }
    }
}

/// Find `latexdiff`. Look at the explicit override first, then at `$PATH`.
fn latexdiff_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GALLEY_LATEXDIFF") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("latexdiff"))
        .find(|p| p.is_file())
}

pub(crate) struct FigurePass {
    run: Option<RunOutput>,
    stats: FigureStats,
    pending: Vec<String>,
    figure_ms: u64,
}

/// Figure jobs are whole-document passes. More than a few at the same time overloads a small host.
fn figure_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 4)
}

/// Identifies the engine build, so an engine upgrade drops every cached figure.
fn engine_id(engine: &Tectonic) -> String {
    let meta = std::fs::metadata(&engine.binary).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}:{len}:{mtime}", engine.binary.display())
}

pub(crate) struct RunOutput {
    pub(crate) stem: String,
    pub(crate) output_dir: PathBuf,
    pub(crate) exit_ok: bool,
    pub(crate) timed_out: bool,
    pub(crate) log: String,
    pub(crate) stderr: String,
    pub(crate) stderr_summary: Option<String>,
    pub(crate) raw: Vec<RawDiag>,
}

/// A `File not found` for something that is not in the project points at the bundle cache.
/// A cold Tectonic cache is empty or missing. It means that no bundle was downloaded before, so
/// the first compile must run with the network on.
pub(crate) fn cache_cold(engine: &Tectonic, project_dir: &Path) -> bool {
    let local = Tectonic::local_cache_dir(project_dir);
    let dir = if local.is_dir() {
        &local
    } else {
        &engine.cache_dir
    };
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    }
}

pub(crate) fn needs_fetch(raw: &[RawDiag], project_dir: &Path) -> bool {
    let re = regex::Regex::new(r"File `([^']+)' not found").expect("regex");
    raw.iter().any(|d| {
        if d.level != Level::Error {
            return false;
        }
        // A font that the bundle has but the cache lacks fails in the same way as a missing
        // package file. The first italic or small-caps run in an 11pt document is a typical cause.
        let font_missing = d.message.contains("not loadable")
            || d.raw.contains("installed font not found")
            || d.raw.contains("Metric (TFM) file");
        let tikz_library_missing = d.message.contains("did not find the tikz library");
        font_missing
            || tikz_library_missing
            || re.captures(&d.message).is_some_and(|c| {
                let name = &c[1];
                let looks_like_bundle_file = !name.contains('/') && name.contains('.');
                looks_like_bundle_file && !project_dir.join(name).exists()
            })
    })
}

/// `sections/method` becomes `sections/method.tex`. `./main.tex` becomes `main.tex`. The draft
/// wrapper becomes main.
/// Galley uses a build target from the client only if it is a relative `.tex` path with no
/// traversal, and the file exists in the project.
fn is_safe_target(project_dir: &Path, file: &str) -> bool {
    if !file.ends_with(".tex") || file.starts_with('/') || file.contains('\\') {
        return false;
    }
    if Path::new(file)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return false;
    }
    project_dir.join(file).is_file()
}

fn resolve_file(name: &str, project_dir: &Path, main_file: &str, files: &[String]) -> String {
    let relative = name
        .strip_prefix(project_dir.to_str().unwrap_or("\0"))
        .unwrap_or(name);
    let name = relative
        .trim_start_matches("/work/")
        .trim_start_matches('/')
        .trim_start_matches("./");
    if name.ends_with("galley-draft.tex")
        || name.ends_with("galley-draft")
        || name.ends_with("galley-fig.tex")
    {
        return main_file.to_string();
    }
    if files.iter().any(|f| f == name) {
        return name.to_string();
    }
    for ext in [".tex", ".bib", ".sty", ".cls"] {
        let candidate = format!("{name}{ext}");
        if files.contains(&candidate) {
            return candidate;
        }
    }
    name.to_string()
}

fn list_text_files(project_dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || name == ".galley" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(project_dir, project_dir, &mut out);
    out
}

pub type BeforeBuild = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The queue for one project. At most one build runs. At most one build waits, the newest one.
pub struct ProjectBuilds {
    builder: Arc<Builder>,
    project_dir: PathBuf,
    main_file: RwLock<String>,
    pending: Mutex<Option<(u64, BuildRequest)>>,
    publication_gate: Mutex<()>,
    running: Mutex<bool>,
    /// The worker caches figures between builds. Clients do not see this as a build.
    warming: std::sync::atomic::AtomicBool,
    next_id: AtomicU64,
    events: broadcast::Sender<BuildEvent>,
    last: RwLock<Option<BuildResult>>,
    before: Option<BeforeBuild>,
}

impl ProjectBuilds {
    pub fn new(
        builder: Arc<Builder>,
        project_dir: &Path,
        main_file: &str,
        before: Option<BeforeBuild>,
    ) -> Arc<ProjectBuilds> {
        let (events, _) = broadcast::channel(64);
        let out_dir = project_dir.join(".galley").join("build");
        let last = std::fs::read(out_dir.join(LAST_RESULT_JSON))
            .ok()
            .and_then(|b| serde_json::from_slice::<BuildResult>(&b).ok())
            .or_else(|| {
                std::fs::read(out_dir.join(ERRORS_JSON))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Vec<Diagnostic>>(&b).ok())
                    .map(|errors| BuildResult {
                        id: 0,
                        status: if errors.iter().any(|d| d.level == Level::Error) {
                            BuildStatus::Failed
                        } else {
                            BuildStatus::Ok
                        },
                        error_count: errors.iter().filter(|d| d.level == Level::Error).count(),
                        warning_count: errors.iter().filter(|d| d.level == Level::Warning).count(),
                        errors,
                        profile: Profile::default(),
                        pages: None,
                        figures: None,
                        pending_figures: Vec::new(),
                        fetched_packages: false,
                        pdf_fresh: false,
                        pdf_available: out_dir.join(LAST_GOOD_PDF).exists(),
                        stale: false,
                        draft: false,
                        main_file: main_file.to_string(),
                        engine: "tectonic".into(),
                        engine_version: None,
                        pdf_engine: read_producer(&out_dir).map(|p| p.engine),
                        pdf_engine_version: read_producer(&out_dir).and_then(|p| p.version),
                        sandbox: builder.sandbox.kind.to_string(),
                        finished_at: std::fs::metadata(out_dir.join(ERRORS_JSON))
                            .and_then(|m| m.modified())
                            .map(DateTime::<Utc>::from)
                            .unwrap_or_else(|_| Utc::now()),
                        message: None,
                    })
            });
        let next_id = last
            .as_ref()
            .map_or(1, |result| result.id.saturating_add(1));
        Arc::new(ProjectBuilds {
            builder,
            project_dir: project_dir.to_path_buf(),
            main_file: RwLock::new(main_file.to_string()),
            pending: Mutex::new(None),
            publication_gate: Mutex::new(()),
            running: Mutex::new(false),
            warming: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(next_id),
            events,
            last: RwLock::new(last),
            before,
        })
    }

    /// The project's main file changed (a rename, or a new choice in settings).
    pub async fn set_main_file(&self, main_file: &str) {
        *self.main_file.write().await = main_file.to_string();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BuildEvent> {
        self.events.subscribe()
    }

    pub async fn last(&self) -> Option<BuildResult> {
        self.last.read().await.clone()
    }

    pub async fn is_running(&self) -> bool {
        *self.running.lock().await && !self.warming.load(Ordering::Relaxed)
    }

    pub fn out_dir(&self) -> PathBuf {
        self.project_dir.join(".galley").join("build")
    }

    /// Load SyncTeX for the PDF currently shown (the last good build).
    pub fn synctex(&self) -> Result<SyncTex> {
        SyncTex::load(&self.out_dir().join(LAST_GOOD_SYNCTEX), &self.project_dir)
    }

    /// Queue a build. This request replaces a request that has not started.
    pub async fn request(self: &Arc<Self>, req: BuildRequest) -> u64 {
        let _gate = self.publication_gate.lock().await;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut pending = self.pending.lock().await;
        if pending.as_ref().is_some_and(|(old_id, _)| *old_id > id) {
            let _ = self.events.send(BuildEvent::BuildSuperseded { id });
            return id;
        }
        if let Some((old_id, _)) = pending.replace((id, req)) {
            let _ = self.events.send(BuildEvent::BuildSuperseded { id: old_id });
        }
        let mut running = self.running.lock().await;
        if *running {
            return id;
        }
        *running = true;
        drop(running);
        drop(pending);
        drop(_gate);
        let me = Arc::clone(self);
        tokio::spawn(async move { me.worker().await });
        id
    }

    async fn worker(self: Arc<Self>) {
        loop {
            let mut pending = self.pending.lock().await;
            let Some((id, req)) = pending.take() else {
                *self.running.lock().await = false;
                return;
            };
            drop(pending);
            let _ = self.events.send(BuildEvent::BuildStarted {
                id,
                draft: req.draft,
                engine: req.engine,
            });
            if let Some(before) = &self.before {
                before().await;
            }
            let events = self.events.clone();
            let progress = move |message: String| {
                let _ = events.send(BuildEvent::BuildProgress { id, message });
            };
            let default_main = self.main_file.read().await.clone();
            // Compile the requested file if it is a safe .tex path in the project. If not, compile
            // the project's main file.
            let target = req
                .file
                .as_deref()
                .filter(|f| is_safe_target(&self.project_dir, f))
                .map(str::to_string)
                .unwrap_or(default_main);
            let latest = || self.next_id.load(Ordering::Relaxed) == id + 1;
            let mut result = self
                .builder
                .run_checked(
                    id,
                    &self.project_dir,
                    &target,
                    &req,
                    &progress,
                    RunControl {
                        latest: &latest,
                        publication_gate: Some(&self.publication_gate),
                    },
                )
                .await;
            result.stale |= self.pending.lock().await.is_some();
            let warm = std::mem::take(&mut result.pending_figures);
            persist_result(&self.out_dir(), &result);
            tracing::info!(project = %self.project_dir.display(), id, status = ?result.status, ms = result.profile.total_ms, "build finished");
            *self.last.write().await = Some(result.clone());
            let _ = self
                .events
                .send(BuildEvent::BuildFinished(Box::new(result)));
            if !warm.is_empty() {
                self.warming.store(true, Ordering::Relaxed);
                let made = self
                    .builder
                    .warm_figures(&self.project_dir, req.engine, warm, || async {
                        self.pending.lock().await.is_some()
                    })
                    .await;
                self.warming.store(false, Ordering::Relaxed);
                tracing::debug!(project = %self.project_dir.display(), made, "figures cached in the background");
            }
        }
    }
}

/// `Output written on main.pdf (9 pages, 123456 bytes).`
pub(crate) fn parse_pages(log: &str) -> Option<u32> {
    let i = log.rfind("Output written on ")?;
    let rest = &log[i..];
    let open = rest.find('(')?;
    let num: String = rest[open + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num.parse().ok()
}

fn own_failures(n: usize) -> String {
    if n == 1 {
        "1 figure fails to compile on its own".into()
    } else {
        format!("{n} figures fail to compile on their own")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_a_spawned_descendant() {
        let project = tempfile::tempdir().unwrap();
        let out_dir = project.path().join(".galley/build");
        std::fs::create_dir_all(&out_dir).unwrap();
        let started = out_dir.join("child-started");
        let survived = out_dir.join("child-survived");
        let spec = crate::engine::CompileSpec {
            spec: crate::sandbox::Spec {
                project_dir: project.path().to_path_buf(),
                out_dir: out_dir.clone(),
                mounts: Vec::new(),
                masked: Vec::new(),
                env: Vec::new(),
                network: false,
                program: PathBuf::from("/bin/sh"),
                args: vec![
                    "-c".into(),
                    "(printf ready > \"$1\"; sleep 1; printf escaped > \"$2\") & wait".into(),
                    "galley-test".into(),
                    started.to_string_lossy().into_owned(),
                    survived.to_string_lossy().into_owned(),
                ],
                name: "galley-timeout-test".into(),
            },
            output_dir: out_dir,
            stem: "main".into(),
        };
        let run = one_slot_builder()
            .run_spec_limited(spec, Duration::from_millis(300))
            .await
            .unwrap();
        assert!(run.timed_out);
        assert!(started.exists(), "the descendant must start before timeout");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(
            !survived.exists(),
            "a descendant survived the process-group kill"
        );
    }

    fn one_slot_builder() -> Builder {
        let sandbox = crate::sandbox::Sandbox {
            kind: crate::sandbox::SandboxKind::None,
            docker_image: String::new(),
            memory_mb: 512,
            cpus: 1.0,
            systemd_scope: false,
        };
        let dir = std::env::temp_dir();
        Builder::new(
            sandbox,
            Hints::bundled().unwrap(),
            Duration::from_secs(5),
            &dir,
            None,
            1,
        )
    }

    #[tokio::test]
    async fn a_waiting_build_is_told_its_place_and_starts_when_a_slot_frees() {
        let b = std::sync::Arc::new(one_slot_builder());
        let running = b.slots.try_acquire().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        // Two builds queue behind the running one.
        let (b1, s1) = (b.clone(), seen.clone());
        let first = tokio::spawn(async move {
            let log = move |m: String| s1.lock().unwrap().push(format!("first: {m}"));
            b1.wait_for_slot(&log).await.map(|_| ())
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (b2, s2) = (b.clone(), seen.clone());
        let second = tokio::spawn(async move {
            let log = move |m: String| s2.lock().unwrap().push(format!("second: {m}"));
            b2.wait_for_slot(&log).await.map(|_| ())
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        {
            let seen = seen.lock().unwrap();
            assert!(
                seen.iter()
                    .any(|m| m == "first: Next in line. Starting in a moment…"),
                "{seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|m| m == "second: In line: 1 build ahead of yours…"),
                "{seen:?}"
            );
        }
        drop(running);
        assert!(first.await.unwrap().is_some());
        assert!(second.await.unwrap().is_some());
    }

    #[test]
    fn resolves_log_file_names() {
        let files = vec![
            "main.tex".to_string(),
            "sections/method.tex".to_string(),
            "refs.bib".to_string(),
        ];
        let root = Path::new("/tmp/project");
        assert_eq!(
            resolve_file("sections/method", root, "main.tex", &files),
            "sections/method.tex"
        );
        assert_eq!(
            resolve_file("./main.tex", root, "main.tex", &files),
            "main.tex"
        );
        assert_eq!(
            resolve_file(".galley/build/galley-draft.tex", root, "main.tex", &files),
            "main.tex"
        );
        assert_eq!(
            resolve_file("article.cls", root, "main.tex", &files),
            "article.cls"
        );
        assert_eq!(
            resolve_file("/tmp/project/sections/method.tex", root, "main.tex", &files),
            "sections/method.tex"
        );
    }

    #[tokio::test]
    async fn pending_builds_emit_superseded_with_enqueue_ids() {
        let project = tempfile::tempdir().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let before: BeforeBuild = {
            let entered = entered.clone();
            let release = release.clone();
            let calls = calls.clone();
            Arc::new(move || {
                let entered = entered.clone();
                let release = release.clone();
                let calls = calls.clone();
                Box::pin(async move {
                    if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                        entered.notify_one();
                        release.notified().await;
                    }
                })
            })
        };
        let builds = ProjectBuilds::new(
            Arc::new(one_slot_builder()),
            project.path(),
            "missing.tex",
            Some(before),
        );
        let mut events = builds.subscribe();
        let first = builds.request(BuildRequest::default()).await;
        entered.notified().await;
        let second = builds.request(BuildRequest::default()).await;
        let third = builds.request(BuildRequest::default()).await;
        assert_eq!((first, second, third), (1, 2, 3));
        let mut superseded = false;
        while !superseded {
            if let BuildEvent::BuildSuperseded { id } = events.recv().await.unwrap() {
                assert_eq!(id, second);
                superseded = true;
            }
        }
        release.notify_one();
        let mut finished = Vec::new();
        while finished.len() < 2 {
            if let BuildEvent::BuildFinished(result) = events.recv().await.unwrap() {
                finished.push(result.id);
            }
        }
        assert_eq!(finished, vec![first, third]);
    }

    #[test]
    fn publication_switches_pdf_and_synctex_as_one_generation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let first_pdf = root.join("first.pdf");
        let second_pdf = root.join("second.pdf");
        let first_sync = root.join("first.synctex.gz");
        std::fs::write(&first_pdf, b"first").unwrap();
        std::fs::write(&second_pdf, b"second").unwrap();
        std::fs::write(&first_sync, b"sync-first").unwrap();
        let mut result = BuildResult {
            id: 1,
            status: BuildStatus::Ok,
            errors: Vec::new(),
            error_count: 0,
            warning_count: 0,
            profile: Profile::default(),
            pages: Some(1),
            figures: None,
            pending_figures: Vec::new(),
            fetched_packages: false,
            pdf_fresh: true,
            pdf_available: true,
            stale: false,
            draft: false,
            main_file: "main.tex".into(),
            engine: "pdflatex".into(),
            engine_version: Some("one".into()),
            pdf_engine: Some("pdflatex".into()),
            pdf_engine_version: Some("one".into()),
            sandbox: "bwrap".into(),
            finished_at: Utc::now(),
            message: None,
        };
        publish_pdf(root, &first_pdf, Some(&first_sync), &result).unwrap();
        assert_eq!(std::fs::read(root.join(LAST_GOOD_PDF)).unwrap(), b"first");
        assert_eq!(
            std::fs::read(root.join(LAST_GOOD_SYNCTEX)).unwrap(),
            b"sync-first"
        );
        result.id = 2;
        result.engine = "xelatex".into();
        result.engine_version = Some("two".into());
        publish_pdf(root, &second_pdf, None, &result).unwrap();
        assert_eq!(std::fs::read(root.join(LAST_GOOD_PDF)).unwrap(), b"second");
        assert!(!root.join(LAST_GOOD_SYNCTEX).exists());
        let producer = read_producer(root).unwrap();
        assert_eq!(
            (producer.engine.as_str(), producer.version.as_deref()),
            ("xelatex", Some("two"))
        );
    }

    #[test]
    fn fetch_only_for_bundle_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = |m: &str| RawDiag {
            level: Level::Error,
            file: None,
            line: None,
            message: m.into(),
            context: String::new(),
            raw: String::new(),
        };
        assert!(needs_fetch(
            &[d("LaTeX Error: File `size10.clo' not found")],
            dir.path()
        ));
        assert!(needs_fetch(
            &[d("LaTeX Error: File `tikz.sty' not found")],
            dir.path()
        ));
        assert!(needs_fetch(
            &[d(
                "Package tikz Error: I did not find the tikz library 'external'."
            )],
            dir.path()
        ));
        assert!(!needs_fetch(
            &[d("LaTeX Error: File `missingfig' not found")],
            dir.path()
        ));
        assert!(!needs_fetch(
            &[d("LaTeX Error: File `sections/x.tex' not found")],
            dir.path()
        ));
        std::fs::write(dir.path().join("local.sty"), "").unwrap();
        assert!(!needs_fetch(
            &[d("LaTeX Error: File `local.sty' not found")],
            dir.path()
        ));
    }

    #[test]
    fn converter_failure_names_the_failed_tool() {
        let log = "Latexmk: applying rule 'ps2pdf'...\n  ps2pdf: Command for 'ps2pdf' gave return code 256\n";
        assert!(auxiliary_failure(log).unwrap().contains("ps2pdf"));
    }

    #[test]
    fn texlive_file_line_errors_include_package_files() {
        let raw = parse_file_line_errors(
            "/tmp/texmf/l3backend-xetex.def:803: LaTeX Error: Control sequence already defined.\n",
        );
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].line, Some(803));
        assert_eq!(
            raw[0].file.as_deref(),
            Some("/tmp/texmf/l3backend-xetex.def")
        );
    }
}
