//! The build queue for one project. The newest request wins. Each job runs the flush hook, then a
//! sandboxed compile, then an optional package fetch pass, then the log parse, then the artifacts.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::engine::Tectonic;
use crate::figures::{self, FigCache, FigureStats};
use crate::hints::{Hints, ProjectView};
use crate::log::{parse_log, Diagnostic, Level, RawDiag};
use crate::sandbox::Sandbox;
use crate::synctex::SyncTex;
use crate::{install, Result};

#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildRequest {
    pub draft: bool,
    /// The file to compile. The build uses the project's main file when this is None. This lets
    /// the editor build the document that the user reads.
    pub file: Option<String>,
    /// Lint rules switched off for this project (SPEC §13.5).
    pub lint_disabled: Vec<String>,
    /// Use the persistent figure cache when the project is eligible (SPEC §13.1).
    pub figure_cache: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    Ok,
    Failed,
    Timeout,
    /// The engine could not run at all (missing binary, sandbox failure).
    Error,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Profile {
    pub total_ms: u64,
    pub compile_ms: u64,
    pub fetch_ms: u64,
    /// Time spent compiling figures on their own during this build.
    pub figure_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
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
    pub sandbox: String,
    pub finished_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuildEvent {
    BuildStarted { id: u64, draft: bool },
    BuildProgress { id: u64, message: String },
    /// Boxed: a result carries every diagnostic, and events are cloned per subscriber.
    BuildFinished(Box<BuildResult>),
}

pub const LAST_GOOD_PDF: &str = "last-good.pdf";
pub const LAST_GOOD_SYNCTEX: &str = "last-good.synctex.gz";
pub const ERRORS_JSON: &str = "errors.json";
pub const LAST_LOG: &str = "last.log";
pub const LAST_DIFF_PDF: &str = "last-diff.pdf";

pub struct Builder {
    pub sandbox: Sandbox,
    pub hints: Hints,
    pub timeout: Duration,
    engine: RwLock<Option<Tectonic>>,
    data_dir: PathBuf,
    configured_path: Option<String>,
}

impl Builder {
    pub fn new(sandbox: Sandbox, hints: Hints, timeout: Duration, data_dir: &Path, configured_path: Option<String>) -> Builder {
        let engine = Tectonic::locate(data_dir, configured_path.as_deref());
        Builder {
            sandbox,
            hints,
            timeout,
            engine: RwLock::new(engine),
            data_dir: data_dir.to_path_buf(),
            configured_path,
        }
    }

    pub async fn engine_path(&self) -> Option<PathBuf> {
        self.engine.read().await.as_ref().map(|e| e.binary.clone())
    }

    /// Install Tectonic on first use. `progress` feeds the build bar.
    pub(crate) async fn ensure_engine(&self, progress: &(dyn Fn(String) + Send + Sync)) -> Result<Tectonic> {
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
        let found = Tectonic::locate(&self.data_dir, None)
            .ok_or_else(|| crate::Error::Engine("Tectonic was downloaded but cannot be found".into()))?;
        *slot = Some(found.clone());
        Ok(found)
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
        let started = Instant::now();
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
            engine: "tectonic".into(),
            sandbox: self.sandbox.kind.to_string(),
            finished_at: Utc::now(),
            message: None,
        };

        if !project_dir.join(main_file).is_file() {
            result.message = Some(format!("{main_file} does not exist. Create it, or pick another main file in project settings."));
            return self.finish(result, started, &out_dir);
        }
        let engine = match self.ensure_engine(progress).await {
            Ok(e) => e,
            Err(e) => {
                result.message = Some(e.to_string());
                return self.finish(result, started, &out_dir);
            }
        };

        let main_text = std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default();
        let files = list_text_files(project_dir);
        let svg_problems = {
            let dir = project_dir.to_path_buf();
            tokio::task::spawn_blocking(move || crate::svg::prepare(&dir)).await.unwrap_or_default()
        };

        // The first build on a host has no Tectonic bundle in the cache. An offline pass tries to
        // initialise the read-only cache and fails with EROFS. Download the bundle with the
        // network on for this build. Later builds compile offline against the warm cache.
        let cold = cache_cold(&engine);
        progress(if cold { "Fetching packages…" } else { "Compiling…" }.into());
        let t0 = Instant::now();

        // The figure cache runs first when it can. It returns a run to publish, or the reason it
        // could not. In the second case the plain compile below takes over.
        let tex: Vec<(String, String)> = files
            .iter()
            .filter(|f| f.ends_with(".tex"))
            .filter_map(|f| std::fs::read_to_string(project_dir.join(f)).ok().map(|t| (f.clone(), t)))
            .collect();
        let eligibility = if req.draft { Err(String::new()) } else { figures::eligibility(&main_text, &tex) };
        let has_pictures = !matches!(&eligibility, Err(why) if why.is_empty() || why == figures::NO_PICTURES);
        let mut use_cache = false;
        if has_pictures {
            result.figures = match (&eligibility, req.figure_cache) {
                (_, false) => Some(FigureStats::off("turned off for this project")),
                (Err(why), true) => Some(FigureStats::off(why)),
                (Ok(()), true) if cold => Some(FigureStats::off("the first build fetches packages")),
                (Ok(()), true) => {
                    use_cache = true;
                    None
                }
            };
        }
        let mut cached_run = None;
        if use_cache {
            let pass = self.figure_pass(&engine, project_dir, main_file, &main_text, progress).await;
            result.profile.figure_ms = pass.figure_ms;
            result.figures = Some(pass.stats);
            result.pending_figures = pass.pending;
            cached_run = pass.run;
        }

        let mut run = match cached_run {
            Some(r) => r,
            None => {
                let tp = Instant::now();
                let r = match self.compile_once(&engine, project_dir, main_file, req.draft, cold).await {
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
        // with network access so the engine fills the shared cache.
        if needs_fetch(&run.raw, project_dir) {
            progress("Fetching packages…".into());
            let t1 = Instant::now();
            match self.compile_once(&engine, project_dir, main_file, req.draft, true).await {
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
                result.message = Some("Fetching packages took too long. Check the network and build again.".into());
                return self.finish(result, started, &out_dir);
            }
        }

        result.pages = parse_pages(&run.log);
        let view = ProjectView { main_file, main_text: &main_text };
        let mut diags: Vec<Diagnostic> = run
            .raw
            .into_iter()
            .map(|mut d| {
                d.file = d.file.as_deref().map(|f| resolve_file(f, main_file, &files));
                self.hints.annotate(d, &view)
            })
            .collect();
        if diags.is_empty() && !run.exit_ok {
            diags.push(Diagnostic {
                level: Level::Error,
                file: None,
                line: None,
                code: "engine-failed".into(),
                message: run.stderr_summary.clone().unwrap_or_else(|| "The engine exited with an error".into()),
                hint: Some("The log has no LaTeX error; the raw output below may explain it.".into()),
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
            .filter_map(|f| std::fs::read_to_string(project_dir.join(f)).ok().map(|t| (f.clone(), t)))
            .collect();
        diags.extend(svg_problems);
        diags.extend(crate::lint::lint(&texts, &req.lint_disabled));
        // The bibliography checks come from the paper index, which holds the citations of the
        // document. The lint rule named `uncited` reports that issue, so nothing is reported twice.
        let paper = galley_index::scan(project_dir, main_file);
        for f in galley_index::bib::audit(&paper) {
            if f.issue == galley_index::bib::Issue::Uncited || req.lint_disabled.iter().any(|d| d == f.issue.code()) {
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

        let pdf = out_dir.join(format!("{}.pdf", run.stem));
        if run.exit_ok && result.error_count == 0 && pdf.exists() {
            let promote = || -> std::io::Result<()> {
                std::fs::copy(&pdf, out_dir.join(LAST_GOOD_PDF))?;
                let st = out_dir.join(format!("{}.synctex.gz", run.stem));
                if st.exists() {
                    std::fs::copy(st, out_dir.join(LAST_GOOD_SYNCTEX))?;
                }
                Ok(())
            };
            match promote() {
                Ok(()) => {
                    result.status = BuildStatus::Ok;
                    result.pdf_fresh = true;
                    result.pdf_available = true;
                }
                Err(e) => {
                    result.status = BuildStatus::Failed;
                    result.message = Some(format!("The PDF was produced but could not be saved: {e}"));
                }
            }
        } else {
            result.status = BuildStatus::Failed;
        }
        let _ = std::fs::write(out_dir.join(LAST_LOG), &run.log);
        self.finish(result, started, &out_dir)
    }

    /// Figure cache step (SPEC §13.1). Returns a run to publish only when every figure PDF is
    /// present and built from the current picture code.
    async fn figure_pass(
        &self,
        engine: &Tectonic,
        project_dir: &Path,
        main_file: &str,
        main_text: &str,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> FigurePass {
        let out_dir = project_dir.join(".galley").join("build");
        let mut cache = FigCache::load(&out_dir);
        let key = figures::project_key(project_dir, main_text, &engine_id(engine));
        if cache.key != key {
            cache.figures.clear();
            cache.failed.clear();
            cache.key = key;
        }
        let plain = |cache: &FigCache, stats: FigureStats, pending: Vec<String>, figure_ms: u64| {
            cache.save(&out_dir);
            FigurePass { run: None, stats, pending, figure_ms }
        };
        let bypass = |why: &str| FigureStats { mode: "plain".into(), reason: Some(why.to_string()), ..Default::default() };

        let t = Instant::now();
        let Ok(run) = self.wrapper_once(engine, project_dir, main_file).await else {
            return plain(&cache, bypass("the cached pass could not start"), Vec::new(), 0);
        };
        cache.wrapper_ms = t.elapsed().as_millis() as u64;
        if run.timed_out || needs_fetch(&run.raw, project_dir) {
            return plain(&cache, bypass("packages had to be fetched first"), Vec::new(), 0);
        }
        let names = figures::figure_list(&out_dir);
        if names.is_empty() {
            return plain(&cache, bypass("TikZ is not loaded by the document"), Vec::new(), 0);
        }
        let todo = figures::stale(&out_dir, &cache, &names);
        let blocked = todo
            .iter()
            .filter(|n| cache.failed.get(*n).is_some_and(|m| Some(m) == figures::current_md5(&out_dir, n).as_ref()))
            .count();
        let total = names.len();
        let stats = |cached: usize, pending: usize, mode: &str, reason: Option<String>| FigureStats {
            mode: mode.into(),
            total,
            cached,
            pending,
            reason,
        };
        if blocked > 0 {
            return plain(&cache, stats(total - todo.len(), 0, "plain", Some(own_failures(blocked))), Vec::new(), 0);
        }
        if todo.is_empty() {
            cache.save(&out_dir);
            return FigurePass { run: Some(run), stats: stats(total, 0, "cached", None), pending: Vec::new(), figure_ms: 0 };
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
            return plain(&cache, stats(total - todo.len(), todo.len(), "plain", None), pending, 0);
        }

        progress(format!("Caching {} figure{}…", todo.len(), if todo.len() == 1 { "" } else { "s" }));
        let t = Instant::now();
        let failed = self.make_figures(engine, project_dir, &todo, &mut cache).await;
        let figure_ms = t.elapsed().as_millis() as u64;
        if failed > 0 {
            return plain(&cache, stats(total - todo.len(), 0, "plain", Some(own_failures(failed))), Vec::new(), figure_ms);
        }
        progress("Compiling…".into());
        let Ok(run) = self.wrapper_once(engine, project_dir, main_file).await else {
            return plain(&cache, bypass("the cached pass could not start"), Vec::new(), figure_ms);
        };
        let names = figures::figure_list(&out_dir);
        let left = figures::stale(&out_dir, &cache, &names);
        if run.timed_out || !left.is_empty() {
            let why = Some("figures changed during the build".to_string());
            return plain(&cache, stats(names.len() - left.len(), 0, "plain", why), Vec::new(), figure_ms);
        }
        cache.save(&out_dir);
        FigurePass { run: Some(run), stats: stats(names.len(), 0, "cached", None), pending: Vec::new(), figure_ms }
    }

    async fn wrapper_once(&self, engine: &Tectonic, project_dir: &Path, main_file: &str) -> Result<RunOutput> {
        let out_dir = project_dir.join(".galley").join("build");
        std::fs::create_dir_all(&out_dir)?;
        let file = format!("{}.tex", figures::WRAPPER_STEM);
        std::fs::write(out_dir.join(&file), figures::wrapper_source(main_file))?;
        let spec = engine.compile_generated(&self.sandbox, project_dir, &file, figures::WRAPPER_STEM, false)?;
        self.run_spec(spec).await
    }

    /// Compile `names` on their own, `figure_parallelism()` at a time, recording which picture
    /// code each PDF was built from. Returns how many failed.
    async fn make_figures(&self, engine: &Tectonic, project_dir: &Path, names: &[String], cache: &mut FigCache) -> usize {
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
                    .and_then(|_| engine.compile_generated(&self.sandbox, project_dir, &file, n, false));
                jobs.push(async move {
                    let ok = match spec {
                        Ok(spec) => self.run_spec(spec).await.map(|r| r.exit_ok && !r.timed_out).unwrap_or(false),
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
    pub async fn warm_figures<F, Fut>(&self, project_dir: &Path, names: Vec<String>, stop: F) -> usize
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let Ok(engine) = self.ensure_engine(&|_| {}).await else { return 0 };
        let out_dir = project_dir.join(".galley").join("build");
        let mut cache = FigCache::load(&out_dir);
        let mut made = 0;
        for chunk in names.chunks(figure_parallelism()) {
            if stop().await {
                break;
            }
            let failed = self.make_figures(&engine, project_dir, chunk, &mut cache).await;
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
        result
    }

    async fn compile_once(&self, engine: &Tectonic, project_dir: &Path, main_file: &str, draft: bool, fetch: bool) -> Result<RunOutput> {
        let compile = engine.compile(&self.sandbox, project_dir, main_file, draft, fetch, self.timeout.as_secs())?;
        self.run_spec(compile).await
    }

    /// Run one prepared compile spec. The normal build and the latexdiff compare both use it.
    pub(crate) async fn run_spec(&self, compile: crate::engine::CompileSpec) -> Result<RunOutput> {
        let out_dir = compile.spec.out_dir.clone();
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
        let (exit_ok, timed_out, stderr) = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => (out.status.success(), false, String::from_utf8_lossy(&out.stderr).into_owned()),
            Ok(Err(e)) => return Err(crate::Error::Engine(format!("the engine could not be run: {e}"))),
            Err(_) => {
                self.sandbox.kill(&compile.spec).await;
                (false, true, String::new())
            }
        };
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let raw = parse_log(&log);
        let stderr_summary = stderr
            .lines()
            .find_map(|l| l.strip_prefix("error: "))
            .map(|s| s.trim_end_matches('.').to_string());
        Ok(RunOutput {
            stem: compile.stem,
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
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<()> {
        let bin = latexdiff_bin().ok_or(crate::Error::LatexdiffUnavailable)?;
        let engine = self.ensure_engine(progress).await?;
        let out_dir = project_dir.join(".galley").join("build");
        std::fs::create_dir_all(&out_dir)?;
        let old_path = out_dir.join("galley-diff-old.tex");
        let new_path = out_dir.join("galley-diff-new.tex");
        std::fs::write(&old_path, old_text)?;
        std::fs::write(&new_path, new_text)?;

        progress("Marking up changes…".into());
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&bin).arg(&old_path).arg(&new_path).output()
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
        let cold = cache_cold(&engine);
        let spec = engine.compile_generated(&self.sandbox, project_dir, "galley-diff.tex", "galley-diff", cold)?;
        let mut run = self.run_spec(spec).await?;
        if needs_fetch(&run.raw, project_dir) {
            let spec = engine.compile_generated(&self.sandbox, project_dir, "galley-diff.tex", "galley-diff", true)?;
            run = self.run_spec(spec).await?;
        }
        let _ = run;
        let pdf = out_dir.join("galley-diff.pdf");
        if pdf.is_file() {
            std::fs::copy(&pdf, out_dir.join(LAST_DIFF_PDF))?;
            Ok(())
        } else {
            Err(crate::Error::Engine(
                "the comparison did not produce a PDF; the marked-up document may not compile".into(),
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
    std::env::split_paths(&path).map(|d| d.join("latexdiff")).find(|p| p.is_file())
}

pub(crate) struct FigurePass {
    run: Option<RunOutput>,
    stats: FigureStats,
    pending: Vec<String>,
    figure_ms: u64,
}

/// Figure jobs are whole-document passes. More than a few at the same time overloads a small host.
fn figure_parallelism() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 4)
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
pub(crate) fn cache_cold(engine: &Tectonic) -> bool {
    match std::fs::read_dir(&engine.cache_dir) {
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
        font_missing
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
    if Path::new(file).components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return false;
    }
    project_dir.join(file).is_file()
}

fn resolve_file(name: &str, main_file: &str, files: &[String]) -> String {
    let name = name.trim_start_matches("./");
    if name.ends_with("galley-draft.tex") || name.ends_with("galley-draft") || name.ends_with("galley-fig.tex") {
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
        let Ok(rd) = std::fs::read_dir(dir) else { return };
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
    pending: Mutex<Option<BuildRequest>>,
    running: Mutex<bool>,
    /// The worker caches figures between builds. Clients do not see this as a build.
    warming: std::sync::atomic::AtomicBool,
    next_id: AtomicU64,
    events: broadcast::Sender<BuildEvent>,
    last: RwLock<Option<BuildResult>>,
    before: Option<BeforeBuild>,
}

impl ProjectBuilds {
    pub fn new(builder: Arc<Builder>, project_dir: &Path, main_file: &str, before: Option<BeforeBuild>) -> Arc<ProjectBuilds> {
        let (events, _) = broadcast::channel(64);
        let out_dir = project_dir.join(".galley").join("build");
        let last = std::fs::read(out_dir.join(ERRORS_JSON))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Diagnostic>>(&b).ok())
            .map(|errors| BuildResult {
                id: 0,
                status: if errors.iter().any(|d| d.level == Level::Error) { BuildStatus::Failed } else { BuildStatus::Ok },
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
                sandbox: builder.sandbox.kind.to_string(),
                finished_at: std::fs::metadata(out_dir.join(ERRORS_JSON))
                    .and_then(|m| m.modified())
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now()),
                message: None,
            });
        Arc::new(ProjectBuilds {
            builder,
            project_dir: project_dir.to_path_buf(),
            main_file: RwLock::new(main_file.to_string()),
            pending: Mutex::new(None),
            running: Mutex::new(false),
            warming: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
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
    pub async fn request(self: &Arc<Self>, req: BuildRequest) {
        *self.pending.lock().await = Some(req);
        let mut running = self.running.lock().await;
        if *running {
            return;
        }
        *running = true;
        drop(running);
        let me = Arc::clone(self);
        tokio::spawn(async move { me.worker().await });
    }

    async fn worker(self: Arc<Self>) {
        loop {
            let Some(req) = self.pending.lock().await.take() else {
                *self.running.lock().await = false;
                return;
            };
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let _ = self.events.send(BuildEvent::BuildStarted { id, draft: req.draft });
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
            let mut result = self.builder.run(id, &self.project_dir, &target, &req, &progress).await;
            result.stale = self.pending.lock().await.is_some();
            let warm = std::mem::take(&mut result.pending_figures);
            tracing::info!(project = %self.project_dir.display(), id, status = ?result.status, ms = result.profile.total_ms, "build finished");
            *self.last.write().await = Some(result.clone());
            let _ = self.events.send(BuildEvent::BuildFinished(Box::new(result)));
            if !warm.is_empty() {
                self.warming.store(true, Ordering::Relaxed);
                let made = self
                    .builder
                    .warm_figures(&self.project_dir, warm, || async { self.pending.lock().await.is_some() })
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
    let num: String = rest[open + 1..].chars().take_while(|c| c.is_ascii_digit()).collect();
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

    #[test]
    fn resolves_log_file_names() {
        let files = vec!["main.tex".to_string(), "sections/method.tex".to_string(), "refs.bib".to_string()];
        assert_eq!(resolve_file("sections/method", "main.tex", &files), "sections/method.tex");
        assert_eq!(resolve_file("./main.tex", "main.tex", &files), "main.tex");
        assert_eq!(resolve_file(".galley/build/galley-draft.tex", "main.tex", &files), "main.tex");
        assert_eq!(resolve_file("article.cls", "main.tex", &files), "article.cls");
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
        assert!(needs_fetch(&[d("LaTeX Error: File `size10.clo' not found")], dir.path()));
        assert!(needs_fetch(&[d("LaTeX Error: File `tikz.sty' not found")], dir.path()));
        assert!(!needs_fetch(&[d("LaTeX Error: File `missingfig' not found")], dir.path()));
        assert!(!needs_fetch(&[d("LaTeX Error: File `sections/x.tex' not found")], dir.path()));
        std::fs::write(dir.path().join("local.sty"), "").unwrap();
        assert!(!needs_fetch(&[d("LaTeX Error: File `local.sty' not found")], dir.path()));
    }
}
