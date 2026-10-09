//! Single sign-on against an in-process provider that signs real ID tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use galley_server::{router, AppState, Config};
use openidconnect::core::{
    CoreGenderClaim, CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey,
};
use openidconnect::{
    Audience, EmptyAdditionalClaims, EndUserEmail, EndUserName, IssuerUrl, JsonWebKeyId, LocalizedClaim, Nonce,
    PrivateSigningKey, StandardClaims, SubjectIdentifier,
};
use rsa::pkcs1::EncodeRsaPrivateKey;
use serde_json::{json, Value};
use tower::ServiceExt;

const CLIENT: &str = "galley-test";

#[derive(Clone)]
struct Provider {
    issuer: String,
    key: Arc<CoreRsaPrivateSigningKey>,
    /// The nonce of the sign-in in flight, read from the authorize URL, and who signs in.
    next: Arc<Mutex<Option<(String, Who)>>>,
}

#[derive(Clone)]
struct Who {
    sub: &'static str,
    email: &'static str,
    verified: bool,
}

async fn discovery(State(p): State<Provider>) -> Json<Value> {
    Json(json!({
        "issuer": p.issuer,
        "authorization_endpoint": format!("{}/authorize", p.issuer),
        "token_endpoint": format!("{}/token", p.issuer),
        "jwks_uri": format!("{}/jwks", p.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

async fn jwks(State(p): State<Provider>) -> Json<CoreJsonWebKeySet> {
    Json(CoreJsonWebKeySet::new(vec![p.key.as_verification_key()]))
}

async fn token(State(p): State<Provider>, body: String) -> Result<Json<Value>, StatusCode> {
    // PKCE: the client must prove it started this sign-in.
    if !body.contains("code_verifier=") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (nonce, who) = p.next.lock().unwrap().clone().ok_or(StatusCode::BAD_REQUEST)?;
    let now = chrono::Utc::now();
    let mut name = LocalizedClaim::new();
    name.insert(None, EndUserName::new("Alicia Lorem".into()));
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(p.issuer.clone()).unwrap(),
        vec![Audience::new(CLIENT.into())],
        now + chrono::Duration::minutes(5),
        now,
        StandardClaims::<CoreGenderClaim>::new(SubjectIdentifier::new(who.sub.into()))
            .set_email(Some(EndUserEmail::new(who.email.into())))
            .set_email_verified(Some(who.verified))
            .set_name(Some(name)),
        EmptyAdditionalClaims {},
    )
    .set_nonce(Some(Nonce::new(nonce)));
    let id_token = CoreIdToken::new(claims, p.key.as_ref(), CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256, None, None).unwrap();
    Ok(Json(json!({ "access_token": "at", "token_type": "Bearer", "expires_in": 300, "id_token": id_token.to_string() })))
}

async fn provider() -> Provider {
    // 1024 bits keeps key generation fast in a debug build. The key lives for one test.
    let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 1024).unwrap();
    let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
    let key = CoreRsaPrivateSigningKey::from_pem(&pem, Some(JsonWebKeyId::new("k1".into()))).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = Provider {
        issuer: format!("http://{}", listener.local_addr().unwrap()),
        key: Arc::new(key),
        next: Arc::default(),
    };
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .with_state(p.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    p
}

async fn galley(p: &Provider, domains: &[&str], public_signup: bool) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.server.public_signup = public_signup;
    config.oidc.issuer = p.issuer.clone();
    config.oidc.client_id = CLIENT.into();
    config.oidc.redirect_url = "http://localhost/api/auth/oidc/callback".into();
    config.oidc.label = "Sign in with your university account".into();
    config.oidc.allowed_domains = domains.iter().map(|d| d.to_string()).collect();
    (AppState::new(dir.path(), &config).await.unwrap(), dir)
}

fn cookies(res: &axum::response::Response) -> HashMap<String, String> {
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok()?.split(';').next()?.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn location(res: &axum::response::Response) -> String {
    res.headers().get(header::LOCATION).unwrap().to_str().unwrap().to_string()
}

async fn fetch(state: &AppState, uri: &str, cookie: &str) -> axum::response::Response {
    let req = Request::get(uri).header(header::COOKIE, cookie).body(Body::empty()).unwrap();
    router(state.clone()).oneshot(req).await.unwrap()
}

/// Run the whole flow as a browser would. Returns where the callback sent the browser, and its cookies.
async fn sign_in(state: &AppState, p: &Provider, who: Who) -> (String, HashMap<String, String>) {
    let start = fetch(state, "/api/auth/oidc/start", "").await;
    assert!(start.status().is_redirection());
    let to = url::Url::parse(&location(&start)).unwrap();
    assert!(to.as_str().starts_with(&format!("{}/authorize", p.issuer)), "{to}");
    let q: HashMap<String, String> = to.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    *p.next.lock().unwrap() = Some((q["nonce"].clone(), who));
    let state_cookie = cookies(&start)["galley_oidc_state"].clone();
    let back = fetch(state, &format!("/api/auth/oidc/callback?code=c1&state={}", q["state"]), &format!("galley_oidc_state={state_cookie}")).await;
    (location(&back), cookies(&back))
}

async fn me(state: &AppState, session: &str) -> Value {
    let res = fetch(state, "/api/auth/me", &format!("galley_session={session}")).await;
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_verified_university_account_signs_in_and_comes_back_to_the_same_account() {
    let p = provider().await;
    let (state, _dir) = galley(&p, &["example.edu"], false).await;
    let anon = me(&state, "none").await;
    assert_eq!(anon["oidc"]["label"], "Sign in with your university account");

    let alicia = Who { sub: "u-1", email: "Alicia@Example.edu", verified: true };
    let (to, jar) = sign_in(&state, &p, alicia.clone()).await;
    assert_eq!(to, "/");
    let first = me(&state, &jar["galley_session"]).await;
    assert_eq!(first["user"]["email"], "alicia@example.edu");
    assert_eq!(first["user"]["name"], "Alicia Lorem");

    let (_, jar) = sign_in(&state, &p, alicia).await;
    let again = me(&state, &jar["galley_session"]).await;
    assert_eq!(again["user"]["id"], first["user"]["id"], "the subject finds the same account");
}

#[tokio::test]
async fn only_listed_domains_with_verified_email_get_in() {
    let p = provider().await;
    let (state, _dir) = galley(&p, &["example.edu"], false).await;
    let (to, jar) = sign_in(&state, &p, Who { sub: "u-2", email: "bob@elsewhere.org", verified: true }).await;
    assert!(to.starts_with("/?signin_error="), "{to}");
    assert!(to.contains("example.edu"), "the message names the accepted domain: {to}");
    assert!(!jar.contains_key("galley_session"));
    let (to, _) = sign_in(&state, &p, Who { sub: "u-3", email: "bob@example.edu", verified: false }).await;
    assert!(to.starts_with("/?signin_error="), "{to}");
}

#[tokio::test]
async fn an_existing_password_account_is_linked_only_by_a_verified_email() {
    let p = provider().await;
    let (state, _dir) = galley(&p, &[], true).await;
    let existing = state.store.create_user("bob@example.edu", "Bob", "password-123", true).unwrap();

    let (to, _) = sign_in(&state, &p, Who { sub: "u-4", email: "bob@example.edu", verified: false }).await;
    assert!(to.starts_with("/?signin_error="), "an unverified email must not take over an account: {to}");

    let (to, jar) = sign_in(&state, &p, Who { sub: "u-4", email: "bob@example.edu", verified: true }).await;
    assert_eq!(to, "/");
    assert_eq!(me(&state, &jar["galley_session"]).await["user"]["id"], existing.id.as_str());
}

#[tokio::test]
async fn invite_only_servers_without_a_domain_list_refuse_new_accounts() {
    let p = provider().await;
    let (state, _dir) = galley(&p, &[], false).await;
    state.store.create_user("admin@example.edu", "Admin", "password-123", true).unwrap();
    let (to, _) = sign_in(&state, &p, Who { sub: "u-5", email: "new@example.org", verified: true }).await;
    assert!(to.contains("invite-only"), "{to}");
}

#[tokio::test]
async fn a_callback_from_another_browser_or_a_replay_is_refused() {
    let p = provider().await;
    let (state, _dir) = galley(&p, &[], true).await;
    let start = fetch(&state, "/api/auth/oidc/start", "").await;
    let to = url::Url::parse(&location(&start)).unwrap();
    let q: HashMap<String, String> = to.query_pairs().into_owned().collect();
    *p.next.lock().unwrap() = Some((q["nonce"].clone(), Who { sub: "u-6", email: "eve@example.org", verified: true }));

    // Someone else's browser, without the cookie this sign-in set.
    let foreign = fetch(&state, &format!("/api/auth/oidc/callback?code=c&state={}", q["state"]), "").await;
    assert!(location(&foreign).starts_with("/?signin_error="));
    assert!(!cookies(&foreign).contains_key("galley_session"));

    let cookie = format!("galley_oidc_state={}", cookies(&start)["galley_oidc_state"]);
    let ok = fetch(&state, &format!("/api/auth/oidc/callback?code=c&state={}", q["state"]), &cookie).await;
    assert_eq!(location(&ok), "/");
    let replay = fetch(&state, &format!("/api/auth/oidc/callback?code=c&state={}", q["state"]), &cookie).await;
    assert!(location(&replay).starts_with("/?signin_error="), "a state works once");
}

#[tokio::test]
async fn a_cancelled_sign_in_says_so() {
    let p = provider().await;
    let (state, _dir) = galley(&p, &[], true).await;
    let res = fetch(&state, "/api/auth/oidc/callback?error=access_denied&state=x", "").await;
    assert!(location(&res).contains("cancelled"));
}
