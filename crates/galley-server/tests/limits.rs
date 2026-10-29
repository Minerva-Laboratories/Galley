//! The rate limits, stage 1: the policy table against the real router, the per-IP rules, the
//! client-IP header trust, and the 429 answer. These tests turn the limits on; every other test
//! server runs with them off.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath, Request};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use galley_server::config::{LimitsConfig, Rule};
use galley_server::limits::{group_for, Group, TOO_MANY};
use galley_server::{router, AppState, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The peer most tests connect from: a private address, as a proxy in front of Galley would be,
/// so the `Fly-Client-IP` header is trusted.
const PROXY: [u8; 4] = [10, 0, 0, 1];

/// Limits on, with the client IP from `Fly-Client-IP`, the signup rules off, and then `adjust`.
async fn state_with(adjust: impl FnOnce(&mut LimitsConfig)) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits = LimitsConfig {
        enabled: true,
        client_ip_header: "Fly-Client-IP".into(),
        signup_ip: Rule::new(0, 3600),
        signup_global: Rule::new(0, 3600),
        ..Default::default()
    };
    adjust(&mut config.limits);
    (AppState::new(dir.path(), &config).await.unwrap(), dir)
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

/// One request through a fresh router, from `peer` (none means no `ConnectInfo`, as when the
/// server is not run by `serve`), with `Fly-Client-IP: ip` when given.
async fn send(state: &AppState, peer: Option<[u8; 4]>, ip: Option<&str>, method: Method, path: &str, body: Option<Value>) -> Answer {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(ip) = ip {
        builder = builder.header("Fly-Client-IP", ip);
    }
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let mut request = builder.body(body).unwrap();
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(SocketAddr::from((peer, 1))));
    }
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer { status, headers, body }
}

/// A request from client `ip` behind the private proxy.
async fn rest_from(state: &AppState, ip: &str, method: Method, path: &str, body: Option<Value>) -> Answer {
    send(state, Some(PROXY), Some(ip), method, path, body).await
}

fn assert_refused(a: &Answer, max_wait: u64) {
    assert_eq!(a.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(a.body, json!({ "error": "Too many attempts. Try again in a few moments." }));
    let secs: u64 = a.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=max_wait).contains(&secs), "Retry-After {secs} not in 1..={max_wait}");
}

/// Answers with the group the policy table gives the request, and runs nothing else.
async fn record(request: Request, _next: Next) -> Response {
    let group = group_for(request.method(), request.extensions().get::<MatchedPath>().map(MatchedPath::as_str));
    (StatusCode::NO_CONTENT, [("x-group", format!("{group:?}"))]).into_response()
}

#[tokio::test]
async fn every_route_in_the_table_lands_in_its_group() {
    let (state, _dir) = state_with(|_| {}).await;
    let app = router(state).layer(middleware::from_fn(record));
    let rows: &[(Method, &str, Group)] = &[
        (Method::POST, "/api/auth/login", Group::Login),
        (Method::POST, "/api/auth/signup", Group::Signup),
        (Method::POST, "/api/auth/landing", Group::Join),
        (Method::GET, "/api/share/sometoken", Group::SharePreview),
        (Method::HEAD, "/api/share/sometoken", Group::SharePreview),
        (Method::GET, "/api/auth/oidc/start", Group::Oidc),
        (Method::GET, "/api/auth/oidc/callback", Group::Oidc),
        (Method::POST, "/api/projects/paper/build", Group::Build),
        (Method::POST, "/api/projects/paper/latexdiff", Group::Build),
        (Method::POST, "/api/projects/paper/pack", Group::Build),
        (Method::POST, "/api/projects/import", Group::Import),
        (Method::POST, "/mcp/paper", Group::Mcp),
        (Method::POST, "/api/auth/tokens", Group::TokenCreate),
        (Method::POST, "/api/projects", Group::ProjectCreate),
        (Method::POST, "/api/projects/paper/members", Group::Invite),
        (Method::POST, "/api/projects/paper/grammar", Group::Lookup),
        (Method::GET, "/api/projects/paper/bib/search", Group::Lookup),
        (Method::POST, "/api/projects/paper/bib/smith2020/lookup", Group::Lookup),
        (Method::POST, "/api/projects/paper/citations/candidates", Group::Lookup),
        (Method::POST, "/api/projects/paper/bib", Group::Lookup),
        (Method::GET, "/ws/paper/events", Group::Socket),
        (Method::GET, "/ws/paper/doc/main.tex", Group::Socket),
        (Method::GET, "/ws/paper/doc/chapters/one.tex", Group::Socket),
        // Everything else.
        (Method::GET, "/api/projects/paper/latexdiff/pdf", Group::Default),
        (Method::GET, "/api/projects/paper/build", Group::Default),
        (Method::GET, "/api/projects/paper/bib", Group::Default),
        (Method::GET, "/api/auth/tokens", Group::Default),
        (Method::GET, "/api/projects", Group::Default),
        (Method::GET, "/mcp/paper", Group::Default),
        (Method::GET, "/api/auth/login", Group::Default),
        (Method::DELETE, "/api/projects/paper", Group::Default),
        (Method::GET, "/api/health", Group::Default),
        (Method::GET, "/api/no/such/route", Group::Default),
        (Method::GET, "/", Group::Default),
        (Method::GET, "/assets/index.js", Group::Default),
    ];
    for (method, path, want) in rows {
        let response = app
            .clone()
            .oneshot(Request::builder().method(method.clone()).uri(*path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{method} {path} reached a handler");
        assert_eq!(response.headers()["x-group"], format!("{want:?}").as_str(), "{method} {path}");
    }
}

#[tokio::test]
async fn an_ip_over_its_login_limit_gets_429_and_another_ip_does_not() {
    let (state, _dir) = state_with(|l| l.login_ip = Rule::new(3, 600)).await;
    // An empty body fails before any password work; stage 1 counts it all the same.
    for _ in 0..3 {
        let a = rest_from(&state, "198.51.100.1", Method::POST, "/api/auth/login", Some(json!({}))).await;
        assert_ne!(a.status, StatusCode::TOO_MANY_REQUESTS);
    }
    let a = rest_from(&state, "198.51.100.1", Method::POST, "/api/auth/login", Some(json!({}))).await;
    assert_refused(&a, 600);
    let b = rest_from(&state, "198.51.100.2", Method::POST, "/api/auth/login", Some(json!({}))).await;
    assert_ne!(b.status, StatusCode::TOO_MANY_REQUESTS, "another client keeps its own budget");
    // Other groups are not affected by the login rule.
    let health = rest_from(&state, "198.51.100.1", Method::GET, "/api/health", None).await;
    assert_eq!(health.status, StatusCode::OK);
}

#[tokio::test]
async fn the_client_ip_header_is_believed_only_from_a_private_peer() {
    let login = |state: AppState, peer: Option<[u8; 4]>, ip: &'static str| async move {
        send(&state, peer, Some(ip), Method::POST, "/api/auth/login", Some(json!({}))).await.status
    };

    // A public peer: the header is ignored, so both "clients" share the peer's budget.
    let (state, _dir) = state_with(|l| l.login_ip = Rule::new(1, 600)).await;
    assert_ne!(login(state.clone(), Some([203, 0, 113, 5]), "198.51.100.1").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(login(state.clone(), Some([203, 0, 113, 5]), "198.51.100.2").await, StatusCode::TOO_MANY_REQUESTS);

    // A private peer: the header names the client.
    let (state, _dir) = state_with(|l| l.login_ip = Rule::new(1, 600)).await;
    assert_ne!(login(state.clone(), Some(PROXY), "198.51.100.1").await, StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(login(state.clone(), Some(PROXY), "198.51.100.2").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(login(state.clone(), Some(PROXY), "198.51.100.1").await, StatusCode::TOO_MANY_REQUESTS);

    // No peer at all: nothing is believed, and every such request shares one key.
    let (state, _dir) = state_with(|l| l.login_ip = Rule::new(1, 600)).await;
    assert_ne!(login(state.clone(), None, "198.51.100.1").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(login(state.clone(), None, "198.51.100.2").await, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn every_stage_one_group_can_refuse() {
    type Adjust = fn(&mut LimitsConfig);
    let cases: &[(&str, Adjust, Method, &str, Option<Value>)] = &[
        ("signup_ip", |l| l.signup_ip = Rule::new(1, 600), Method::POST, "/api/auth/signup", Some(json!({}))),
        ("join_ip", |l| l.join_ip = Rule::new(1, 600), Method::POST, "/api/auth/landing", Some(json!({}))),
        ("share_preview_ip", |l| l.share_preview_ip = Rule::new(1, 600), Method::GET, "/api/share/no-such-token", None),
        ("oidc_ip", |l| l.oidc_ip = Rule::new(1, 600), Method::GET, "/api/auth/oidc/start", None),
        ("oidc_ip", |l| l.oidc_ip = Rule::new(1, 600), Method::GET, "/api/auth/oidc/callback?code=c&state=s", None),
        ("build_ip", |l| l.build_ip = Rule::new(1, 600), Method::POST, "/api/projects/paper/build", Some(json!({}))),
        ("import_ip", |l| l.import_ip = Rule::new(1, 600), Method::POST, "/api/projects/import", None),
        ("mcp_ip", |l| l.mcp_ip = Rule::new(1, 600), Method::POST, "/mcp/paper", Some(json!({}))),
        ("socket_ip", |l| l.socket_ip = Rule::new(1, 600), Method::GET, "/ws/paper/events", None),
        ("default_ip", |l| l.default_ip = Rule::new(1, 600), Method::GET, "/api/health", None),
    ];
    for (name, adjust, method, path, body) in cases {
        let (state, _dir) = state_with(*adjust).await;
        let first = rest_from(&state, "198.51.100.1", method.clone(), path, body.clone()).await;
        assert_ne!(first.status, StatusCode::TOO_MANY_REQUESTS, "{name}: the first request is allowed");
        let second = rest_from(&state, "198.51.100.1", method.clone(), path, body.clone()).await;
        if *name == "oidc_ip" {
            // Browser navigations go back to the sign-in screen with the message, to a fixed path.
            assert_eq!(second.status, StatusCode::SEE_OTHER, "{path}");
            assert_eq!(second.headers["location"], SIGNIN_TOO_MANY, "{path}");
            assert!(!second.headers.contains_key("set-cookie"), "{path}");
            continue;
        }
        assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS, "{name}");
        assert_refused(&second, 600);
    }
}

/// Where a refused single sign-on navigation is sent.
const SIGNIN_TOO_MANY: &str = "/?signin_error=Too+many+attempts.+Try+again+in+a+few+moments.";

#[tokio::test]
async fn join_global_is_shared_by_every_client() {
    let (state, _dir) = state_with(|l| l.join_global = Rule::new(2, 600)).await;
    for ip in ["198.51.100.1", "198.51.100.2"] {
        let a = rest_from(&state, ip, Method::POST, "/api/auth/landing", Some(json!({}))).await;
        assert_ne!(a.status, StatusCode::TOO_MANY_REQUESTS);
    }
    let a = rest_from(&state, "198.51.100.3", Method::POST, "/api/auth/landing", Some(json!({}))).await;
    assert_refused(&a, 600);
}

#[tokio::test]
async fn signup_global_is_checked_before_the_handler() {
    let (state, _dir) = state_with(|l| l.signup_global = Rule::new(1, 600)).await;
    let a = rest_from(&state, "198.51.100.1", Method::POST, "/api/auth/signup", Some(json!({}))).await;
    assert_ne!(a.status, StatusCode::TOO_MANY_REQUESTS, "a failed signup does not use the budget");
    state.limits.record(galley_server::limits::RuleId::SignupGlobal, galley_server::limits::Key::Global);
    let a = rest_from(&state, "198.51.100.2", Method::POST, "/api/auth/signup", Some(json!({}))).await;
    assert_refused(&a, 600);
}

#[tokio::test]
async fn disabled_limits_never_answer_429() {
    let (state, _dir) = state_with(|l| {
        l.enabled = false;
        l.login_ip = Rule::new(1, 600);
        l.default_ip = Rule::new(1, 600);
        l.socket_ip = Rule::new(1, 600);
    })
    .await;
    for _ in 0..10 {
        for (method, path) in [(Method::POST, "/api/auth/login"), (Method::GET, "/api/health"), (Method::GET, "/ws/paper/events")] {
            let a = send(&state, None, None, method, path, Some(json!({}))).await;
            assert_ne!(a.status, StatusCode::TOO_MANY_REQUESTS, "{path}");
        }
    }
}

#[tokio::test]
async fn a_real_socket_upgrade_over_its_limit_is_refused_with_429() {
    let (state, _dir) = state_with(|l| {
        l.client_ip_header = String::new();
        l.socket_ip = Rule::new(1, 600);
    })
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("ws://{addr}/ws/paper/events");
    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_ne!(r.status(), StatusCode::TOO_MANY_REQUESTS),
        Err(other) => panic!("expected an HTTP refusal without a session, got {other}"),
        Ok(_) => panic!("an anonymous socket connected"),
    }
    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
            let secs: u64 = r.headers()["retry-after"].to_str().unwrap().parse().unwrap();
            assert!((1..=600).contains(&secs));
            let body: Value = serde_json::from_slice(r.body().as_deref().unwrap_or_default()).unwrap();
            assert_eq!(body, json!({ "error": TOO_MANY }));
        }
        Err(other) => panic!("expected a 429 handshake, got {other}"),
        Ok(_) => panic!("the second upgrade connected"),
    }
}

// ---- Stage 2: per account, the login lock, signups, joins, sockets and MCP ----------------------

/// How a request authenticates.
#[derive(Clone)]
enum Who {
    Anonymous,
    /// A session cookie, with a CSRF cookie and header for writes.
    Cookie(String),
    /// A device token.
    Bearer(String),
}

const CSRF: &str = "test-csrf-token";

/// One request from client `ip` behind the private proxy, as `who`.
async fn call(state: &AppState, ip: &str, who: &Who, method: Method, path: &str, body: Option<Value>) -> Answer {
    let mut builder = Request::builder().method(method).uri(path).header("Fly-Client-IP", ip);
    match who {
        Who::Anonymous => {}
        Who::Cookie(session) => {
            builder = builder.header("cookie", format!("galley_session={session}; galley_csrf={CSRF}")).header("x-csrf-token", CSRF);
        }
        Who::Bearer(token) => builder = builder.header("authorization", format!("Bearer {token}")),
    }
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let mut request = builder.body(body).unwrap();
    request.extensions_mut().insert(ConnectInfo(SocketAddr::from((PROXY, 1))));
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer { status, headers, body }
}

/// A user with a session.
fn user(state: &AppState, email: &str) -> (galley_server::store::User, Who) {
    let u = state.store.create_user(email, "Someone", "correct horse battery", false).unwrap();
    let session = state.store.create_session(&u.id, 1).unwrap();
    (u, Who::Cookie(session))
}

fn token(state: &AppState, user_id: &str) -> Who {
    Who::Bearer(state.store.create_device_token(user_id, "test").unwrap().1)
}

async fn login(state: &AppState, ip: &str, email: &str, password: &str) -> Answer {
    call(state, ip, &Who::Anonymous, Method::POST, "/api/auth/login", Some(json!({ "email": email, "password": password }))).await
}

#[tokio::test]
async fn the_login_lock_holds_per_email_and_ip_for_known_and_unknown_emails() {
    let (state, _dir) = state_with(|l| l.login_failures = 2).await;
    user(&state, "ann@example.org");
    for _ in 0..2 {
        let a = login(&state, "198.51.100.1", "ann@example.org", "wrong password").await;
        assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    }
    // Locked: even the right password from the same IP is refused, and it is not a failure.
    let a = login(&state, "198.51.100.1", "Ann@Example.org", "correct horse battery").await;
    assert_refused(&a, 900);
    // Another IP is not locked.
    let b = login(&state, "198.51.100.2", "ann@example.org", "correct horse battery").await;
    assert_eq!(b.status, StatusCode::OK);
    // An email with no account locks the same way, so a lock says nothing about who exists.
    for _ in 0..2 {
        let a = login(&state, "198.51.100.1", "nobody@example.org", "wrong password").await;
        assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    }
    assert_refused(&login(&state, "198.51.100.1", "nobody@example.org", "wrong password").await, 900);
    // After the lock ends, the right password works and clears the record.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let a = login(&state, "198.51.100.1", "ann@example.org", "correct horse battery").await;
    assert_eq!(a.status, StatusCode::OK);
    let a = login(&state, "198.51.100.1", "ann@example.org", "wrong password").await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED, "the success cleared the failures");
}

#[tokio::test]
async fn accounts_are_counted_by_user_whatever_the_token() {
    let (state, _dir) = state_with(|l| l.project_create_account = Rule::new(2, 3600)).await;
    let (ann, cookie) = user(&state, "ann@example.org");
    let new = |n: &str| Some(json!({ "name": n }));
    let a = call(&state, "198.51.100.1", &cookie, Method::POST, "/api/projects", new("One")).await;
    assert_eq!(a.status, StatusCode::CREATED);
    let a = call(&state, "198.51.100.2", &token(&state, &ann.id), Method::POST, "/api/projects", new("Two")).await;
    assert_eq!(a.status, StatusCode::CREATED);
    // A newly minted token buys no extra budget, and the refusal is a 429, not a 401.
    let a = call(&state, "198.51.100.3", &token(&state, &ann.id), Method::POST, "/api/projects", new("Three")).await;
    assert_refused(&a, 3600);
    let (_, bob) = user(&state, "bob@example.org");
    let a = call(&state, "198.51.100.1", &bob, Method::POST, "/api/projects", new("Bob's")).await;
    assert_eq!(a.status, StatusCode::CREATED, "another user has their own budget");
    // Unauthenticated requests never reach stage 2.
    for _ in 0..3 {
        let a = call(&state, "198.51.100.9", &Who::Bearer("not-a-token".into()), Method::POST, "/api/projects", new("x")).await;
        assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_second_build_over_the_account_limit_is_refused() {
    let (state, _dir) = state_with(|l| l.build_account = Rule::new(1, 600)).await;
    let (ann, cookie) = user(&state, "ann@example.org");
    let meta = state.registry.create("Paper").await.unwrap();
    state.store.set_member(&meta.id, &ann.id, galley_server::auth::Role::Admin).unwrap();
    let path = format!("/api/projects/{}/build", meta.id);
    let first = call(&state, "198.51.100.1", &cookie, Method::POST, &path, Some(json!({}))).await;
    assert_ne!(first.status, StatusCode::TOO_MANY_REQUESTS);
    let second = call(&state, "198.51.100.2", &cookie, Method::POST, &path, Some(json!({}))).await;
    assert_refused(&second, 600);
}

#[tokio::test]
async fn every_account_group_can_refuse() {
    type Adjust = fn(&mut LimitsConfig);
    let cases: &[(&str, Adjust, Method, &str, Option<Value>)] = &[
        ("token_create_account", |l| l.token_create_account = Rule::new(1, 600), Method::POST, "/api/auth/tokens", Some(json!({}))),
        ("invite_account", |l| l.invite_account = Rule::new(1, 600), Method::POST, "/api/projects/paper/members", Some(json!({}))),
        ("lookup_account", |l| l.lookup_account = Rule::new(1, 600), Method::POST, "/api/projects/paper/grammar", Some(json!({}))),
        ("lookup_account search", |l| l.lookup_account = Rule::new(1, 600), Method::GET, "/api/projects/paper/bib/search?q=x", None),
        ("import_account", |l| l.import_account = Rule::new(1, 600), Method::POST, "/api/projects/import", None),
        ("project_create_account", |l| l.project_create_account = Rule::new(1, 600), Method::POST, "/api/projects", Some(json!({}))),
    ];
    for (name, adjust, method, path, body) in cases {
        let (state, _dir) = state_with(*adjust).await;
        let (_, cookie) = user(&state, "ann@example.org");
        let first = call(&state, "198.51.100.1", &cookie, method.clone(), path, body.clone()).await;
        assert_ne!(first.status, StatusCode::TOO_MANY_REQUESTS, "{name}: the first request is allowed");
        // From another IP, so only the account rule can refuse.
        let second = call(&state, "198.51.100.2", &cookie, method.clone(), path, body.clone()).await;
        assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS, "{name}");
        assert_refused(&second, 600);
    }
}

#[tokio::test]
async fn guests_are_capped_per_share_link() {
    let (state, _dir) = state_with(|l| l.join_link = Rule::new(1, 600)).await;
    let (ann, _) = user(&state, "ann@example.org");
    let meta = state.registry.create("Paper").await.unwrap();
    state.store.set_member(&meta.id, &ann.id, galley_server::auth::Role::Admin).unwrap();
    let viewer = state.store.create_share(&meta.id, galley_server::auth::Role::Viewer, None, None, &ann.id).unwrap().1;
    let editor = state.store.create_share(&meta.id, galley_server::auth::Role::Editor, None, None, &ann.id).unwrap().1;
    let join = |token: String, ip: &'static str| {
        let state = state.clone();
        async move { call(&state, ip, &Who::Anonymous, Method::POST, "/api/auth/landing", Some(json!({ "token": token, "name": "Guest" }))).await }
    };
    assert_eq!(join(viewer.clone(), "198.51.100.1").await.status, StatusCode::OK);
    assert_refused(&join(viewer.clone(), "198.51.100.2").await, 600);
    assert_eq!(join(editor, "198.51.100.3").await.status, StatusCode::OK, "another link has its own budget");
    // A signed-in account joining by the link is not a new guest and is not capped by it.
    let (_, bob) = user(&state, "bob@example.org");
    let a = call(&state, "198.51.100.4", &bob, Method::POST, "/api/auth/landing", Some(json!({ "token": viewer, "name": "Bob" }))).await;
    assert_eq!(a.status, StatusCode::OK);
}

#[tokio::test]
async fn failed_signups_do_not_use_the_global_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.server.public_signup = true;
    config.limits = LimitsConfig { enabled: true, client_ip_header: "Fly-Client-IP".into(), signup_global: Rule::new(1, 3600), ..Default::default() };
    let state = AppState::new(dir.path(), &config).await.unwrap();
    user(&state, "taken@example.org");
    let signup = |email: &'static str, ip: &'static str| {
        let state = state.clone();
        async move {
            let body = json!({ "email": email, "name": "New", "password": "a long password" });
            call(&state, ip, &Who::Anonymous, Method::POST, "/api/auth/signup", Some(body)).await
        }
    };
    let dup = signup("taken@example.org", "198.51.100.1").await;
    assert!(dup.status.is_client_error() && dup.status != StatusCode::TOO_MANY_REQUESTS, "{}", dup.status);
    assert_eq!(signup("new@example.org", "198.51.100.2").await.status, StatusCode::OK);
    assert_refused(&signup("newer@example.org", "198.51.100.3").await, 3600);
    assert!(state.store.user_by_email("newer@example.org").unwrap().is_none());
}

#[tokio::test]
async fn a_real_socket_over_its_account_limit_is_refused_with_429() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let (state, _dir) = state_with(|l| {
        l.client_ip_header = String::new();
        l.socket_account = Rule::new(1, 600);
    })
    .await;
    let (ann, _) = user(&state, "ann@example.org");
    let meta = state.registry.create("Paper").await.unwrap();
    state.store.set_member(&meta.id, &ann.id, galley_server::auth::Role::Admin).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let connect = |session: String| {
        let mut request = format!("ws://{addr}/ws/{}/events", meta.id).into_client_request().unwrap();
        request.headers_mut().insert("cookie", format!("galley_session={session}").parse().unwrap());
        tokio_tungstenite::connect_async(request)
    };
    // Two sessions of one account share its budget.
    let first = state.store.create_session(&ann.id, 1).unwrap();
    let second = state.store.create_session(&ann.id, 1).unwrap();
    let (socket, _) = connect(first).await.expect("the first socket connects");
    match connect(second).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
            assert!(r.headers().contains_key("retry-after"));
        }
        Err(other) => panic!("expected a 429 handshake, got {other}"),
        Ok(_) => panic!("the second socket connected"),
    }
    drop(socket);
}

/// A project member with a device token, for MCP.
fn mcp_member(state: &AppState, email: &str, project: &str) -> Who {
    let (u, _) = user(state, email);
    state.store.set_member(project, &u.id, galley_server::auth::Role::Editor).unwrap();
    token(state, &u.id)
}

fn ping(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "ping" })
}

#[tokio::test]
async fn mcp_counts_each_message_and_caps_batches() {
    let (state, _dir) = state_with(|l| l.mcp_account = Rule::new(2, 600)).await;
    let meta = state.registry.create("Paper").await.unwrap();
    let ann = mcp_member(&state, "ann@example.org", &meta.id);
    let path = format!("/mcp/{}", meta.id);
    let a = call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(json!([ping(1), ping(2), ping(3)]))).await;
    assert_eq!(a.status, StatusCode::OK);
    let out = a.body.as_array().unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(out[0]["result"], json!({}));
    assert_eq!(out[1]["result"], json!({}));
    assert_eq!(out[2]["id"], 3);
    assert_eq!(out[2]["error"]["code"], -32000);
    assert_eq!(out[2]["error"]["message"], TOO_MANY);

    let batch: Vec<Value> = (0..21).map(ping).collect();
    let a = call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(Value::Array(batch))).await;
    assert_eq!(a.body["error"]["code"], -32600);
    assert_eq!(a.body["error"]["message"], "Batch too large");
    assert_eq!(a.body["id"], Value::Null);
}

#[tokio::test]
async fn mcp_runs_one_request_at_a_time_per_account() {
    let (state, _dir) = state_with(|l| l.mcp_wait_secs = 1).await;
    let meta = state.registry.create("Paper").await.unwrap();
    let ann = mcp_member(&state, "ann@example.org", &meta.id);
    let bob = mcp_member(&state, "bob@example.org", &meta.id);
    let ann_id = state.store.user_by_email("ann@example.org").unwrap().unwrap().id;
    let path = format!("/mcp/{}", meta.id);
    // Ann's turn is taken, as by a long compile.
    let held = state.limits.mcp_turn(&ann_id).await.unwrap();
    let a = call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(ping(7))).await;
    assert_eq!(a.body["id"], 7);
    assert_eq!(a.body["error"]["code"], -32000);
    assert_eq!(a.body["error"]["message"], TOO_MANY);
    let b = call(&state, "198.51.100.2", &bob, Method::POST, &path, Some(ping(8))).await;
    assert_eq!(b.body["result"], json!({}), "another account is not affected");
    drop(held);
    let a = call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(ping(9))).await;
    assert_eq!(a.body["result"], json!({}), "the next request goes through once the first finishes");
}

/// Take every password slot, so the next login or signup has to wait for one.
async fn hold_password_slots(state: &AppState) -> tokio::sync::OwnedSemaphorePermit {
    let n = u32::try_from(state.password_slots.available_permits()).unwrap();
    state.password_slots.clone().acquire_many_owned(n).await.unwrap()
}

#[tokio::test]
async fn a_login_that_gets_no_password_slot_is_not_a_failure() {
    let (state, _dir) = state_with(|l| {
        l.password_wait_secs = 0;
        l.login_failures = 2;
        l.login_email_failures = Rule::new(3, 3600);
    })
    .await;
    user(&state, "ann@example.org");
    let held = hold_password_slots(&state).await;
    // Far more timed-out attempts than either lock allows, for one email and one IP.
    for _ in 0..8 {
        let a = login(&state, "198.51.100.1", "ann@example.org", "wrong password").await;
        assert_refused(&a, 1);
    }
    drop(held);
    // None of them counted: the right password gets in at once, from the same IP.
    let a = login(&state, "198.51.100.1", "ann@example.org", "correct horse battery").await;
    assert_eq!(a.status, StatusCode::OK);
    // A real wrong password still counts.
    for _ in 0..2 {
        assert_eq!(login(&state, "198.51.100.1", "ann@example.org", "wrong password").await.status, StatusCode::UNAUTHORIZED);
    }
    assert_refused(&login(&state, "198.51.100.1", "ann@example.org", "correct horse battery").await, 900);
}

#[tokio::test]
async fn a_signup_that_gets_no_password_slot_is_refused_and_uses_no_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.server.public_signup = true;
    config.limits = LimitsConfig {
        enabled: true,
        client_ip_header: "Fly-Client-IP".into(),
        signup_global: Rule::new(1, 3600),
        password_wait_secs: 0,
        ..Default::default()
    };
    let state = AppState::new(dir.path(), &config).await.unwrap();
    user(&state, "admin@example.org");
    let signup = |email: &'static str, ip: &'static str| {
        let state = state.clone();
        async move {
            let body = json!({ "email": email, "name": "New", "password": "a long password" });
            call(&state, ip, &Who::Anonymous, Method::POST, "/api/auth/signup", Some(body)).await
        }
    };
    let held = hold_password_slots(&state).await;
    assert_refused(&signup("new@example.org", "198.51.100.1").await, 1);
    assert!(state.store.user_by_email("new@example.org").unwrap().is_none());
    drop(held);
    // The refused signup did not use the one signup the budget allows.
    assert_eq!(signup("new@example.org", "198.51.100.2").await.status, StatusCode::OK);
}

/// Install a stand-in Tectonic engine in `data_dir`, complete as Galley checks it. It answers
/// `--version` at once, and every compile sleeps for `secs` and then fails.
fn slow_tectonic(data_dir: &std::path::Path, secs: u32) {
    let pins = galley_build::install::pins().unwrap();
    let engine = data_dir.join("tectonic").join(pins.folder());
    std::fs::create_dir_all(engine.join("bundle")).unwrap();
    std::fs::create_dir_all(engine.join("cache/formats")).unwrap();
    std::fs::write(engine.join("bundle/SHA256SUM"), &pins.bundle_digest).unwrap();
    std::fs::write(engine.join("cache/formats/latex.fmt"), "fmt").unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo tectonic test-version; exit 0; fi\nsleep {secs}\nexit 1\n"
    );
    std::fs::write(engine.join("tectonic"), script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(engine.join("tectonic"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[tokio::test]
async fn a_compile_waiting_for_its_build_does_not_hold_the_accounts_turn() {
    let dir = tempfile::tempdir().unwrap();
    slow_tectonic(dir.path(), 4);
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits = LimitsConfig {
        enabled: true,
        client_ip_header: "Fly-Client-IP".into(),
        signup_ip: Rule::new(0, 3600),
        signup_global: Rule::new(0, 3600),
        mcp_wait_secs: 1,
        ..Default::default()
    };
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let meta = state.registry.create("Paper").await.unwrap();
    let ann = mcp_member(&state, "ann@example.org", &meta.id);
    let path = format!("/mcp/{}", meta.id);

    // A batch: a compile, then a ping that runs after the compile's build has finished.
    let compile = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "compile", "arguments": { "engine": "tectonic" } } });
    let batch = {
        let (state, ann, path) = (state.clone(), ann.clone(), path.clone());
        tokio::spawn(async move { call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(json!([compile, ping(2)]))).await })
    };
    // Wait for the build to start: from then on the compile only waits for its result.
    let builds = state.builds(&meta.id).await.unwrap();
    let started = std::time::Instant::now();
    while !builds.is_running().await {
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "the build never started");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Ann's other calls go ahead meanwhile, well within the 1 s wait for a turn.
    for id in [3, 4] {
        let a = call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(ping(id))).await;
        assert_eq!(a.body["id"], id);
        assert_eq!(a.body["result"], json!({}), "a ping during a compile is not refused: {}", a.body);
    }
    assert!(!batch.is_finished(), "the compile is still waiting for its slow build");

    // The batch finishes: the compile reports its (failed) build, and the ping after it got
    // the turn back.
    let done = tokio::time::timeout(std::time::Duration::from_secs(60), batch).await.unwrap().unwrap();
    let out = done.body.as_array().unwrap_or_else(|| panic!("{}", done.body));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["id"], 1);
    assert!(out[0]["result"]["content"][0]["text"].is_string(), "{}", out[0]);
    assert_eq!(out[1]["id"], 2);
    assert_eq!(out[1]["result"], json!({}));
    // Nothing is left holding a turn: the next request goes straight through.
    let a = call(&state, "198.51.100.1", &ann, Method::POST, &path, Some(ping(5))).await;
    assert_eq!(a.body["result"], json!({}));
}
