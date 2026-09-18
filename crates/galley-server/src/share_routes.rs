//! Members, share links, and governance. Only an admin can manage membership (SPEC.md §4.3).
//! Under majority or unanimous governance, a change to a member, a role or the governance mode
//! becomes a pending request that the admins vote on. Under solo governance it applies at once.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState, CollabEvent};
use crate::auth::current::CurrentUser;
use crate::auth::Role;
use crate::store::{GovernanceMode, Member, Resolution, RoleRequest, ShareLink, User};

async fn blocking<T, F>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::Internal(e.to_string()))?
}

// --- members ------------------------------------------------------------------------------

pub async fn members(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<Member>>, AppError> {
    app.require(&user, &id, Role::is_admin, "seeing the member list").await?;
    let store = app.store.clone();
    let list = blocking(move || store.members(&id).map_err(|e| AppError::Internal(e.to_string()))).await?;
    Ok(Json(list))
}

/// The governance mode and the open role-change requests. This drives the Share modal.
pub async fn governance_state(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "seeing governance").await?;
    let store = app.store.clone();
    let (mode, requests) = blocking(move || -> Result<(GovernanceMode, Vec<RoleRequest>), AppError> {
        Ok((
            store.governance(&id).map_err(|e| AppError::Internal(e.to_string()))?,
            store.open_requests(&id).map_err(|e| AppError::Internal(e.to_string()))?,
        ))
    })
    .await?;
    Ok(Json(json!({ "mode": mode, "requests": requests })))
}

#[derive(Deserialize)]
pub struct Invite {
    email: String,
    role: String,
}

pub async fn invite(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<Invite>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    app.require(&user, &id, Role::is_admin, "inviting people").await?;
    let role = Role::parse(&body.role).ok_or_else(|| AppError::BadRequest("Unknown role.".into()))?;
    // Validate first, so the server never queues a request that cannot pass.
    let store = app.store.clone();
    let email = body.email.trim().to_string();
    let invitee = blocking(move || {
        store
            .user_by_email(&email)
            .map_err(|e| AppError::Internal(e.to_string()))
    })
    .await?
    .ok_or_else(|| AppError::NotFound(format!("No account for {}. They need to sign in once before you can add them.", body.email.trim())))?;
    let summary = format!("Add {} as {}", invitee.name, role.as_str());
    let payload = json!({ "email": invitee.email, "role": role.as_str() });
    let out = govern(&app, &user, &id, "invite", Some(invitee.id.clone()), payload, summary).await?;
    Ok((StatusCode::CREATED, Json(out)))
}

#[derive(Deserialize)]
pub struct SetRole {
    role: String,
}

pub async fn set_role(
    State(app): State<AppState>,
    Path((id, uid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SetRole>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "changing roles").await?;
    let role = Role::parse(&body.role).ok_or_else(|| AppError::BadRequest("Unknown role.".into()))?;
    let store = app.store.clone();
    let (pid, target) = (id.clone(), uid.clone());
    let name = blocking(move || -> Result<String, AppError> {
        let members = store.members(&pid).map_err(|e| AppError::Internal(e.to_string()))?;
        let member = members
            .iter()
            .find(|m| m.user_id == target)
            .ok_or_else(|| AppError::NotFound("That person is not a member.".into()))?;
        if !role.is_admin() && is_last_admin(&members, &target) {
            return Err(AppError::BadRequest("This is the project's only admin. Make someone else an admin first.".into()));
        }
        Ok(member.name.clone())
    })
    .await?;
    let summary = format!("Change {name} to {}", role.as_str());
    let out = govern(&app, &user, &id, "set_role", Some(uid), json!({ "role": role.as_str() }), summary).await?;
    Ok(Json(out))
}

pub async fn remove_member(
    State(app): State<AppState>,
    Path((id, uid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "removing people").await?;
    let store = app.store.clone();
    let (pid, target) = (id.clone(), uid.clone());
    let name = blocking(move || -> Result<String, AppError> {
        let members = store.members(&pid).map_err(|e| AppError::Internal(e.to_string()))?;
        let member = members
            .iter()
            .find(|m| m.user_id == target)
            .ok_or_else(|| AppError::NotFound("That person is not a member.".into()))?;
        if is_last_admin(&members, &target) {
            return Err(AppError::BadRequest("You can't remove the project's only admin.".into()));
        }
        Ok(member.name.clone())
    })
    .await?;
    let summary = format!("Remove {name}");
    let out = govern(&app, &user, &id, "remove_member", Some(uid), json!({}), summary).await?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct SetGovernance {
    mode: String,
}

pub async fn set_governance(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SetGovernance>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "changing governance").await?;
    let mode = GovernanceMode::parse(&body.mode)
        .ok_or_else(|| AppError::BadRequest("Mode must be solo, majority, or unanimous.".into()))?;
    let summary = format!("Set role changes to {}", mode.as_str());
    let out = govern(&app, &user, &id, "set_governance", None, json!({ "governance": mode.as_str() }), summary).await?;
    Ok(Json(out))
}

// --- request votes ------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct VoteBody {
    approve: bool,
}

pub async fn vote(
    State(app): State<AppState>,
    Path((id, rid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<VoteBody>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "voting on a request").await?;
    let store = app.store.clone();
    let (pid, request_id, uid, approve) = (id.clone(), rid.clone(), user.id.clone(), body.approve);
    let (resolution, kind) = blocking(move || -> Result<(Resolution, String), AppError> {
        let req = store
            .request(&pid, &request_id)
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or_else(|| AppError::NotFound("That request no longer exists.".into()))?;
        if req.status != "open" {
            return Err(AppError::Conflict("That request is already resolved.".into()));
        }
        store.record_vote(&request_id, &uid, approve).map_err(|e| AppError::Internal(e.to_string()))?;
        let resolution = store.evaluate_request(&pid, &request_id).map_err(AppError::from)?;
        Ok((resolution, req.kind))
    })
    .await?;
    finish(&app, &user, &id, &rid, resolution, &kind).await;
    Ok(Json(json!({ "status": status_word(resolution) })))
}

pub async fn cancel_request(
    State(app): State<AppState>,
    Path((id, rid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "cancelling a request").await?;
    let store = app.store.clone();
    let (pid, request_id) = (id.clone(), rid.clone());
    blocking(move || -> Result<(), AppError> {
        let req = store
            .request(&pid, &request_id)
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or_else(|| AppError::NotFound("That request no longer exists.".into()))?;
        if req.status != "open" {
            return Err(AppError::Conflict("That request is already resolved.".into()));
        }
        store.set_request_status(&request_id, "cancelled").map_err(|e| AppError::Internal(e.to_string()))
    })
    .await?;
    app.emit(&id, CollabEvent::RequestsChanged).await;
    Ok(Json(json!({ "ok": true })))
}

// --- share links --------------------------------------------------------------------------

pub async fn list_shares(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<ShareLink>>, AppError> {
    app.require(&user, &id, Role::is_admin, "managing share links").await?;
    let store = app.store.clone();
    let list = blocking(move || store.shares(&id).map_err(|e| AppError::Internal(e.to_string()))).await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct CreateShare {
    role: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    expires_days: i64,
}

pub async fn create_share(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateShare>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    app.require(&user, &id, Role::is_admin, "creating share links").await?;
    let role = Role::parse(&body.role).filter(|r| !r.is_admin()).ok_or_else(|| {
        AppError::BadRequest("Share links can grant editor, commenter, or viewer access.".into())
    })?;
    let expires = (body.expires_days > 0).then(|| Utc::now() + Duration::days(body.expires_days));
    let store = app.store.clone();
    let (pid, label, uid) = (id.clone(), body.label.clone(), user.id.clone());
    let (link, token) = blocking(move || {
        store
            .create_share(&pid, role, label.as_deref(), expires, &uid)
            .map_err(|e| AppError::Internal(e.to_string()))
    })
    .await?;
    app.store.audit(Some(&id), Some(&user), "share.create", Some(role.as_str()));
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": link.id, "role": link.role, "label": link.label, "expires_at": link.expires_at, "path": format!("/s/{token}") })),
    ))
}

pub async fn revoke_share(
    State(app): State<AppState>,
    Path((id, sid)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::is_admin, "revoking share links").await?;
    let store = app.store.clone();
    let (pid, share_id) = (id.clone(), sid.clone());
    let removed = blocking(move || store.revoke_share(&pid, &share_id).map_err(|e| AppError::Internal(e.to_string()))).await?;
    if !removed {
        return Err(AppError::NotFound("That share link no longer exists.".into()));
    }
    app.store.audit(Some(&id), Some(&user), "share.revoke", Some(&sid));
    Ok(Json(json!({ "ok": true })))
}

/// A public route. It reports what a share link grants, so the landing page can name the project.
pub async fn preview(State(app): State<AppState>, Path(token): Path<String>) -> Result<Json<Value>, AppError> {
    let store = app.store.clone();
    let resolved = blocking(move || store.share_by_token(&token).map_err(|e| AppError::Internal(e.to_string()))).await?;
    let (project_id, role) = resolved
        .ok_or_else(|| AppError::NotFound("This share link is invalid, revoked, or expired.".into()))?;
    let name = app.registry.meta(&project_id).map(|m| m.name).unwrap_or_else(|_| "a project".into());
    Ok(Json(json!({ "project_name": name, "role": role })))
}

// --- governance plumbing ------------------------------------------------------------------

/// Create a request for a governed action and evaluate it. It applies at once in solo mode, and
/// also when the proposer is the only admin. In every other case it waits for votes.
async fn govern(
    app: &AppState,
    user: &User,
    project_id: &str,
    kind: &str,
    target_id: Option<String>,
    payload: Value,
    summary: String,
) -> Result<Value, AppError> {
    let store = app.store.clone();
    let (pid, u, kind_owned) = (project_id.to_string(), user.clone(), kind.to_string());
    let (request_id, resolution) = blocking(move || -> Result<(String, Resolution), AppError> {
        let req = store
            .create_request(&pid, &kind_owned, target_id.as_deref(), &payload, &summary, &u)
            .map_err(AppError::from)?;
        let resolution = store.evaluate_request(&pid, &req.id).map_err(AppError::from)?;
        Ok((req.id, resolution))
    })
    .await?;
    finish(app, user, project_id, &request_id, resolution, kind).await;
    Ok(json!({ "status": status_word(resolution), "request_id": request_id }))
}

/// Emit the right events and audit after a request is created or voted on.
async fn finish(app: &AppState, user: &User, project_id: &str, request_id: &str, resolution: Resolution, kind: &str) {
    match resolution {
        Resolution::Applied => {
            app.store.audit(Some(project_id), Some(user), &format!("governance.apply.{kind}"), Some(request_id));
            app.emit(project_id, CollabEvent::MembersChanged).await;
            app.emit(project_id, CollabEvent::RequestsChanged).await;
        }
        Resolution::Rejected => {
            app.store.audit(Some(project_id), Some(user), "governance.reject", Some(request_id));
            app.emit(project_id, CollabEvent::RequestsChanged).await;
        }
        Resolution::Pending => {
            app.emit(project_id, CollabEvent::RequestsChanged).await;
        }
        Resolution::Gone => {}
    }
}

fn status_word(r: Resolution) -> &'static str {
    match r {
        Resolution::Applied => "applied",
        Resolution::Rejected => "rejected",
        Resolution::Pending => "pending",
        Resolution::Gone => "gone",
    }
}

fn is_last_admin(members: &[Member], user_id: &str) -> bool {
    let admins: Vec<&Member> = members.iter().filter(|m| m.role.is_admin()).collect();
    admins.len() == 1 && admins[0].user_id == user_id
}
