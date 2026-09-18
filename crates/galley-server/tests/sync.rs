//! End to end without a browser: sign up, create a project, connect two y-websocket clients,
//! edit, watch the peer and the commit event, and check that roles gate access.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use galley_server::{router, AppState, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;
use yrs::encoding::read::Cursor;
use yrs::sync::{Message as YMessage, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, Text, Transact, Update};

struct Server {
    state: AppState,
    addr: String,
    _dir: tempfile::TempDir,
}

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.sync.flush_quiet_ms = 150;
    config.build.sandbox = "none".into();
    config.server.public_signup = true; // tests create several accounts directly
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        state,
        addr: format!("127.0.0.1:{}", addr.port()),
        _dir: dir,
    }
}

/// A signed-in identity: the session cookie value and the CSRF token to echo on writes.
#[derive(Clone, Default)]
struct Auth {
    session: Option<String>,
    csrf: Option<String>,
}

impl Auth {
    fn cookie_header(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(s) = &self.session {
            parts.push(format!("galley_session={s}"));
        }
        if let Some(c) = &self.csrf {
            parts.push(format!("galley_csrf={c}"));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

fn cookie_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    for v in headers.get_all("set-cookie") {
        let s = v.to_str().ok()?;
        if let Some(rest) = s.strip_prefix(&format!("{name}=")) {
            return Some(rest.split(';').next().unwrap_or("").to_string());
        }
    }
    None
}

async fn rest(state: &AppState, auth: &Auth, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value, Auth) {
    let mut builder = Request::builder().method(method).uri(path).header("content-type", "application/json");
    if let Some(cookie) = auth.cookie_header() {
        builder = builder.header("cookie", cookie);
    }
    if matches!(method, "POST" | "PUT" | "DELETE" | "PATCH") {
        if let Some(csrf) = &auth.csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
    }
    let req = builder
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let res = router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let next = Auth {
        session: cookie_value(&headers, "galley_session").or_else(|| auth.session.clone()),
        csrf: cookie_value(&headers, "galley_csrf").or_else(|| auth.csrf.clone()),
    };
    (status, json, next)
}

/// Create an account and return its authenticated cookies.
async fn signup(state: &AppState, email: &str, name: &str) -> Auth {
    let (status, _, auth) = rest(
        state,
        &Auth::default(),
        "POST",
        "/api/auth/signup",
        Some(json!({ "email": email, "name": name, "password": "supersecret" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup should succeed");
    assert!(auth.session.is_some() && auth.csrf.is_some());
    auth
}

/// A y-protocol client over a real WebSocket, authenticated by cookie.
struct Client {
    doc: Doc,
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

impl Client {
    async fn connect(addr: &str, auth: &Auth, project: &str, path: &str) -> Result<Client, ()> {
        let url = format!("ws://{addr}/ws/{project}/doc/{path}");
        let mut req = url.into_client_request().unwrap();
        req.headers_mut()
            .insert("Cookie", auth.cookie_header().unwrap().parse().unwrap());
        let (ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|_| ())?;
        let mut c = Client { doc: Doc::new(), ws };
        let greeting = c.recv_binary().await;
        c.apply_frame(&greeting);
        let sv = c.doc.transact().state_vector();
        c.send(YMessage::Sync(SyncMessage::SyncStep1(sv)).encode_v1()).await;
        c.recv_until(|c| !c.text().is_empty()).await;
        Ok(c)
    }

    async fn send(&mut self, frame: Vec<u8>) {
        self.ws.send(Message::Binary(frame.into())).await.unwrap();
    }

    async fn recv_binary(&mut self) -> Vec<u8> {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                .await
                .expect("timed out waiting for a frame")
                .expect("socket closed")
                .unwrap();
            if let Message::Binary(b) = msg {
                return b.to_vec();
            }
        }
    }

    fn apply_frame(&self, frame: &[u8]) {
        let mut dec = DecoderV1::new(Cursor::new(frame));
        for msg in MessageReader::new(&mut dec) {
            match msg.unwrap() {
                YMessage::Sync(SyncMessage::SyncStep2(u)) | YMessage::Sync(SyncMessage::Update(u)) => {
                    let mut txn = self.doc.transact_mut();
                    txn.apply_update(Update::decode_v1(&u).unwrap()).unwrap();
                }
                _ => {}
            }
        }
    }

    async fn recv_until(&mut self, done: impl Fn(&Client) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(tokio::time::Instant::now() < deadline, "condition not met in time");
            let frame = self.recv_binary().await;
            self.apply_frame(&frame);
        }
    }

    async fn insert(&mut self, at: u32, s: &str) {
        let before = self.doc.transact().state_vector();
        let text = self.doc.get_or_insert_text("content");
        text.insert(&mut self.doc.transact_mut(), at, s);
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        self.send(YMessage::Sync(SyncMessage::Update(update)).encode_v1()).await;
    }

    fn text(&self) -> String {
        let text = self.doc.get_or_insert_text("content");
        let txn = self.doc.transact();
        text.get_string(&txn)
    }
}

#[tokio::test]
async fn two_editors_sync_commit_and_a_viewer_is_read_only() {
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana Novak").await;

    let (status, meta, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Thesis" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{meta}");
    assert_eq!(meta["id"], "thesis");
    assert_eq!(meta["role"], "admin");

    // A second account with no membership cannot see the project.
    let mallory = signup(&server.state, "mallory@evil.com", "Mallory").await;
    let (status, _, _) = rest(&server.state, &mallory, "GET", "/api/projects/thesis", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "non-members get 404, not 403");

    // Invite mallory as a viewer.
    let (status, _, ana) =
        rest(&server.state, &ana, "POST", "/api/projects/thesis/members", Some(json!({ "email": "mallory@evil.com", "role": "viewer" }))).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, files, _) = rest(&server.state, &ana, "GET", "/api/projects/thesis/files", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = files.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["main.tex", "refs.bib"]);

    // Events socket (authenticated) to observe the commit.
    let (mut events, _) = {
        let url = format!("ws://{}/ws/thesis/events", server.addr);
        let mut req = url.into_client_request().unwrap();
        req.headers_mut().insert("Cookie", ana.cookie_header().unwrap().parse().unwrap());
        tokio_tungstenite::connect_async(req).await.unwrap()
    };

    let mut owner = Client::connect(&server.addr, &ana, "thesis", "main.tex").await.unwrap();
    assert!(owner.text().starts_with("\\documentclass"));

    // The viewer connects read-only: they receive the text but their edits are dropped, not
    // applied, and the connection stays open so they keep viewing.
    let mut viewer = Client::connect(&server.addr, &mallory, "thesis", "main.tex").await.unwrap();
    assert!(viewer.text().starts_with("\\documentclass"));
    viewer.insert(0, "SNEAKY").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The owner edits; it commits.
    owner.insert(0, "% Hello from Ana\n").await;
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(t))) = events.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "commit" {
                    return v;
                }
            }
        }
    })
    .await
    .expect("commit event");
    assert_eq!(event["message"], "edit: main.tex");
    assert_eq!(event["author"], "Ana Novak");

    let (status, history, _) = rest(&server.state, &ana, "GET", "/api/projects/thesis/history", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history.as_array().unwrap().len(), 2, "template commit + edit");

    // The owner's edit landed; the viewer's never did.
    let on_disk = std::fs::read_to_string(server.state.registry.root().join("thesis/main.tex")).unwrap();
    assert!(on_disk.contains("% Hello from Ana"));
    assert!(!on_disk.contains("SNEAKY"), "a read-only client's edit is dropped server-side");
}

#[tokio::test]
async fn share_link_landing_and_comments() {
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    let (_, _, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Paper" }))).await;

    // Owner mints a commenter share link.
    let (status, link, ana) =
        rest(&server.state, &ana, "POST", "/api/projects/paper/shares", Some(json!({ "role": "commenter", "label": "Reviewers" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let path = link["path"].as_str().unwrap();
    let token = path.strip_prefix("/s/").unwrap().to_string();

    // Anyone can preview what the link grants.
    let (status, preview, _) = rest(&server.state, &Auth::default(), "GET", &format!("/api/share/{token}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["project_name"], "Paper");
    assert_eq!(preview["role"], "commenter");

    // A reviewer lands with just a display name and joins as a guest commenter.
    let (status, landed, guest) =
        rest(&server.state, &Auth::default(), "POST", "/api/auth/landing", Some(json!({ "token": token, "name": "Reviewer 2" }))).await;
    assert_eq!(status, StatusCode::OK, "{landed}");
    assert_eq!(landed["role"], "commenter");
    assert_eq!(landed["user"]["is_guest"], true);

    // The guest can comment but not edit files.
    let (status, comment, guest) = rest(
        &server.state,
        &guest,
        "POST",
        "/api/projects/paper/comments",
        Some(json!({ "file": "main.tex", "anchor": "abc", "quote": "the claim", "body": "needs a citation" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{comment}");
    let (status, _, _) =
        rest(&server.state, &guest, "POST", "/api/projects/paper/files", Some(json!({ "path": "extra.tex" }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "commenter can't add files");

    // The owner sees the comment.
    let (status, comments, _) = rest(&server.state, &ana, "GET", "/api/projects/paper/comments", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(comments.as_array().unwrap().len(), 1);
    assert_eq!(comments[0]["author_name"], "Reviewer 2");

    // Revoking the link locks new visitors out.
    let (status, _, ana) = rest(&server.state, &ana, "DELETE", &format!("/api/projects/paper/shares/{}", link["id"].as_str().unwrap()), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = rest(&server.state, &Auth::default(), "GET", &format!("/api/share/{token}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = ana;
}

#[tokio::test]
async fn governance_solo_immediate_then_unanimous_vote() {
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    let bob = signup(&server.state, "bob@uni.edu", "Bob").await;
    let carol = signup(&server.state, "carol@uni.edu", "Carol").await;
    let (_, _, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Gov" }))).await;

    // Solo (default): inviting Bob as admin applies immediately.
    let (status, out, ana) =
        rest(&server.state, &ana, "POST", "/api/projects/gov/members", Some(json!({ "email": "bob@uni.edu", "role": "admin" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["status"], "applied");

    // Switch governance to unanimous (governed by the current solo rule → applies at once).
    let (status, out, ana) = rest(&server.state, &ana, "PUT", "/api/projects/gov/governance", Some(json!({ "mode": "unanimous" }))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["status"], "applied");
    let (_, gov, _) = rest(&server.state, &ana, "GET", "/api/projects/gov/governance", None).await;
    assert_eq!(gov["mode"], "unanimous");

    // Now inviting Carol needs both admins. Ana's proposal is pending until Bob approves.
    let (status, out, ana) =
        rest(&server.state, &ana, "POST", "/api/projects/gov/members", Some(json!({ "email": "carol@uni.edu", "role": "viewer" }))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(out["status"], "pending", "two admins under unanimous → needs a vote");
    let request_id = out["request_id"].as_str().unwrap().to_string();

    // Carol is not a member yet, and Bob sees the pending request.
    let (status, _, _) = rest(&server.state, &carol, "GET", "/api/projects/gov", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, gov, bob) = rest(&server.state, &bob, "GET", "/api/projects/gov/governance", None).await;
    assert_eq!(gov["requests"].as_array().unwrap().len(), 1);
    assert_eq!(gov["requests"][0]["summary"], "Add Carol as viewer");

    // Bob approves → the invite applies.
    let (status, out, _) =
        rest(&server.state, &bob, "POST", &format!("/api/projects/gov/requests/{request_id}/vote"), Some(json!({ "approve": true }))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["status"], "applied");
    let (status, _, _) = rest(&server.state, &carol, "GET", "/api/projects/gov", None).await;
    assert_eq!(status, StatusCode::OK, "Carol is now a member");
    let _ = ana;
}

#[tokio::test]
async fn csrf_and_auth_are_enforced() {
    let server = start().await;
    // No session: writes and reads are refused.
    let (status, _, _) = rest(&server.state, &Auth::default(), "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    // A valid session but a missing CSRF header is blocked on writes.
    let no_csrf = Auth { session: ana.session.clone(), csrf: None };
    let (status, _, _) = rest(&server.state, &no_csrf, "POST", "/api/projects", Some(json!({ "name": "X" }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "missing CSRF token is rejected");

    // With the token it works.
    let (status, _, _) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "X" }))).await;
    assert_eq!(status, StatusCode::CREATED);

    // A device token needs no CSRF token, and a bad one never falls back to the session cookie.
    let (_, made, _) = rest(&server.state, &ana, "POST", "/api/auth/tokens", Some(json!({ "label": "cli" }))).await;
    let token = made["token"].as_str().unwrap().to_string();
    let with_bearer = |t: &str, cookie: Option<String>| {
        let mut b = Request::builder()
            .method("PUT")
            .uri("/api/projects/x/files/content?path=notes.txt")
            .header("authorization", format!("Bearer {t}"));
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        b.body(Body::from("hello")).unwrap()
    };
    let res = router(server.state.clone()).oneshot(with_bearer(&token, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "bearer upload without CSRF");
    let res = router(server.state.clone()).oneshot(with_bearer("not-a-token", ana.cookie_header())).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "a bad token does not ride on the browser session");
}

/// Creating from a template writes every file, including the ones in subdirectories, and commits
/// them as the first revision.
#[tokio::test]
async fn a_project_starts_from_the_template_it_asked_for() {
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana Novak").await;

    let (status, list, ana) = rest(&server.state, &ana, "GET", "/api/templates", None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = list.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"thesis") && ids.contains(&"blank-article"), "{ids:?}");

    let body = json!({ "name": "Dissertation", "template": "thesis" });
    let (status, meta, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{meta}");

    let (status, files, ana) = rest(&server.state, &ana, "GET", "/api/projects/dissertation/files", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = files.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(names.contains(&"chapters/introduction.tex"), "chapters are missing: {names:?}");
    assert_eq!(names.len(), 5, "{names:?}");

    // The venue a template implies is applied; the blank article implies none.
    let (_, meta, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Draft", "template": "preprint" }))).await;
    assert_eq!(meta["venue"], "arxiv");

    let (status, err, _) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Nope", "template": "not-real" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
}
