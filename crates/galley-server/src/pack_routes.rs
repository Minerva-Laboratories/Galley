//! Submission (SPEC §13.8). Package the project for a venue, download the archive, and freeze the
//! project as submitted.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use galley_build::pack::{PackReport, ARCHIVE};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::CurrentUser;
use crate::auth::Role;
use crate::history_routes::checkpoint_now;
use crate::venues;
use crate::app::CollabEvent;

pub async fn list_venues() -> Json<Vec<venues::Venue>> {
    Json(venues::all())
}

/// POST /api/projects/{id}/pack. It stages the files, verifies them in a clean sandbox, and makes
/// the archive. The route is slow. The client shows the report when the route returns.
pub async fn pack(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<PackReport>, AppError> {
    app.require(&user, &id, Role::can_compile, "packaging the project").await?;
    let project = app.registry.open(&id).await?;
    project.flush_now().await?;
    let meta = app.registry.meta(&id)?;
    let workdir = project.workdir().to_path_buf();
    let report = app
        .builder
        .pack(&workdir, &meta.main_file, &|_p| {})
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    app.store.audit(Some(&id), Some(&user), "project.pack", Some(if report.archive { "ok" } else { "failed" }));
    Ok(Json(report))
}

/// GET /api/projects/{id}/pack/download. It returns the archive from the last successful pack.
pub async fn download(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    app.require(&user, &id, Role::can_view, "downloading the package").await?;
    let project = app.registry.open(&id).await?;
    let meta = app.registry.meta(&id)?;
    let path = project.workdir().join(ARCHIVE);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| AppError::NotFound("No package yet. Run Package first.".into()))?;
    let slug: String = meta.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let venue = meta.venue.as_deref().unwrap_or("submission");
    Ok((
        [
            (header::CONTENT_TYPE, "application/gzip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}-{venue}.tar.gz\"", slug.trim_matches('-'))),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize, Default)]
pub struct FreezeBody {
    /// A venue preset id or free text. The default is the project's venue setting.
    #[serde(default)]
    venue: Option<String>,
}

/// POST /api/projects/{id}/submitted. It makes the checkpoint "Submitted to <venue>" and records
/// it, so History can offer the camera-ready diff against it.
pub async fn freeze(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    body: Option<Json<FreezeBody>>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    app.require(&user, &id, Role::can_edit, "freezing the submission").await?;
    let meta = app.registry.meta(&id)?;
    let venue = body
        .and_then(|Json(b)| b.venue)
        .or(meta.venue.clone())
        .map(|v| venues::find(&v).map(|p| p.name).unwrap_or(v))
        .unwrap_or_else(|| "the venue".to_string());
    let checkpoint = checkpoint_now(&app, &id, &user, &format!("Submitted to {venue}")).await?;
    let sha = checkpoint.commit_sha.clone();
    app.registry.update_meta(&id, |m| m.submitted_checkpoint = Some(sha))?;
    app.emit(&id, CollabEvent::CheckpointsChanged).await;
    Ok((StatusCode::CREATED, Json(json!({ "checkpoint": checkpoint }))))
}
