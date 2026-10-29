//! Request-time identity. This module holds the cookie-backed session extractors and the CSRF
//! check. The routes check the role on each project with explicit calls to `AppState::access`.

use std::sync::Arc;

use axum::extract::{FromRef, FromRequestParts, MatchedPath, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use time::Duration as CookieDuration;

use crate::app::AppError;
use crate::limits::{group_for, Key, Limiter};
use crate::store::Store;

pub const SESSION_COOKIE: &str = "galley_session";
pub const CSRF_COOKIE: &str = "galley_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The signed-in user, or 401 if there is no valid session.
pub struct CurrentUser(pub crate::store::User);

/// The signed-in user, if there is one. It refuses only when the user is over a per-account rate
/// limit (stage 2, see [`count_account`]); an unknown caller is `None`.
pub struct MaybeUser(pub Option<crate::store::User>);

/// Marks a request whose account was already counted, so a handler with two user extractors
/// counts once.
#[derive(Clone, Copy)]
struct Counted;

/// Stage 2 of the rate limits: count this request against the account's rule for its group, once
/// per request. Minting tokens or sessions does not reset it, because the key is the user id.
/// Sockets and MCP count elsewhere (`ws::authenticate`, `mcp_routes::rpc`).
fn count_account(parts: &mut Parts, limiter: &Limiter, user: &crate::store::User) -> Result<(), AppError> {
    if parts.extensions.get::<Counted>().is_some() {
        return Ok(());
    }
    parts.extensions.insert(Counted);
    let group = group_for(&parts.method, parts.extensions.get::<MatchedPath>().map(MatchedPath::as_str));
    match group.account_rule() {
        Some(rule) => limiter.hit(rule, Key::Account(user.id.clone())).map_err(AppError::TooManyRequests),
        None => Ok(()),
    }
}

impl<S> FromRequestParts<S> for MaybeUser
where
    Store: FromRef<S>,
    Arc<Limiter>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = Self::resolve(parts, state).await;
        if let Some(u) = &user {
            count_account(parts, &Arc::<Limiter>::from_ref(state), u)?;
        }
        Ok(MaybeUser(user))
    }
}

impl MaybeUser {
    /// The caller by bearer token or session cookie, or `None`.
    async fn resolve<S>(parts: &Parts, state: &S) -> Option<crate::store::User>
    where
        Store: FromRef<S>,
        S: Send + Sync,
    {
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
            return tokio::task::spawn_blocking(move || store.device_token_user(&bearer))
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();
        }
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(SESSION_COOKIE).map(|c| c.value().to_string())?;
        tokio::task::spawn_blocking(move || store.session_user(&token))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    }
}

impl<S> FromRequestParts<S> for CurrentUser
where
    Store: FromRef<S>,
    Arc<Limiter>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // A rate-limit refusal stays a 429; only a missing user is a 401.
        let MaybeUser(user) = MaybeUser::from_request_parts(parts, state).await.map_err(IntoResponse::into_response)?;
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

/// An origin split into its parts: the scheme, the lowercased host, and the port, with the
/// scheme's default filled in.
#[derive(Debug, PartialEq, Eq)]
struct ParsedOrigin {
    scheme: &'static str,
    host: String,
    port: u16,
}

impl ParsedOrigin {
    fn default_port(scheme: &str) -> u16 {
        if scheme == "https" { 443 } else { 80 }
    }

    /// `host[:port]`, with the port left out when it is the scheme's default.
    fn authority(&self) -> String {
        if self.port == Self::default_port(self.scheme) {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Parse `scheme://host[:port]`, with scheme `http` or `https` and nothing after the authority.
/// Anything else, `null` included, is `None`.
fn parse_origin(value: &str) -> Option<ParsedOrigin> {
    let (scheme, rest) = value.split_once("://")?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" => "http",
        "https" => "https",
        _ => return None,
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '\\']) || rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
        // An IPv6 literal, such as [::1]:7000.
        let (inner, after) = bracketed.split_once(']')?;
        if inner.is_empty() || !inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') {
            return None;
        }
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (format!("[{}]", inner.to_ascii_lowercase()), port)
    } else {
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_') {
            return None;
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None => ParsedOrigin::default_port(scheme),
        Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => p.parse().ok()?,
        Some(_) => return None,
    };
    Some(ParsedOrigin { scheme, host, port })
}

/// Whether a request's `Origin` may reach a browser-reachable route outside the CSRF-checked API.
/// No `Origin` is allowed, because CLI tools and MCP clients send none. With a public origin, only
/// that exact origin is allowed. Without one (local use), the origin's `host[:port]` must equal
/// the `Host` header, or the URI authority when there is no `Host`.
pub fn origin_allowed(headers: &HeaderMap, uri_authority: Option<&str>, public_origin: Option<&str>) -> bool {
    let mut values = headers.get_all(axum::http::header::ORIGIN).iter();
    let Some(value) = values.next() else { return true };
    if values.next().is_some() {
        return false;
    }
    let Some(origin) = value.to_str().ok().and_then(parse_origin) else { return false };
    match public_origin {
        Some(public) => parse_origin(public).is_some_and(|p| p.scheme == "https" && p == origin),
        None => {
            let host = match headers.get(axum::http::header::HOST) {
                Some(h) => match h.to_str() {
                    Ok(h) => h,
                    Err(_) => return false,
                },
                None => match uri_authority {
                    Some(a) => a,
                    None => return false,
                },
            };
            let host = host.trim().to_ascii_lowercase();
            let default = format!(":{}", ParsedOrigin::default_port(origin.scheme));
            let host = host.strip_suffix(default.as_str()).unwrap_or(&host);
            !host.is_empty() && host == origin.authority()
        }
    }
}

/// Refuse a cross-site `Origin` on the WebSocket and MCP routes, which sit outside the
/// CSRF-checked API and accept the session cookie. It runs as a route layer, before any extractor,
/// so a refused request costs no lookup. On `/mcp/` only, a Bearer request skips the check: a
/// browser cannot attach that header cross-origin without a CORS preflight, and none is allowed.
/// Sockets authenticate by cookie only, so a Bearer header never skips the check there.
pub async fn origin_guard(
    State(app): State<crate::app::AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let bearer = request.uri().path().starts_with("/mcp/")
        && request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer ") && !v["Bearer ".len()..].trim().is_empty());
    if bearer || origin_allowed(request.headers(), request.uri().authority().map(|a| a.as_str()), app.public_origin.as_deref()) {
        return next.run(request).await;
    }
    crate::app::AppError::Forbidden("Cross-site connections are not allowed.".into()).into_response()
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    fn headers(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = origin {
            h.insert(axum::http::header::ORIGIN, o.parse().unwrap());
        }
        if let Some(host) = host {
            h.insert(axum::http::header::HOST, host.parse().unwrap());
        }
        h
    }

    const PUBLIC: Option<&str> = Some("https://galley.example");

    #[test]
    fn no_origin_is_allowed() {
        assert!(origin_allowed(&headers(None, Some("galley.example")), None, PUBLIC));
        assert!(origin_allowed(&headers(None, None), None, None));
    }

    #[test]
    fn with_a_domain_only_that_https_origin_is_allowed() {
        let ok = |o: &str| origin_allowed(&headers(Some(o), Some("galley.example")), None, PUBLIC);
        assert!(ok("https://galley.example"));
        assert!(ok("https://Galley.Example"), "case");
        assert!(ok("HTTPS://galley.example"), "scheme case");
        assert!(ok("https://galley.example:443"), "the default port written out");
        assert!(!ok("https://evil.example"), "another host");
        assert!(!ok("https://galley.example.evil.example"));
        assert!(!ok("https://app.galley.example"), "a sibling subdomain");
        assert!(!ok("https://galley.example:8443"), "another port");
        assert!(!ok("http://galley.example"), "another scheme");
        assert!(!ok("null"));
        assert!(!ok("ftp://galley.example"));
        assert!(!ok("https://galley.example/"), "a path");
        assert!(!ok("https://galley.example/x"));
        assert!(!ok("https://user@galley.example"));
        assert!(!ok("https://galley.example:"));
        assert!(!ok("galley.example"));
        assert!(!ok(""));
        // The Host header does not matter with a domain set.
        assert!(!origin_allowed(&headers(Some("https://evil.example"), Some("evil.example")), None, PUBLIC));
        // The configured domain is normalized the same way.
        assert!(origin_allowed(&headers(Some("https://galley.example"), None), None, Some("https://Galley.Example:443")));
    }

    #[test]
    fn ipv6_literals_parse() {
        assert_eq!(
            parse_origin("http://[::1]:7000"),
            Some(ParsedOrigin { scheme: "http", host: "[::1]".into(), port: 7000 })
        );
        assert_eq!(parse_origin("https://[::1]").map(|o| o.port), Some(443));
        assert!(parse_origin("http://[::1").is_none());
        assert!(parse_origin("http://[::1]x").is_none());
        assert!(origin_allowed(&headers(Some("http://[::1]:7000"), Some("[::1]:7000")), None, None));
        assert!(!origin_allowed(&headers(Some("http://[::1]:7000"), Some("[::1]:7001")), None, None));
    }

    #[test]
    fn without_a_domain_the_origin_must_match_the_host() {
        let ok = |o: &str, host: Option<&str>, authority: Option<&str>| origin_allowed(&headers(Some(o), host), authority, None);
        assert!(ok("http://127.0.0.1:7000", Some("127.0.0.1:7000"), None));
        assert!(ok("http://LOCALHOST:7000", Some("localhost:7000"), None), "case");
        assert!(ok("http://localhost:5173", Some("localhost:5173"), None), "the Vite dev proxy keeps the Host");
        assert!(ok("http://localhost", Some("localhost:80"), None), "a default port on Host");
        assert!(ok("http://localhost:80", Some("localhost"), None), "a default port on Origin");
        assert!(ok("https://box.lan", Some("box.lan:443"), None));
        assert!(!ok("http://127.0.0.1:7001", Some("127.0.0.1:7000"), None), "another port");
        assert!(!ok("https://evil.example", Some("127.0.0.1:7000"), None), "another host");
        assert!(!ok("http://localhost.evil.example:7000", Some("localhost:7000"), None));
        assert!(!ok("null", Some("127.0.0.1:7000"), None));
        assert!(!ok("http://127.0.0.1:7000/", Some("127.0.0.1:7000"), None));
        // Without Host (HTTP/2), the URI authority stands in.
        assert!(ok("http://127.0.0.1:7000", None, Some("127.0.0.1:7000")));
        assert!(!ok("http://127.0.0.1:7000", None, Some("127.0.0.1:7001")));
        assert!(!ok("http://127.0.0.1:7000", None, None), "nothing to compare with");
    }
}
