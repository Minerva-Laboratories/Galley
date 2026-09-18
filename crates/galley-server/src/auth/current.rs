//! Request-time identity. This module holds the cookie-backed session extractors and the CSRF
//! check. The routes check the role on each project with explicit calls to `AppState::access`.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use time::Duration as CookieDuration;

use crate::store::Store;

pub const SESSION_COOKIE: &str = "galley_session";
pub const CSRF_COOKIE: &str = "galley_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The signed-in user, or 401 if there is no valid session.
pub struct CurrentUser(pub crate::store::User);

/// The signed-in user, if there is one. This extractor always succeeds.
pub struct MaybeUser(pub Option<crate::store::User>);

impl<S> FromRequestParts<S> for MaybeUser
where
    Store: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let store = Store::from_ref(state);
        // A bearer device token from an MCP client or a runner is checked first. It never falls
        // back to a cookie, so an invalid token cannot ride on a browser session.
        if let Some(bearer) = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
        {
            let user = tokio::task::spawn_blocking(move || store.device_token_user(&bearer))
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();
            return Ok(MaybeUser(user));
        }
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
            return Ok(MaybeUser(None));
        };
        let user = tokio::task::spawn_blocking(move || store.session_user(&token))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten();
        Ok(MaybeUser(user))
    }
}

impl<S> FromRequestParts<S> for CurrentUser
where
    Store: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let MaybeUser(user) = MaybeUser::from_request_parts(parts, state).await.unwrap_or(MaybeUser(None));
        match user {
            Some(u) => Ok(CurrentUser(u)),
            None => Err((
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({ "error": "Sign in to continue." })),
            )
                .into_response()),
        }
    }
}

pub fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(SESSION_COOKIE, token);
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    c.set_path("/");
    c.set_secure(secure);
    c.set_max_age(CookieDuration::days(30));
    c
}

/// The SPA can read the CSRF cookie, because it is not HttpOnly. The SPA echoes the value in a header.
pub fn csrf_cookie(token: String, secure: bool) -> Cookie<'static> {
    let mut c = Cookie::new(CSRF_COOKIE, token);
    c.set_http_only(false);
    c.set_same_site(SameSite::Strict);
    c.set_path("/");
    c.set_secure(secure);
    c.set_max_age(CookieDuration::days(30));
    c
}

pub fn expired(name: &'static str) -> Cookie<'static> {
    let mut c = Cookie::new(name, "");
    c.set_path("/");
    c.set_max_age(CookieDuration::seconds(0));
    c
}

/// Double-submit CSRF. An unsafe method must carry an `X-CSRF-Token` header that equals the
/// `galley_csrf` cookie. Safe methods pass through. The login and landing routes also pass
/// through, because they have no cookie yet. A cross-origin page can neither read the cookie nor
/// set the custom header.
pub async fn csrf_guard(request: axum::extract::Request, next: Next) -> Response {
    let method = request.method();
    let unsafe_method = matches!(method, &Method::POST | &Method::PUT | &Method::DELETE | &Method::PATCH);
    let path = request.uri().path();
    let exempt = matches!(
        path,
        "/api/auth/login" | "/api/auth/signup" | "/api/auth/landing"
    );
    // A client sends a bearer token deliberately. A browser never attaches one. A request that
    // carries one is authenticated by that token alone (see MaybeUser), so CSRF cannot apply.
    let bearer = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer ") && !v["Bearer ".len()..].trim().is_empty());
    if !unsafe_method || exempt || bearer || !path.starts_with("/api/") {
        return next.run(request).await;
    }
    let jar = CookieJar::from_headers(request.headers());
    let cookie = jar.get(CSRF_COOKIE).map(|c| c.value().to_string());
    let header = request
        .headers()
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match (cookie, header) {
        (Some(c), Some(h)) if !c.is_empty() && c == h => next.run(request).await,
        _ => (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({ "error": "Your session expired or this request was blocked. Reload the page and try again." })),
        )
            .into_response(),
    }
}
