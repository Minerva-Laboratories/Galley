//! Checkpoints, revision browsing, diffs, and restore. All git work goes through the project's
//! `ProjectSync`, which owns the repository. This module only enforces roles and shapes JSON.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use galley_build::runner::LAST_DIFF_PDF;
use galley_history::FileDiff;
use galley_sync::DiffSide;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState, CollabEvent};
use crate::auth::current::CurrentUser;
use crate::auth::Role;
use crate::store::Checkpoint;

/// The query value meaning "the live working tree" instead of a committed revision.
const WORKDIR: &str = "WORKDIR";

/// A caller-supplied target file is honoured only if it is a relative in-project `.tex` file.
fn safe_tex_target(workdir: &std::path::Path, file: &str) -> bool {
    !file.is_empty()
        && file.ends_with(".tex")
        && !file.starts_with('/')
        && !file.contains('\\')
        && !file.split('/').any(|c| c == "..")
        && workdir.join(file).is_file()
}

fn side(value: &str) -> DiffSide {
    if value.is_empty() || value == WORKDIR {
        DiffSide::Workdir
    } else {
        DiffSide::Rev(value.to_string())
    }
}

pub async fn list_checkpoints(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<Checkpoint>>, AppError> {
    app.require(&user, &id, Role::can_view, "viewing checkpoints").await?;
    let store = app.store.clone();
    let pid = id.clone();
    let list = tokio::task::spawn_blocking(move || store.checkpoints(&pid))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct CheckpointBody {
    label: String,
}

pub async fn create_checkpoint(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CheckpointBody>,
) -> Result<(StatusCode, Json<Checkpoint>), AppError> {
    app.require(&user, &id, Role::can_edit, "creating a checkpoint").await?;
    let label = body.label.trim().to_string();
    if label.is_empty() {
        return Err(AppError::BadRequest("a checkpoint needs a label".into()));
    }
    let checkpoint = checkpoint_now(&app, &id, &user, &label).await?;
    app.emit(&id, CollabEvent::CheckpointsChanged).await;
    Ok((StatusCode::CREATED, Json(checkpoint)))
}

/// Flush, tag the current commit, record it. Callers check the role and emit the event.
pub(crate) async fn checkpoint_now(app: &AppState, id: &str, user: &crate::store::User, label: &str) -> Result<Checkpoint, AppError> {
    let project = app.registry.open(id).await?;
    let cp_id = app.store.new_checkpoint_id();
    let author = galley_history::Author::from_display_name(&user.name);
    let sha = project.create_checkpoint(&cp_id, label, author).await?;

    let store = app.store.clone();
    let (pid, cpid, sha2, label2, uid) = (id.to_string(), cp_id.clone(), sha.clone(), label.to_string(), user.id.clone());
    let checkpoint = tokio::task::spawn_blocking(move || store.record_checkpoint(&cpid, &pid, &sha2, &label2, &uid))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;

    app.store.audit(Some(id), Some(user), "checkpoint.create", Some(label));
    Ok(checkpoint)
}

#[derive(Deserialize)]
pub struct FileAtQuery {
    rev: String,
    path: String,
}

pub async fn file_at(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<FileAtQuery>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_view, "browsing history").await?;
    let project = app.registry.open(&id).await?;
    let content = project.file_at(&q.rev, &q.path).await?;
    Ok(Json(json!({ "content": content })))
}

#[derive(Deserialize)]
pub struct DiffQuery {
    /// A commit sha, or `WORKDIR` for the live working tree.
    from: String,
    /// A commit sha. Use `WORKDIR`, or omit it, for the live working tree.
    #[serde(default)]
    to: Option<String>,
    /// Limit the diff to one file.
    #[serde(default)]
    path: Option<String>,
}

pub async fn diff(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<DiffQuery>,
) -> Result<Json<Vec<FileDiff>>, AppError> {
    app.require(&user, &id, Role::can_view, "viewing a diff").await?;
    let project = app.registry.open(&id).await?;
    let to = q.to.as_deref().unwrap_or(WORKDIR);
    let diff = project.diff(side(&q.from), side(to), q.path).await?;
    Ok(Json(diff))
}

#[derive(Deserialize)]
pub struct RestoreBody {
    /// The commit sha to restore the working tree to.
    rev: String,
    /// A human label for the commit message (e.g. a checkpoint name or short sha).
    #[serde(default)]
    label: Option<String>,
}

pub async fn restore(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<RestoreBody>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "restoring a version").await?;
    let project = app.registry.open(&id).await?;
    let label = body.label.unwrap_or_else(|| body.rev.chars().take(7).collect());
    let author = galley_history::Author::from_display_name(&user.name);
    let commit = project.restore(&body.rev, author, &label).await?;
    if commit.is_some() {
        app.registry.touch(&id);
    }
    app.store.audit(Some(&id), Some(&user), "history.restore", Some(&label));
    Ok(Json(json!({ "commit": commit })))
}

#[derive(Deserialize)]
pub struct CompareBody {
    /// The base revision (a commit sha).
    from: String,
    /// The other side. It is a commit sha, or `WORKDIR`, or omitted for the live text.
    #[serde(default)]
    to: Option<String>,
    /// The `.tex` file to compare. The default is the project's main file.
    #[serde(default)]
    file: Option<String>,
}

/// Build a marked-up PDF of the changes between two versions of a `.tex` file with latexdiff.
pub async fn latexdiff(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CompareBody>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_compile, "comparing versions").await?;
    if !app.builder.latexdiff_available() {
        return Err(AppError::BadRequest(
            "This server has no latexdiff installed, so PDF comparison is unavailable.".into(),
        ));
    }
    let project = app.registry.open(&id).await?;
    let meta = app.registry.meta(&id)?;
    let workdir = project.workdir().to_path_buf();
    let file = body
        .file
        .filter(|f| safe_tex_target(&workdir, f))
        .unwrap_or(meta.main_file);

    let old = project
        .file_at(&body.from, &file)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("{file} does not exist in that version.")))?;
    let to = body.to.as_deref().unwrap_or(WORKDIR);
    let new = if to == WORKDIR {
        project.flush_now().await?;
        tokio::fs::read_to_string(workdir.join(&file)).await.unwrap_or_default()
    } else {
        project
            .file_at(to, &file)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("{file} does not exist in that version.")))?
    };

    app.builder
        .latexdiff(&workdir, &old, &new, &|_| {})
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(json!({ "pdf_available": true, "file": file })))
}

/// Serve the most recent latexdiff comparison PDF.
pub async fn latexdiff_pdf(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    app.require(&user, &id, Role::can_view, "viewing the comparison").await?;
    let builds = app.builds(&id).await?;
    let path = builds.out_dir().join(LAST_DIFF_PDF);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound("No comparison yet. Run Compare PDFs first.".into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/pdf")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        Body::from(bytes),
    )
        .into_response())
}
