//! The sign-in flows. They are the first-run admin, email and password login, logout, and the
//! share-link landing page. The landing page asks only for a display name. The session and the
//! CSRF token travel in cookies.

use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::{csrf_cookie, expired, session_cookie, CurrentUser, MaybeUser, CSRF_COOKIE, SESSION_COOKIE};
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
            "oidc": app.oidc.as_ref().map(|o| json!({ "label": o.label })),
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
/// Send the person to a page that says what went wrong. The sign-in screen shows the message.
fn signin_error(message: &str) -> Redirect {
    let encoded: String = message
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Redirect::to(&format!("/?signin_error={encoded}"))
}

const OIDC_STATE_COOKIE: &str = "galley_oidc_state";

/// The state also rides in a cookie of the browser that started the sign-in. Without it, someone
/// could finish a sign-in with their own account and send the callback link to someone else, who
/// would then work inside the sender's account without noticing.
fn oidc_state_cookie(state: String, secure: bool) -> axum_extra::extract::cookie::Cookie<'static> {
    let mut c = axum_extra::extract::cookie::Cookie::new(OIDC_STATE_COOKIE, state);
    c.set_http_only(true);
    c.set_same_site(axum_extra::extract::cookie::SameSite::Lax);
    c.set_path("/api/auth/oidc");
    c.set_secure(secure);
    c.set_max_age(time::Duration::minutes(10));
    c
}

/// GET /api/auth/oidc/start. Off to the provider's sign-in page.
pub async fn oidc_start(State(app): State<AppState>, jar: CookieJar) -> (CookieJar, Redirect) {
    let Some(oidc) = app.oidc.clone() else {
        return (jar, signin_error("Single sign-on is not set up on this server."));
    };
    match oidc.start().await {
        Ok((url, state)) => (jar.add(oidc_state_cookie(state, app.secure)), Redirect::to(&url)),
        Err(e) => {
            tracing::warn!(error = ?e, "single sign-on could not start");
            (jar, signin_error(&e.to_string()))
        }
    }
}

#[derive(Deserialize)]
pub struct OidcReturn {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// GET /api/auth/oidc/callback. The provider sends the person back here with a code.
pub async fn oidc_callback(
    State(app): State<AppState>,
    jar: CookieJar,
    Query(back): Query<OidcReturn>,
) -> (CookieJar, Redirect) {
    let Some(oidc) = app.oidc.clone() else {
        return (jar, signin_error("Single sign-on is not set up on this server."));
    };
    let started_here = jar.get(OIDC_STATE_COOKIE).map(|c| c.value().to_string());
    let jar = jar.remove(oidc_state_cookie(String::new(), app.secure));
    let (Some(code), Some(state)) = (back.code, back.state) else {
        let why = match back.error.as_deref() {
            Some("access_denied") => "Sign-in was cancelled at the provider.",
            _ => "The sign-in provider did not complete the sign-in. Start again.",
        };
        return (jar, signin_error(why));
    };
    if started_here.as_deref() != Some(state.as_str()) {
        return (jar, signin_error("This sign-in was started in another browser or has expired. Start again here."));
    }
    let identity = match oidc.finish(&code, &state).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = ?e, "single sign-on failed");
            return (jar, signin_error(&e.to_string()));
        }
    };
    let store = app.store.clone();
    let public_signup = app.public_signup;
    let domains_checked = oidc.restricts_domains();
    let resolved = tokio::task::spawn_blocking(move || -> Result<(User, &'static str), String> {
        if let Some(user) = store.user_by_oidc_sub(&identity.subject).map_err(|e| e.to_string())? {
            return Ok((user, "account.signin"));
        }
        let Some(email) = identity.email.clone() else {
            return Err("The provider did not share an email address, which Galley needs for an account. Ask your provider to release it.".into());
        };
        if let Some(user) = store.user_by_email(&email).map_err(|e| e.to_string())? {
            if !identity.email_verified {
                return Err("An account with this email exists, and the provider has not verified the email. Sign in with your password.".into());
            }
            store.link_oidc(&user.id, &identity.subject).map_err(|e| e.to_string())?;
            return Ok((user, "account.link_oidc"));
        }
        let first = matches!(store.user_count(), Ok(0));
        if !first && !public_signup && !domains_checked {
            return Err("This server is invite-only. Ask an admin to add you, or use a share link.".into());
        }
        let name = identity.name.clone().unwrap_or_default();
        let user = store.create_oidc_user(&email, &name, &identity.subject, first).map_err(|e| e.to_string())?;
        Ok((user, "account.create_oidc"))
    })
    .await;
    match resolved {
        Ok(Ok((user, action))) => {
            app.store.audit(None, Some(&user), action, None);
            let (jar, _) = sign_in(&app, jar, &user);
            (jar, Redirect::to("/"))
        }
        Ok(Err(message)) => (jar, signin_error(&message)),
        Err(e) => {
            tracing::error!(error = %e, "single sign-on account lookup failed");
            (jar, signin_error("Something went wrong on the server. Try again."))
        }
    }
}

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

#[derive(Deserialize)]
pub struct NewName {
    name: String,
}

/// POST /api/auth/name. Change the display name of the signed-in user, guest or not. The server
/// attributes commits, comments and the member list to this name, so a rename in the UI has to
/// reach it. A user renames only themselves: the handler takes the id from the session.
pub async fn rename(
    State(app): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewName>,
) -> Result<Json<Value>, AppError> {
    let store = app.store.clone();
    let id = user.id.clone();
    let name = body.name.clone();
    let name = tokio::task::spawn_blocking(move || store.set_user_name(&id, &name))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    app.store.audit(None, Some(&user), "user.rename", Some(&name));
    Ok(Json(public_user(User { name, ..user })))
}

fn public_user(u: User) -> Value {
    json!({ "id": u.id, "name": u.name, "email": u.email, "is_admin": u.is_admin, "is_guest": u.is_guest })
}

fn internal<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Internal(e.to_string())
}
