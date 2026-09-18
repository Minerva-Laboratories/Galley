//! Device tokens. An MCP client or a local runner presents one as `Authorization: Bearer`. A
//! browser session mints a token. The server shows it once. Each token can be revoked on its own.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::CurrentUser;
use crate::store::DeviceToken;

#[derive(Deserialize)]
pub struct NewToken {
    label: String,
}

pub async fn create(
    State(app): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewToken>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let label = body.label.trim().to_string();
    if label.is_empty() {
        return Err(AppError::BadRequest("Give the token a label, like the client it is for.".into()));
    }
    let store = app.store.clone();
    let uid = user.id.clone();
    let (meta, token) = tokio::task::spawn_blocking(move || store.create_device_token(&uid, &label))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    app.store.audit(None, Some(&user), "token.create", Some(&meta.label));
    Ok((StatusCode::CREATED, Json(json!({ "token": token, "id": meta.id, "label": meta.label }))))
}

pub async fn list(
    State(app): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<DeviceToken>>, AppError> {
    let store = app.store.clone();
    let uid = user.id.clone();
    let list = tokio::task::spawn_blocking(move || store.device_tokens(&uid))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(list))
}

pub async fn revoke(
    State(app): State<AppState>,
    Path(tid): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    let store = app.store.clone();
    let (uid, id) = (user.id.clone(), tid.clone());
    let ok = tokio::task::spawn_blocking(move || store.revoke_device_token(&uid, &id))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !ok {
        return Err(AppError::NotFound("No such token.".into()));
    }
    app.store.audit(None, Some(&user), "token.revoke", Some(&tid));
    Ok(Json(json!({ "ok": true })))
}
