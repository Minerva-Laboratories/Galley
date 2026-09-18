//! Comments and suggestions. Both are anchored to CRDT relative positions that the client
//! resolves. The server stores the anchor as opaque data. The server broadcasts each change, so
//! peers update live.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState, CollabEvent};
use crate::auth::current::CurrentUser;
use crate::auth::Role;
use crate::store::{Comment, Suggestion};

async fn blocking<T, F>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::Internal(e.to_string()))?
}

pub async fn comments(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<Comment>>, AppError> {
    app.require(&user, &id, Role::can_view, "viewing comments").await?;
    let store = app.store.clone();
    let list = blocking(move || store.comments(&id).map_err(|e| AppError::Internal(e.to_string()))).await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct NewComment {
    file: String,
    anchor: String,
    #[serde(default)]
    quote: Option<String>,
    body: String,
}

pub async fn add_comment(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewComment>,
) -> Result<(StatusCode, Json<Comment>), AppError> {
    app.require(&user, &id, Role::can_comment, "commenting").await?;
    let store = app.store.clone();
    let pid = id.clone();
    let comment = blocking(move || {
        store
            .add_comment(&pid, &body.file, &body.anchor, body.quote.as_deref(), &user, &body.body)
            .map_err(AppError::from)
    })
    .await?;
    app.emit(&id, CollabEvent::CommentAdded { comment: comment.clone() }).await;
    Ok((StatusCode::CREATED, Json(comment)))
}

#[derive(Deserialize)]
pub struct Resolve {
    resolved: bool,
}

pub async fn resolve_comment(
    State(app): State<AppState>,
    Path((id, cid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<Resolve>,
) -> Result<Json<Value>, AppError> {
    let role = app.require(&user, &id, Role::can_comment, "resolving comments").await?;
    let store = app.store.clone();
    let (pid, comment_id, uid) = (id.clone(), cid.clone(), user.id.clone());
    let is_admin = role.is_admin();
    blocking(move || -> Result<(), AppError> {
        // Anyone who can comment may resolve their own comment. An admin may resolve any comment.
        let author = store.comment_author(&pid, &comment_id).map_err(|e| AppError::Internal(e.to_string()))?;
        match author {
            Some(a) if a == uid || is_admin => {}
            Some(_) => return Err(AppError::Forbidden("Only the author or an admin can resolve this comment.".into())),
            None => return Err(AppError::NotFound("That comment no longer exists.".into())),
        }
        store
            .resolve_comment(&pid, &comment_id, body.resolved)
            .map(|_| ())
            .map_err(|e| AppError::Internal(e.to_string()))
    })
    .await?;
    app.emit(&id, CollabEvent::CommentResolved { id: cid, resolved: body.resolved }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn suggestions(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<Suggestion>>, AppError> {
    app.require(&user, &id, Role::can_view, "viewing suggestions").await?;
    let store = app.store.clone();
    let list = blocking(move || store.suggestions(&id).map_err(|e| AppError::Internal(e.to_string()))).await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct NewSuggestion {
    file: String,
    anchor: String,
    anchor_end: String,
    quote: String,
    replacement: String,
}

pub async fn add_suggestion(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewSuggestion>,
) -> Result<(StatusCode, Json<Suggestion>), AppError> {
    app.require(&user, &id, Role::can_suggest, "suggesting edits").await?;
    let store = app.store.clone();
    let pid = id.clone();
    let suggestion = blocking(move || {
        store
            .add_suggestion(&pid, &body.file, &body.anchor, &body.anchor_end, &body.quote, &body.replacement, &user)
            .map_err(AppError::from)
    })
    .await?;
    app.emit(&id, CollabEvent::SuggestionAdded { suggestion: suggestion.clone() }).await;
    Ok((StatusCode::CREATED, Json(suggestion)))
}

#[derive(Deserialize)]
pub struct SetStatus {
    /// "accepted" or "rejected".
    status: String,
}

/// The client applies an accepted suggestion through the CRDT, so acceptance needs edit rights.
/// A rejection needs only comment rights, and it closes the card.
pub async fn set_suggestion_status(
    State(app): State<AppState>,
    Path((id, sid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SetStatus>,
) -> Result<Json<Value>, AppError> {
    let status = match body.status.as_str() {
        "accepted" | "rejected" => body.status.clone(),
        _ => return Err(AppError::BadRequest("Status must be accepted or rejected.".into())),
    };
    let need: fn(Role) -> bool = if status == "accepted" { Role::can_edit } else { Role::can_comment };
    app.require(&user, &id, need, "acting on this suggestion").await?;
    let store = app.store.clone();
    let (pid, suggestion_id, st) = (id.clone(), sid.clone(), status.clone());
    let changed = blocking(move || {
        store.set_suggestion_status(&pid, &suggestion_id, &st).map_err(|e| AppError::Internal(e.to_string()))
    })
    .await?;
    if !changed {
        return Err(AppError::Conflict("That suggestion was already accepted or rejected.".into()));
    }
    app.emit(&id, CollabEvent::SuggestionUpdated { id: sid, status }).await;
    Ok(Json(json!({ "ok": true })))
}
