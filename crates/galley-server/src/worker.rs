//! `galley worker`: a build worker. It accepts compiles from a Galley server that shares its token
//! and nothing else. It holds no accounts and no documents beyond its build copies, so any number
//! of them can start and stop as load demands.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use galley_build::remote::{pack_outcome, unpack_sources, WorkerJob, JOB_HEADER};
use galley_build::Builder;
use tokio::sync::Mutex;

use crate::config::Config;

/// Sources arrive compressed. The largest project a server accepts fits well within this.
const MAX_SOURCES: usize = 256 * 1024 * 1024;
/// Build copies a worker keeps. The least recently built go first, and a removed copy only means
/// the next build of that project there starts cold.
const KEEP_COPIES: usize = 200;

#[derive(Clone)]
struct Worker {
    builder: Arc<Builder>,
    token: Arc<String>,
    root: PathBuf,
    /// One build at a time per project copy. Builds of different projects run side by side, up
    /// to the builder's own limit.
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

pub async fn serve_worker(data_dir: &Path, config: &Config, bind: SocketAddr) -> std::io::Result<()> {
    let token = config.build.worker_token.resolve("GALLEY_WORKER_TOKEN").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a worker needs a token. Set GALLEY_WORKER_TOKEN to the same value on the server and its workers.",
        )
    })?;
    // A worker compiles whatever the server's users write, so it is held to the public rule.
    let builder = crate::app::make_builder(data_dir, config, true).await?;
    let root = data_dir.join("work");
    std::fs::create_dir_all(&root)?;
    let app = worker_router(Arc::new(builder), &token, root);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %listener.local_addr()?, "build worker listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

/// The worker's routes, for `serve_worker` and for tests that start one on a free port.
pub fn worker_router(builder: Arc<Builder>, token: &str, root: PathBuf) -> Router {
    let state = Worker {
        builder,
        token: Arc::new(token.to_string()),
        root,
        locks: Arc::default(),
    };
    Router::new()
        .route("/compile", post(compile))
        .route("/health", get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(MAX_SOURCES))
        .with_state(state)
}

/// Compare without stopping at the first difference, so response timing reveals nothing.
fn same_secret(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn compile(State(w): State<Worker>, headers: HeaderMap, body: Bytes) -> Response {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !same_secret(bearer, &w.token) {
        return (StatusCode::UNAUTHORIZED, "wrong worker token").into_response();
    }
    let job: WorkerJob = match headers.get(JOB_HEADER).and_then(|v| v.to_str().ok()).map(serde_json::from_str) {
        Some(Ok(job)) => job,
        _ => return (StatusCode::BAD_REQUEST, "missing or unreadable job").into_response(),
    };
    if !is_safe_id(&job.project) {
        return (StatusCode::BAD_REQUEST, "invalid project id").into_response();
    }

    let lock = w.locks.lock().await.entry(job.project.clone()).or_default().clone();
    let _held = lock.lock().await;
    let dir = w.root.join(&job.project);
    let unpacked = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || unpack_sources(&body, &dir)).await
    };
    if !matches!(unpacked, Ok(Ok(()))) {
        return (StatusCode::BAD_REQUEST, "the sources could not be unpacked").into_response();
    }

    let result = w.builder.run(job.id, &dir, &job.main_file, &job.request, &|_| {}).await;
    let out_dir = dir.join(".galley").join("build");
    let reply = {
        let (result, out_dir) = (result.clone(), out_dir.clone());
        tokio::task::spawn_blocking(move || pack_outcome(&result, &out_dir)).await
    };
    let response = match reply {
        Ok(Ok(bytes)) => ([(axum::http::header::CONTENT_TYPE, "application/x-tar")], bytes).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "the build output could not be packed").into_response(),
    };

    if !result.pending_figures.is_empty() {
        let (builder, dir, engine, figures) = (w.builder.clone(), dir.clone(), job.request.engine, result.pending_figures);
        let lock = lock.clone();
        tokio::spawn(async move {
            let _held = lock.lock().await;
            builder.warm_figures(&dir, engine, figures, || async { false }).await;
        });
    }
    let root = w.root.clone();
    tokio::task::spawn_blocking(move || prune(&root, KEEP_COPIES));
    response
}

/// Remove the least recently built copies beyond `keep`.
fn prune(root: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            let built = std::fs::metadata(e.path().join(".galley/build"))
                .or_else(|_| e.metadata())
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (built, e.path())
        })
        .collect();
    if dirs.len() <= keep {
        return;
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
    for (_, path) in dirs.into_iter().skip(keep) {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_compare_whole() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abd", "abc"));
        assert!(!same_secret("ab", "abc"));
        assert!(!same_secret("", "abc"));
    }

    #[test]
    fn project_ids_cannot_leave_the_work_directory() {
        assert!(is_safe_id("p-1_x"));
        assert!(!is_safe_id("../x"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(""));
    }

    #[test]
    fn pruning_keeps_the_most_recent_copies() {
        let tmp = tempfile::tempdir().unwrap();
        for (i, name) in ["old", "mid", "new"].iter().enumerate() {
            let dir = tmp.path().join(name).join(".galley/build");
            std::fs::create_dir_all(&dir).unwrap();
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(3600 * (3 - i as u64));
            std::fs::File::open(&dir).unwrap().set_modified(when).unwrap();
        }
        prune(tmp.path(), 2);
        assert!(!tmp.path().join("old").exists());
        assert!(tmp.path().join("mid").exists());
        assert!(tmp.path().join("new").exists());
    }
}
