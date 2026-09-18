//! The sign-in flows. They are the first-run admin, email and password login, logout, and the
//! share-link landing page. The landing page asks only for a display name. The session and the
//! CSRF token travel in cookies.

use axum::extract::State;
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::{csrf_cookie, expired, session_cookie, MaybeUser, CSRF_COOKIE, SESSION_COOKIE};
use crate::auth::{random_token, Role};
use crate::store::User;

/// Report the current user and whether public signup is on, so the SPA shows the right first screen.
pub async fn me(State(app): State<AppState>, MaybeUser(user): MaybeUser, jar: CookieJar) -> (CookieJar, Json<Value>) {
    let store = app.store.clone();
    let needs_setup = matches!(
        tokio::task::spawn_blocking(move || store.user_count()).await,
        Ok(Ok(0))
    );
    // Give the SPA a CSRF token to echo on writes.
    let jar = if jar.get(CSRF_COOKIE).is_none() {
        jar.add(csrf_cookie(random_token(), app.secure))
    } else {
        jar
    };
    (
        jar,
        Json(json!({
            "user": user.map(public_user),
            "needs_setup": needs_setup,
            "public_signup": app.public_signup,
        })),
    )
}

#[derive(Deserialize)]
pub struct Signup {
    email: String,
    name: String,
    password: String,
}

/// Create the first-run admin, or a new account when public signup is on.
pub async fn signup(
    State(app): State<AppState>,
    jar: CookieJar,
    Json(body): Json<Signup>,
) -> Result<(CookieJar, Json<Value>), AppError> {
    let store = app.store.clone();
    let first = matches!(tokio::task::spawn_blocking({
        let s = store.clone();
        move || s.user_count()
    }).await.map_err(internal)?, Ok(0));
    if !first && !app.public_signup {
        return Err(AppError::Forbidden(
            "This server is invite-only. Ask an admin to add you, or use a share link.".into(),
        ));
    }
    let user = tokio::task::spawn_blocking(move || store.create_user(&body.email, &body.name, &body.password, first))
        .await
        .map_err(internal)??;
    app.store.audit(None, Some(&user), if first { "admin.create" } else { "account.create" }, None);
    Ok(sign_in(&app, jar, &user))
}

#[derive(Deserialize)]
pub struct Login {
    email: String,
    password: String,
}

pub async fn login(
    State(app): State<AppState>,
    jar: CookieJar,
    Json(body): Json<Login>,
) -> Result<(CookieJar, Json<Value>), AppError> {
    let store = app.store.clone();
    let email = body.email.clone();
    let user = tokio::task::spawn_blocking(move || store.user_by_email(&email))
        .await
        .map_err(internal)?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let ok = user
        .as_ref()
        .and_then(|u| u.pw_hash.as_deref())
        .is_some_and(|h| crate::auth::verify_password(&body.password, h));
    match user {
        Some(u) if ok => Ok(sign_in(&app, jar, &u)),
        _ => Err(AppError::Unauthorized("That email and password don't match.".into())),
    }
}

pub async fn logout(State(app): State<AppState>, jar: CookieJar) -> (CookieJar, Json<Value>) {
    if let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) {
        let store = app.store.clone();
        let _ = tokio::task::spawn_blocking(move || store.delete_session(&token)).await;
    }
    let jar = jar.remove(expired(SESSION_COOKIE)).remove(expired(CSRF_COOKIE));
    (jar, Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct Landing {
    token: String,
    name: String,
}

/// A share-link visitor. A signed-in account joins with its own identity and is never downgraded.
/// An anonymous visitor becomes a named guest.
pub async fn landing(
    State(app): State<AppState>,
    MaybeUser(current): MaybeUser,
    jar: CookieJar,
    Json(body): Json<Landing>,
) -> Result<(CookieJar, Json<Value>), AppError> {
    let store = app.store.clone();
    let token = body.token.clone();
    let resolved = tokio::task::spawn_blocking(move || store.share_by_token(&token))
        .await
        .map_err(internal)?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let (project_id, role) = resolved.ok_or_else(|| {
        AppError::NotFound("This share link is invalid, revoked, or expired. Ask for a new one.".into())
    })?;

    // An existing account that is not a guest keeps its stronger role and its current session.
    if let Some(user) = current.filter(|u| !u.is_guest) {
        let store = app.store.clone();
        let (pid, uid) = (project_id.clone(), user.id.clone());
        let effective = tokio::task::spawn_blocking(move || -> Result<Role, AppError> {
            let existing = store.role_of(&pid, &uid).map_err(|e| AppError::Internal(e.to_string()))?;
            let effective = existing.map(|e| e.max(role)).unwrap_or(role);
            store.set_member(&pid, &uid, effective).map_err(|e| AppError::Internal(e.to_string()))?;
            Ok(effective)
        })
        .await
        .map_err(internal)??;
        app.store.audit(Some(&project_id), Some(&user), "share.join", Some(effective.as_str()));
        return Ok((jar, Json(json!({ "user": public_user(user), "role": effective, "project_id": project_id }))));
    }

    let store = app.store.clone();
    let name = body.name.clone();
    let pid = project_id.clone();
    let guest = tokio::task::spawn_blocking(move || -> Result<User, AppError> {
        let guest = store.create_guest(&name)?;
        store.set_member(&pid, &guest.id, role).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(guest)
    })
    .await
    .map_err(internal)??;
    app.store.audit(Some(&project_id), Some(&guest), "share.join", Some(role.as_str()));

    let (jar, mut value) = sign_in(&app, jar, &guest);
    if let Value::Object(map) = &mut value.0 {
        map.insert("role".into(), json!(role));
        map.insert("project_id".into(), json!(project_id));
    }
    Ok((jar, value))
}

/// Create a session, set the cookies, and return the user.
fn sign_in(app: &AppState, jar: CookieJar, user: &User) -> (CookieJar, Json<Value>) {
    let token = match app.store.create_session(&user.id, 30) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "could not create session");
            return (jar, Json(json!({ "user": public_user(user.clone()) })));
        }
    };
    let jar = jar
        .add(session_cookie(token, app.secure))
        .add(csrf_cookie(random_token(), app.secure));
    (jar, Json(json!({ "user": public_user(user.clone()) })))
}

fn public_user(u: User) -> Value {
    json!({ "id": u.id, "name": u.name, "email": u.email, "is_admin": u.is_admin, "is_guest": u.is_guest })
}

fn internal<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Internal(e.to_string())
}
