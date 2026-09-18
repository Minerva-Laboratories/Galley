use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use galley_build::runner::{LAST_GOOD_PDF, LAST_LOG};
use galley_build::BuildRequest;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::CurrentUser;
use crate::auth::Role;

#[derive(Deserialize, Default)]
pub struct BuildBody {
    #[serde(default)]
    draft: bool,
    /// The file to compile. The default is the project's main file.
    #[serde(default)]
    file: Option<String>,
}

pub async fn request(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    body: Option<Json<BuildBody>>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    app.require(&user, &id, Role::can_compile, "building").await?;
    let builds = app.builds(&id).await?;
    let (draft, file) = body.map(|b| (b.draft, b.file.clone())).unwrap_or((false, None));
    let (lint_disabled, figure_cache) = app.registry.meta(&id).map(|m| (m.lint_disabled, m.figure_cache)).unwrap_or((Vec::new(), true));
    builds.request(BuildRequest { draft, file, lint_disabled, figure_cache }).await;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

pub async fn status(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_view, "seeing the build status").await?;
    let builds = app.builds(&id).await?;
    Ok(Json(json!({
        "last": builds.last().await,
        "running": builds.is_running().await,
        "sandbox": app.builder.sandbox.kind,
        "engine": "tectonic",
        "latexdiff": app.builder.latexdiff_available(),
    })))
}

pub async fn pdf(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    app.require(&user, &id, Role::can_view, "viewing the PDF").await?;
    let builds = app.builds(&id).await?;
    let path = builds.out_dir().join(LAST_GOOD_PDF);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::NotFound("No PDF yet. Build the project first.".into()))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let etag = format!("\"{}-{}\"", meta.len(), modified);
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let bytes = tokio::fs::read(&path).await?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/pdf")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (header::ETAG, HeaderValue::from_str(&etag).unwrap_or(HeaderValue::from_static("\"0\""))),
            (
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("inline; filename=\"{id}.pdf\"")).unwrap_or(HeaderValue::from_static("inline")),
            ),
        ],
        Body::from(bytes),
    )
        .into_response())
}

pub async fn log(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    app.require(&user, &id, Role::can_view, "reading the build log").await?;
    let builds = app.builds(&id).await?;
    let text = tokio::fs::read_to_string(builds.out_dir().join(LAST_LOG))
        .await
        .map_err(|_| AppError::NotFound("No build log yet. Build the project first.".into()))?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

#[derive(Deserialize)]
pub struct ForwardQuery {
    file: String,
    line: u32,
}

pub async fn synctex_forward(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ForwardQuery>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_view, "using SyncTeX").await?;
    let builds = app.builds(&id).await?;
    let st = tokio::task::spawn_blocking(move || builds.synctex())
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .map_err(|_| AppError::NotFound("No SyncTeX data yet. Build the project first.".into()))?;
    match st.forward(&q.file, q.line) {
        Some(loc) => Ok(Json(json!(loc))),
        None => Err(AppError::NotFound(format!("Nothing in the PDF comes from {}:{}.", q.file, q.line))),
    }
}

#[derive(Deserialize)]
pub struct InverseQuery {
    page: u32,
    x: f64,
    y: f64,
}

pub async fn synctex_inverse(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<InverseQuery>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_view, "using SyncTeX").await?;
    let builds = app.builds(&id).await?;
    let st = tokio::task::spawn_blocking(move || builds.synctex())
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .map_err(|_| AppError::NotFound("No SyncTeX data yet. Build the project first.".into()))?;
    match st.inverse(q.page, q.x, q.y) {
        Some(loc) => Ok(Json(json!(loc))),
        None => Err(AppError::NotFound("No source location for that spot.".into())),
    }
}

/// POST /api/projects/{id}/figures/clear. It drops every cached figure. The next build makes them again.
pub async fn clear_figures(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    app.require(&user, &id, Role::can_compile, "clearing the figure cache").await?;
    let project = app.registry.open(&id).await?;
    let removed = app.builder.clear_figures(project.workdir());
    app.store.audit(Some(&id), Some(&user), "figures.clear", Some(&removed.to_string()));
    Ok(Json(serde_json::json!({ "removed": removed })))
}
