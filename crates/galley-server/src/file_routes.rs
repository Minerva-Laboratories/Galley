//! File operations beyond create: upload, download, rename, delete. Each change is a commit, so
//! History can bring anything back.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::CurrentUser;
use crate::auth::Role;

/// The server refuses an upload larger than this. Figures and style bundles fit well inside it.
pub const MAX_UPLOAD: usize = 25 * 1024 * 1024;

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    inline: bool,
}

/// PUT /api/projects/{id}/files/content?path=...[&replace=true]. The body is the file.
pub async fn upload(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
    CurrentUser(user): CurrentUser,
    body: Bytes,
) -> Result<(StatusCode, Json<galley_sync::FileEntry>), AppError> {
    app.require(&user, &id, Role::can_edit, "uploading files").await?;
    let project = app.registry.open(&id).await?;
    let entry = project.put_file(&q.path, &body, q.replace, &user.name).await.map_err(|e| match e {
        galley_sync::Error::NotText(p) => AppError::BadRequest(format!("{p} is not valid UTF-8 text. Save it as UTF-8, or give it a binary extension.")),
        other => other.into(),
    })?;
    app.registry.touch(&id);
    app.store.audit(Some(&id), Some(&user), "file.upload", Some(&format!("{} ({} bytes)", entry.path, entry.size)));
    Ok((StatusCode::CREATED, Json(entry)))
}

/// GET /api/projects/{id}/files/content?path=...[&inline=true]. It returns the text as the authors see it now.
pub async fn download(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
    CurrentUser(user): CurrentUser,
) -> Result<Response, AppError> {
    app.require(&user, &id, Role::can_view, "downloading files").await?;
    let project = app.registry.open(&id).await?;
    let (rel, bytes) = project.read_bytes(&q.path).await?;
    let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    // Only a format that a browser renders without running anything is shown inline. Everything
    // else is a download, and that includes SVG and HTML. A sandbox CSP covers both cases.
    let viewable = match ext.as_str() {
        "pdf" => Some("application/pdf"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    };
    let (ctype, disposition) = match viewable {
        Some(t) if q.inline => (t.to_string(), "inline"),
        Some(t) => (t.to_string(), "attachment"),
        None if galley_sync::paths::is_text_path(&rel) => ("text/plain; charset=utf-8".to_string(), "attachment"),
        None => ("application/octet-stream".to_string(), "attachment"),
    };
    Ok((
        [
            (header::CONTENT_TYPE, ctype),
            (header::CONTENT_DISPOSITION, content_disposition(disposition, &name)),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self'; style-src 'unsafe-inline'".to_string()),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
        ],
        bytes,
    )
        .into_response())
}

/// `attachment; filename="plot.png"; filename*=UTF-8''plot.png`, safe for any file name.
fn content_disposition(kind: &str, name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' { c } else { '_' }).collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// DELETE /api/projects/{id}/files?path=...
pub async fn delete(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "deleting files").await?;
    let meta = app.registry.meta(&id)?;
    if galley_sync::paths::clean_rel_path(&q.path).as_deref() == Some(meta.main_file.as_str()) {
        return Err(AppError::BadRequest(format!(
            "{} is the main file. Set another .tex file as the main file first (⋯ → Set as main file).",
            meta.main_file
        )));
    }
    let project = app.registry.open(&id).await?;
    project.delete_file(&q.path, &user.name).await?;
    app.registry.touch(&id);
    app.store.audit(Some(&id), Some(&user), "file.delete", Some(&q.path));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct RenameBody {
    from: String,
    to: String,
}

/// POST /api/projects/{id}/files/rename. A move of the main file moves the setting with it.
pub async fn rename(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<RenameBody>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "renaming files").await?;
    let project = app.registry.open(&id).await?;
    let from = galley_sync::paths::clean_rel_path(&body.from).unwrap_or_default();
    let to = project.rename_file(&body.from, &body.to, &user.name).await?;
    let meta = app.registry.meta(&id)?;
    if meta.main_file == from {
        let new_main = to.clone();
        app.registry.update_meta(&id, move |m| m.main_file = new_main)?;
        if let Ok(builds) = app.builds(&id).await {
            builds.set_main_file(&to).await;
        }
    }
    app.store.audit(Some(&id), Some(&user), "file.rename", Some(&format!("{from} → {to}")));
    Ok(Json(json!({ "path": to })))
}
