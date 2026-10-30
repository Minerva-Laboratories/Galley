//! The sign-in flows. They are the first-run admin, email and password login, logout, and the
//! share-link landing page. The landing page asks only for a display name. The session and the
//! CSRF token travel in cookies.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Json};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::{AccessChanged, AppError, AppState};
use crate::limits::{ClientIp, IpKey, Key, RuleId, TOO_MANY};
use crate::auth::current::{csrf_cookie, expired, session_cookie, CurrentUser, MaybeUser, CSRF_COOKIE, SESSION_COOKIE};
use crate::auth::{random_token, Role};
use crate::share_routes::unavailable_share;
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
    // The global signup budget counts accounts actually created: it is looked at here and
    // recorded only after the account exists, so refused or duplicate signups do not use it.
    app.limits.check(RuleId::SignupGlobal, Key::Global).map_err(AppError::TooManyRequests)?;
    // Hashing the password is argon2 work. The permit bounds how much runs at once, and the
    // blocking task holds it until the hash is done, even if the request is dropped. With limits
    // on, a signup waits for it at most `password_wait_secs`, as a login does; a signup refused
    // here creates nothing and does not use the global budget.
    let permit = password_slot(&app).await?;
    let user = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store.create_user(&body.email, &body.name, &body.password, first)
    })
    .await
    .map_err(internal)??;
    app.limits.record(RuleId::SignupGlobal, Key::Global);
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
    client: Option<Extension<ClientIp>>,
    jar: CookieJar,
    Json(body): Json<Login>,
) -> Result<(CookieJar, Json<Value>), AppError> {
    // The login lock per (email, IP) and per email. A refusal is not a failure. The ticket counts
    // as a failure unless it is settled as a success, including when this request is dropped or
    // returns early.
    let ip = client.map_or(IpKey::Unknown, |Extension(ClientIp(ip))| ip);
    let ticket = app.limits.login_begin(&body.email, ip).map_err(AppError::TooManyRequests)?;
    // Every failure runs argon2 once, off the async threads, so an unknown email, a guest and a
    // single sign-on account answer like a wrong password. The permit bounds argon2 work at once;
    // with limits on, a login waits for it at most `password_wait_secs`. A login that gets no
    // slot in time checked no password, so its ticket is released without counting a failure: a
    // flood that fills the slots cannot lock anyone out. Any other early exit after this point
    // (a client that goes away, an error) still counts as a failure when the ticket drops.
    let permit = match password_slot(&app).await {
        Ok(permit) => permit,
        Err(e) => {
            if matches!(e, AppError::TooManyRequests(_)) {
                ticket.released();
            }
            return Err(e);
        }
    };
    let store = app.store.clone();
    let Login { email, password } = body;
    let (user, ok) = tokio::task::spawn_blocking(move || -> Result<(Option<User>, bool), AppError> {
        let _permit = permit;
        let user = store.user_by_email(&email).map_err(internal)?;
        let ok = crate::auth::verify_password_or_dummy(&password, user.as_ref().and_then(|u| u.pw_hash.as_deref()));
        Ok((user, ok))
    })
    .await
    .map_err(internal)??;
    match user {
        Some(u) if ok => {
            ticket.succeeded();
            Ok(sign_in(&app, jar, &u))
        }
        _ => {
            ticket.failed();
            Err(AppError::Unauthorized("That email and password don't match.".into()))
        }
    }
}

/// One of the password slots that bound argon2 work at once. With limits on, the wait is at most
/// `password_wait_secs`, then 429 with Retry-After 1 s; with limits off, it waits as long as it
/// takes.
async fn password_slot(app: &AppState) -> Result<tokio::sync::OwnedSemaphorePermit, AppError> {
    let acquire = app.password_slots.clone().acquire_owned();
    match app.limits.password_wait() {
        Some(wait) => tokio::time::timeout(wait, acquire)
            .await
            .map_err(|_| AppError::TooManyRequests(Duration::from_secs(1)))?,
        None => acquire.await,
    }
    .map_err(internal)
}

pub async fn logout(State(app): State<AppState>, jar: CookieJar) -> (CookieJar, Json<Value>) {
    if let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) {
        let store = app.store.clone();
        let signed_out = tokio::task::spawn_blocking(move || -> Result<Option<String>, crate::db::DbError> {
            let uid = store.session_user(&token)?.map(|u| u.id);
            store.delete_session(&token)?;
            Ok(uid)
        })
        .await;
        // Each of this user's sockets re-checks its own session, so only this session's close.
        if let Ok(Ok(Some(uid))) = signed_out {
            app.access_changed(AccessChanged::User(uid));
        }
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
    let Some((project_id, role, share_id)) = resolved else {
        return Err(invalid_share(&app, body.token.clone()).await);
    };

    // An existing account that is not a guest keeps its stronger role and its current session.
    if let Some(user) = current.filter(|u| !u.is_guest) {
        let store = app.store.clone();
        let (pid, uid) = (project_id.clone(), user.id.clone());
        let (effective, changed) = tokio::task::spawn_blocking(move || -> Result<(Role, bool), AppError> {
            let existing = store.role_of(&pid, &uid).map_err(|e| AppError::Internal(e.to_string()))?;
            let effective = existing.map(|e| e.max(role)).unwrap_or(role);
            store.set_member(&pid, &uid, effective).map_err(|e| AppError::Internal(e.to_string()))?;
            Ok((effective, existing != Some(effective)))
        })
        .await
        .map_err(internal)??;
        if changed {
            // This account's open sockets on the project pick up the stronger role.
            app.access_changed(AccessChanged::Project(project_id.clone()));
        }
        app.store.audit(Some(&project_id), Some(&user), "share.join", Some(effective.as_str()));
        return Ok((jar, Json(json!({ "user": public_user(user), "role": effective, "project_id": project_id }))));
    }

    // Guests per share link, so one leaked link cannot mint guests without limit.
    app.limits.hit(RuleId::JoinLink, Key::Link(share_id.clone())).map_err(AppError::TooManyRequests)?;
    let store = app.store.clone();
    let name = body.name.clone();
    let pid = project_id.clone();
    // The guest and its membership are written together, and only while the link is live. The
    // membership records the link, so revoking it removes this guest.
    let guest = tokio::task::spawn_blocking(move || store.join_as_guest(&share_id, &pid, role, &name))
        .await
        .map_err(internal)??;
    // The link was revoked or expired after it resolved above.
    let Some(guest) = guest else {
        return Err(invalid_share(&app, body.token.clone()).await);
    };
    app.store.audit(Some(&project_id), Some(&guest), "share.join", Some(role.as_str()));

    let (jar, mut value) = sign_in(&app, jar, &guest);
    if let Value::Object(map) = &mut value.0 {
        map.insert("role".into(), json!(role));
        map.insert("project_id".into(), json!(project_id));
    }
    Ok((jar, value))
}

/// Send the person to a page that says what went wrong. The sign-in screen shows the message.
/// The target is a fixed path; only the message is encoded into it.
pub(crate) fn signin_error(message: &str) -> Redirect {
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

/// Why a single sign-on callback did not sign anyone in.
enum OidcRefusal {
    /// Shown on the sign-in screen.
    Message(String),
    /// The global signup budget is full. The sign-in screen says "too many attempts".
    TooMany,
}

impl From<String> for OidcRefusal {
    fn from(message: String) -> Self {
        OidcRefusal::Message(message)
    }
}

/// GET /api/auth/oidc/callback. The provider sends the person back here with a code.
pub async fn oidc_callback(
    State(app): State<AppState>,
    jar: CookieJar,
    Query(back): Query<OidcReturn>,
) -> Response {
    let Some(oidc) = app.oidc.clone() else {
        return (jar, signin_error("Single sign-on is not set up on this server.")).into_response();
    };
    let started_here = jar.get(OIDC_STATE_COOKIE).map(|c| c.value().to_string());
    let jar = jar.remove(oidc_state_cookie(String::new(), app.secure));
    let (Some(code), Some(state)) = (back.code, back.state) else {
        let why = match back.error.as_deref() {
            Some("access_denied") => "Sign-in was cancelled at the provider.",
            _ => "The sign-in provider did not complete the sign-in. Start again.",
        };
        return (jar, signin_error(why)).into_response();
    };
    if started_here.as_deref() != Some(state.as_str()) {
        return (jar, signin_error("This sign-in was started in another browser or has expired. Start again here.")).into_response();
    }
    let identity = match oidc.finish(&code, &state).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = ?e, "single sign-on failed");
            return (jar, signin_error(&e.to_string())).into_response();
        }
    };
    let store = app.store.clone();
    let limits = app.limits.clone();
    let public_signup = app.public_signup;
    let domains_checked = oidc.restricts_domains();
    let resolved = tokio::task::spawn_blocking(move || -> Result<(User, &'static str), OidcRefusal> {
        if let Some(user) = store.user_by_oidc_sub(&identity.subject).map_err(|e| e.to_string())? {
            return Ok((user, "account.signin"));
        }
        let Some(email) = identity.email.clone() else {
            return Err("The provider did not share an email address, which Galley needs for an account. Ask your provider to release it.".to_string().into());
        };
        if let Some(user) = store.user_by_email(&email).map_err(|e| e.to_string())? {
            if !identity.email_verified {
                return Err("An account with this email exists, and the provider has not verified the email. Sign in with your password.".to_string().into());
            }
            store.link_oidc(&user.id, &identity.subject).map_err(|e| e.to_string())?;
            return Ok((user, "account.link_oidc"));
        }
        let first = matches!(store.user_count(), Ok(0));
        if !first && !public_signup && !domains_checked {
            return Err("This server is invite-only. Ask an admin to add you, or use a share link.".to_string().into());
        }
        // The same global signup budget as password signups: looked at before the account is
        // created, and counted only once it exists.
        limits.check(RuleId::SignupGlobal, Key::Global).map_err(|_| OidcRefusal::TooMany)?;
        let name = identity.name.clone().unwrap_or_default();
        let user = store.create_oidc_user(&email, &name, &identity.subject, first).map_err(|e| e.to_string())?;
        limits.record(RuleId::SignupGlobal, Key::Global);
        Ok((user, "account.create_oidc"))
    })
    .await;
    match resolved {
        Ok(Ok((user, action))) => {
            app.store.audit(None, Some(&user), action, None);
            let (jar, _) = sign_in(&app, jar, &user);
            (jar, Redirect::to("/")).into_response()
        }
        Ok(Err(OidcRefusal::Message(message))) => (jar, signin_error(&message)).into_response(),
        Ok(Err(OidcRefusal::TooMany)) => (jar, signin_error(TOO_MANY)).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "single sign-on account lookup failed");
            (jar, signin_error("Something went wrong on the server. Try again.")).into_response()
        }
    }
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

/// The error for a share token that does not resolve, saying whether it was revoked or expired.
async fn invalid_share(app: &AppState, token: String) -> AppError {
    let store = app.store.clone();
    tokio::task::spawn_blocking(move || unavailable_share(&store, &token)).await.unwrap_or_else(internal)
}

fn internal<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Internal(e.to_string())
}
