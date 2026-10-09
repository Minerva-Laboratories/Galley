//! Compiles on build workers. The machine that holds the documents needs little CPU, while
//! compiles need a lot of it in bursts. A worker is the same binary started with `galley worker`.
//! It runs the same `Builder` and sandbox on a copy of the sources and sends back the PDF, the
//! SyncTeX file, the log and the result. The server keeps its queue, its latest-wins rule and the
//! publication of the PDF, so a remote build looks the same to everything above it.
//!
//! A worker keeps a copy of each project it has built, including the build directory, so the next
//! build of that project there reuses its auxiliary files and figure cache.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::runner::{BuildRequest, BuildResult};

/// The request header that carries the job. The body is the sources as a gzipped tar.
pub const JOB_HEADER: &str = "x-galley-job";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerJob {
    pub id: u64,
    /// The project id. The worker keeps a copy per project under this name.
    pub project: String,
    pub main_file: String,
    pub request: BuildRequest,
}

/// What a worker sends back. The PDF and SyncTeX come only with a build that published a new PDF.
#[derive(Debug, Default)]
pub struct Outcome {
    pub result: Option<BuildResult>,
    pub pdf: Option<Vec<u8>>,
    pub synctex: Option<Vec<u8>>,
    pub log: Option<Vec<u8>>,
}

const RESULT: &str = "result.json";
const PDF: &str = "paper.pdf";
const SYNCTEX: &str = "paper.synctex.gz";
const LOG: &str = "build.log";

pub struct RemoteWorker {
    url: String,
    token: String,
    http: reqwest::Client,
}

impl RemoteWorker {
    pub fn new(url: &str, token: &str) -> RemoteWorker {
        RemoteWorker {
            url: format!("{}/compile", url.trim_end_matches('/')),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Send one build. `limit` covers the whole exchange: a worker that started from a stop, the
    /// compile, and the reply.
    pub async fn compile(&self, job: &WorkerJob, sources: Vec<u8>, limit: Duration) -> Result<Outcome, String> {
        let header = serde_json::to_string(job).map_err(|e| e.to_string())?;
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .header(JOB_HEADER, header)
            .header(reqwest::header::CONTENT_TYPE, "application/gzip")
            .body(sources)
            .timeout(limit)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "The build worker did not answer in time".to_string()
                } else {
                    "The build worker could not be reached".to_string()
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("The build worker refused the build (HTTP {status}): {}", body.trim()));
        }
        let bytes = response.bytes().await.map_err(|_| "The build worker's reply was cut off".to_string())?;
        unpack_outcome(&bytes).map_err(|e| format!("The build worker's reply was unreadable: {e}"))
    }
}

/// Skip what a build makes or what history holds. The worker builds from the files alone.
fn skip_source(rel: &Path) -> bool {
    let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
    first == ".git" || first == ".galley" || rel.to_string_lossy().ends_with(".galley-tmp")
}

/// The project's files, without git history or Galley's own state, as a gzipped tar.
pub fn pack_sources(project_dir: &Path) -> std::io::Result<Vec<u8>> {
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    tar.follow_symlinks(false);
    add_sources(&mut tar, project_dir, project_dir)?;
    tar.into_inner()?.finish()
}

fn add_sources<W: std::io::Write>(tar: &mut tar::Builder<W>, root: &Path, dir: &Path) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(std::io::Error::other)?;
        if skip_source(rel) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            tar.append_dir(rel, &path)?;
            add_sources(tar, root, &path)?;
        } else if kind.is_file() {
            match std::fs::File::open(&path) {
                Ok(mut f) => tar.append_file(rel, &mut f)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// Replace the sources in `dest` with the ones in the archive. `.galley`, which holds the build
/// directory and figure cache of earlier builds, is kept.
pub fn unpack_sources(bytes: &[u8], dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(dest)? {
        let entry = entry?;
        if entry.file_name() == ".galley" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.into_owned();
        // The sender never packs these. A crafted archive must not plant build state either.
        if skip_source(&rel) {
            continue;
        }
        // `unpack_in` refuses paths that would land outside `dest`.
        entry.unpack_in(dest)?;
    }
    Ok(())
}

/// The worker's reply: the result, the log, and the PDF and SyncTeX when the build published one.
pub fn pack_outcome(result: &BuildResult, out_dir: &Path) -> std::io::Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |name: &str, bytes: &[u8]| -> std::io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes)
    };
    add(RESULT, &serde_json::to_vec(result).map_err(std::io::Error::other)?)?;
    if let Ok(log) = std::fs::read(out_dir.join(crate::runner::LAST_LOG)) {
        add(LOG, &log)?;
    }
    if result.pdf_fresh {
        add(PDF, &std::fs::read(out_dir.join(crate::runner::LAST_GOOD_PDF))?)?;
        if let Ok(st) = std::fs::read(out_dir.join(crate::runner::LAST_GOOD_SYNCTEX)) {
            add(SYNCTEX, &st)?;
        }
    }
    tar.into_inner()
}

pub fn unpack_outcome(bytes: &[u8]) -> std::io::Result<Outcome> {
    let mut out = Outcome::default();
    let mut archive = tar::Archive::new(bytes);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        match name.as_str() {
            RESULT => out.result = Some(serde_json::from_slice(&buf).map_err(std::io::Error::other)?),
            PDF => out.pdf = Some(buf),
            SYNCTEX => out.synctex = Some(buf),
            LOG => out.log = Some(buf),
            _ => {}
        }
    }
    if out.result.is_none() {
        return Err(std::io::Error::other("no result in the reply"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_travel_without_history_or_build_state_and_keep_the_workers_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        for d in [".git/objects", ".galley/build", "sections"] {
            std::fs::create_dir_all(src.join(d)).unwrap();
        }
        std::fs::write(src.join("main.tex"), "\\input{sections/intro}").unwrap();
        std::fs::write(src.join("sections/intro.tex"), "Lorem ipsum").unwrap();
        std::fs::write(src.join(".git/objects/x"), "history").unwrap();
        std::fs::write(src.join(".galley/build/main.aux"), "server aux").unwrap();

        let packed = pack_sources(&src).unwrap();

        let dest = tmp.path().join("worker");
        std::fs::create_dir_all(dest.join(".galley/build")).unwrap();
        std::fs::write(dest.join(".galley/build/main.aux"), "worker aux").unwrap();
        std::fs::write(dest.join("deleted.tex"), "a file the author removed").unwrap();
        unpack_sources(&packed, &dest).unwrap();

        assert_eq!(std::fs::read_to_string(dest.join("sections/intro.tex")).unwrap(), "Lorem ipsum");
        assert!(!dest.join(".git").exists());
        assert!(!dest.join("deleted.tex").exists(), "a removed file must not linger on the worker");
        assert_eq!(
            std::fs::read_to_string(dest.join(".galley/build/main.aux")).unwrap(),
            "worker aux",
            "the worker keeps its own build state"
        );
    }

    #[test]
    fn a_reply_without_a_new_pdf_carries_no_pdf() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(crate::runner::LAST_LOG), "! Undefined control sequence.").unwrap();
        std::fs::write(tmp.path().join(crate::runner::LAST_GOOD_PDF), "an older pdf").unwrap();
        let result: BuildResult = serde_json::from_value(serde_json::json!({
            "id": 7, "status": "failed", "errors": [], "error_count": 1, "warning_count": 0,
            "profile": {"total_ms": 1, "compile_ms": 1, "fetch_ms": 0, "figure_ms": 0},
            "pages": null, "pending_figures": [], "fetched_packages": false, "pdf_fresh": false,
            "pdf_available": true, "stale": false, "draft": false, "main_file": "main.tex",
            "engine": "pdflatex", "sandbox": "bwrap", "finished_at": "2026-10-08T00:00:00Z"
        }))
        .unwrap();
        let out = unpack_outcome(&pack_outcome(&result, tmp.path()).unwrap()).unwrap();
        assert_eq!(out.result.unwrap().id, 7);
        assert!(out.pdf.is_none(), "an older PDF on the worker must never replace the server's");
        assert_eq!(out.log.unwrap(), b"! Undefined control sequence.");
    }
}
