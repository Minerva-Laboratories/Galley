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
    start_with(|_| {}).await
}

/// A server on a real port, with the test defaults and then `adjust` applied to its config.
async fn start_with(adjust: impl FnOnce(&mut Config)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.sync.flush_quiet_ms = 150;
    config.build.sandbox = "none".into();
    config.server.public_signup = true; // tests create several accounts directly
    config.limits.enabled = false; // tests/limits.rs covers the limits
    adjust(&mut config);
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

#[tokio::test]
async fn project_downloads_use_the_current_display_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let auth = signup(&state, "export@example.com", "Author").await;
    let (status, meta, auth) = rest(&state, &auth, "POST", "/api/projects", Some(json!({"name":"Original project"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = meta["id"].as_str().unwrap();
    let builds = state.builds(id).await.unwrap();
    std::fs::create_dir_all(builds.out_dir()).unwrap();
    let pdf = b"%PDF-1.4\nunchanged PDF bytes\n";
    std::fs::write(builds.out_dir().join("last-good.pdf"), pdf).unwrap();
    let mut previous_etag = None;
    for (name, expected) in [
        ("Informe final", "Informe%20final.zip"),
        ("Artículo / revisión\\final\n\"2026\"", "Art%C3%ADculo%20_%20revisi%C3%B3n_final_%222026%22.zip"),
    ] {
        let (status, renamed, _) = rest(&state, &auth, "PATCH", &format!("/api/projects/{id}/settings"), Some(json!({"name":name}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renamed["id"], id);
        let response = router(state.clone()).oneshot(
            Request::builder().uri(format!("/api/projects/{id}/export"))
                .header("cookie", auth.cookie_header().unwrap())
                .body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/zip");
        let disposition = response.headers()["content-disposition"].to_str().unwrap();
        assert!(disposition.ends_with(&format!("filename*=UTF-8''{expected}")), "{disposition}");
        assert!(!disposition.contains("original-project"));
        assert!(!disposition.contains(['\r', '\n']));
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(archive.by_name("main.tex").is_ok());
        assert!(archive.by_name("refs.bib").is_ok());
        let mut request = Request::builder().uri(format!("/api/projects/{id}/build/pdf"))
            .header("cookie", auth.cookie_header().unwrap());
        if let Some(etag) = &previous_etag {
            request = request.header("if-none-match", etag);
        }
        let response = router(state.clone()).oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "rename must refresh cached download metadata");
        assert_eq!(response.headers()["content-type"], "application/pdf");
        let disposition = response.headers()["content-disposition"].to_str().unwrap();
        let expected_pdf = expected.strip_suffix(".zip").unwrap();
        assert!(disposition.ends_with(&format!("filename*=UTF-8''{expected_pdf}.pdf")), "{disposition}");
        assert!(!disposition.contains("original-project"));
        previous_etag = Some(response.headers()["etag"].clone());
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes().as_ref(), pdf);
    }
}

#[tokio::test]
async fn an_exported_project_with_a_figure_imports_back_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let auth = signup(&state, "roundtrip@example.com", "Author").await;
    let (_, meta, auth) = rest(&state, &auth, "POST", "/api/projects", Some(json!({"name":"Round trip"}))).await;
    let id = meta["id"].as_str().unwrap();
    let figure: Vec<u8> = (0..300_000u32).map(|i| (i * 7919 % 251) as u8).collect();
    std::fs::create_dir_all(state.registry.root().join(id).join("figures")).unwrap();
    std::fs::write(state.registry.root().join(id).join("figures/plot.png"), &figure).unwrap();

    let get = |path: String| Request::builder().uri(path).header("cookie", auth.cookie_header().unwrap()).body(Body::empty()).unwrap();
    let response = router(state.clone()).oneshot(get(format!("/api/projects/{id}/export"))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let length: usize = response.headers()["content-length"].to_str().unwrap().parse().unwrap();
    let zip = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(zip.len(), length);

    let request = Request::builder()
        .method("POST")
        .uri("/api/projects/import?name=Imported%20copy")
        .header("cookie", auth.cookie_header().unwrap())
        .header("x-csrf-token", auth.csrf.clone().unwrap())
        .header("content-type", "application/zip")
        .body(Body::from(zip))
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let copy = body["project"]["id"].as_str().unwrap();
    assert_eq!(body["report"]["main_file"], "main.tex");
    assert_eq!(std::fs::read(state.registry.root().join(copy).join("figures/plot.png")).unwrap(), figure);
    let staging = dir.path().join("cache/staging");
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0, "staging is cleaned up");
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

#[tokio::test]
async fn folder_routes_check_roles_and_notify_listeners() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.server.public_signup = true;
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let admin = signup(&state, "folder-admin@example.com", "Admin").await;
    let viewer = signup(&state, "folder-viewer@example.com", "Viewer").await;
    let editor = signup(&state, "folder-editor@example.com", "Editor").await;
    let outsider = signup(&state, "folder-outsider@example.com", "Outsider").await;
    let (status, meta, admin) = rest(&state, &admin, "POST", "/api/projects", Some(json!({"name":"Folder paper"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = meta["id"].as_str().unwrap();
    let folders_url = format!("/api/projects/{id}/folders");
    for (email, role) in [("folder-viewer@example.com", "viewer"), ("folder-editor@example.com", "editor")] {
        let (status, _, _) = rest(&state, &admin, "POST", &format!("/api/projects/{id}/members"), Some(json!({"email": email, "role": role}))).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, folders, _) = rest(&state, &viewer, "GET", &folders_url, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(folders, json!([]));
    let (status, _, _) = rest(&state, &viewer, "POST", &folders_url, Some(json!({"path":"viewer-folder"}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = rest(&state, &outsider, "GET", &folders_url, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let project = state.registry.open(id).await.unwrap();
    let mut events = project.subscribe_events();
    let (status, created, _) = rest(&state, &editor, "POST", &folders_url, Some(json!({"path":"sections/./drafts"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created, json!({"path":"sections/drafts"}));
    assert!(matches!(events.try_recv(), Ok(galley_sync::ProjectEvent::FilesChanged)));
    let (status, folders, _) = rest(&state, &viewer, "GET", &folders_url, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(folders, json!(["sections", "sections/drafts"]));
    let (status, created, _) = rest(&state, &admin, "POST", &folders_url, Some(json!({"path":"figures"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created, json!({"path":"figures"}));
    let (status, _, _) = rest(&state, &editor, "POST", &folders_url, Some(json!({"path":"../escape"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn engine_selection_is_saved_and_role_gated() {
    let dir = tempfile::tempdir().unwrap();
    let tex_bin = dir.path().join("texlive-bin");
    std::fs::create_dir(&tex_bin).unwrap();
    for name in ["latexmk", "kpsewhich", "xelatex", "xdvipdfmx"] {
        let path = tex_bin.join(name);
        std::fs::write(&path, "#!/bin/sh\necho test-version\n").unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    // A stand-in for an installed Tectonic engine: complete as Galley checks it, with a script as
    // the binary. Its builds fail, which this test does not look at.
    let pins = galley_build::install::pins().unwrap();
    let engine = dir.path().join("tectonic").join(pins.folder());
    std::fs::create_dir_all(engine.join("bundle")).unwrap();
    std::fs::create_dir_all(engine.join("cache/formats")).unwrap();
    std::fs::write(engine.join("bundle/SHA256SUM"), &pins.bundle_digest).unwrap();
    std::fs::write(engine.join("cache/formats/latex.fmt"), "fmt").unwrap();
    std::fs::write(engine.join("tectonic"), "#!/bin/sh\necho tectonic test-version\n").unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(engine.join("tectonic"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.build.texlive_path = tex_bin.to_string_lossy().into_owned();
    config.server.public_signup = true;
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let admin = signup(&state, "engine-admin@example.com", "Admin").await;
    let viewer = signup(&state, "engine-viewer@example.com", "Viewer").await;
    let commenter = signup(&state, "engine-commenter@example.com", "Commenter").await;
    let (status, project, admin) = rest(
        &state,
        &admin,
        "POST",
        "/api/projects",
        Some(json!({"name":"Engine paper"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(project["engine"], "tectonic");
    let (status, inventory, _) = rest(
        &state,
        &admin,
        "GET",
        "/api/projects/engine-paper/engines",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inventory["selected"], "tectonic");
    assert!(
        inventory["engines"]
            .as_array()
            .is_some_and(|engines| engines.len() == 5)
    );
    let (_, _, _) = rest(
        &state,
        &admin,
        "POST",
        "/api/projects/engine-paper/members",
        Some(json!({"email":"engine-viewer@example.com","role":"viewer"})),
    )
    .await;
    let (status, _, _) = rest(
        &state,
        &viewer,
        "PATCH",
        "/api/projects/engine-paper/settings",
        Some(json!({"engine":"tectonic"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = rest(
        &state,
        &viewer,
        "POST",
        "/api/projects/engine-paper/build",
        Some(json!({"engine":"tectonic"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = rest(
        &state,
        &viewer,
        "GET",
        "/api/projects/engine-paper/engines",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = rest(
        &state,
        &admin,
        "PATCH",
        "/api/projects/engine-paper/settings",
        Some(json!({"engine":"unknown"})),
    )
    .await;
    assert!(status.is_client_error());
    assert_eq!(
        state.registry.meta("engine-paper").unwrap().engine,
        galley_build::EngineKind::Tectonic
    );
    let mut events = state.collab_channel("engine-paper").await.subscribe();
    let (status, saved, _) = rest(
        &state,
        &admin,
        "PATCH",
        "/api/projects/engine-paper/settings",
        Some(json!({ "engine": "xelatex" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["engine"], "xelatex");
    let event: Value = serde_json::from_str(&events.try_recv().expect("engine event")).unwrap();
    assert_eq!(
        event,
        json!({ "type": "engine_changed", "engine": "xelatex" })
    );
    let (_, _, _) = rest(
        &state,
        &admin,
        "POST",
        "/api/projects/engine-paper/members",
        Some(json!({"email":"engine-commenter@example.com","role":"commenter"})),
    )
    .await;
    let (status, queued, _) = rest(
        &state,
        &commenter,
        "POST",
        "/api/projects/engine-paper/build",
        Some(json!({"draft":true,"engine":"tectonic"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(queued["queued"], true);
    let first_id = queued["id"].as_u64().unwrap();
    let (status, queued_again, _) = rest(
        &state, &commenter, "POST", "/api/projects/engine-paper/build", Some(json!({"draft":true})),
    ).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(queued_again["id"].as_u64().unwrap() > first_id);
    let (status, unavailable, _) = rest(
        &state, &admin, "PATCH", "/api/projects/engine-paper/settings", Some(json!({"engine":"pdflatex"})),
    ).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(unavailable["error"].as_str().is_some_and(|message| message.contains("pdflatex")));
    assert_eq!(
        state.registry.meta("engine-paper").unwrap().engine,
        galley_build::EngineKind::XeLatex
    );
    let restarted = AppState::new(dir.path(), &config).await.unwrap();
    let (status, restored, _) = rest(&restarted, &admin, "GET", "/api/projects/engine-paper", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(restored["engine"], "xelatex");
}

/// Nothing is downloaded during a build: without an installed engine, a Tectonic build is refused
/// with the way to install it.
#[tokio::test]
async fn a_tectonic_build_without_an_engine_is_refused_with_the_install_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    if galley_build::engine::Tectonic::locate(dir.path()).is_some() {
        return; // This machine's image holds an engine.
    }
    let admin = signup(&state, "no-engine@example.com", "No engine").await;
    let (status, _, admin) = rest(&state, &admin, "POST", "/api/projects", Some(json!({"name":"No engine"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, refused, _) = rest(
        &state, &admin, "POST", "/api/projects/no-engine/build", Some(json!({"engine":"tectonic"})),
    ).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused["error"].as_str().is_some_and(|m| m.contains("galley engine install")), "{refused}");
}

#[tokio::test]
async fn build_status_restores_pdf_producer_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let admin = signup(&state, "producer@example.com", "Producer").await;
    let (status, _, admin) = rest(&state, &admin, "POST", "/api/projects", Some(json!({"name":"Producer test"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    // A build folder of the current layout; one from before it would be deleted on start.
    galley_build::zones::prepare(&dir.path().join("data/producer-test")).unwrap();
    let build_dir = dir.path().join("data/producer-test/.galley/build");
    let record = json!({
        "id": 7, "status": "failed", "errors": [], "error_count": 1, "warning_count": 0,
        "profile": {"total_ms": 1, "compile_ms": 1, "fetch_ms": 0, "figure_ms": 0},
        "pages": null, "fetched_packages": false, "pdf_fresh": false, "pdf_available": true,
        "stale": false, "draft": false, "main_file": "main.tex", "engine": "xelatex",
        "engine_version": "XeTeX test", "pdf_engine": "pdflatex", "pdf_engine_version": "pdfTeX test",
        "sandbox": "none", "finished_at": chrono::Utc::now(), "message": "failed with previous PDF still available"
    });
    std::fs::write(build_dir.join(galley_build::runner::LAST_RESULT_JSON), serde_json::to_vec(&record).unwrap()).unwrap();
    let restarted = AppState::new(dir.path(), &config).await.unwrap();
    let (status, build, _) = rest(&restarted, &admin, "GET", "/api/projects/producer-test/build", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(build["engine"], "tectonic");
    assert_eq!(build["last"]["engine"], "xelatex");
    assert_eq!(build["pdf_engine"], "pdflatex");
    assert_eq!(build["pdf_engine_version"], "pdfTeX test");
}

/// Opt in after provisioning all compilers and a warm Tectonic cache. This exercises the same
/// HTTP MCP route that a client uses, with compiler probing inside bubblewrap.
#[tokio::test]
async fn real_mcp_engine_matrix_preserves_project_selection() {
    if std::env::var("GALLEY_TEST_ENGINE_MATRIX").as_deref() != Ok("1") { return; }
    let engines = std::env::var("GALLEY_TEST_TECTONIC_ENGINES").expect("GALLEY_TEST_TECTONIC_ENGINES is required");
    let texlive = std::env::var("GALLEY_TEST_TEXLIVE_PATH").expect("GALLEY_TEST_TEXLIVE_PATH is required");
    assert!(std::path::Path::new(&engines).is_dir());
    assert!(std::path::Path::new(&texlive).join("latexmk").is_file());
    let dir = tempfile::tempdir().unwrap();
    let folder = galley_build::install::pins().unwrap().folder();
    std::fs::create_dir_all(dir.path().join("tectonic")).unwrap();
    std::os::unix::fs::symlink(std::path::Path::new(&engines).join(&folder), dir.path().join("tectonic").join(&folder)).unwrap();
    let mut config = Config::default();
    config.build.sandbox = "bwrap".into();
    config.build.texlive_path = texlive;
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    assert_eq!(state.builder.sandbox.kind, galley_build::SandboxKind::Bwrap);
    let admin = signup(&state, "matrix@example.com", "Matrix").await;
    let (status, _, admin) = rest(&state, &admin, "POST", "/api/projects", Some(json!({"name":"Matrix paper"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, token_body, _) = rest(&state, &admin, "POST", "/api/auth/tokens", Some(json!({"label":"matrix test"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let token = token_body["token"].as_str().unwrap();

    let inventory = mcp_tool_text(&state, token, "matrix-paper", "list_engines", json!({})).await;
    let inventory: Value = serde_json::from_str(&inventory).unwrap();
    assert_eq!(inventory["selected"], "tectonic");
    let engines = inventory["engines"].as_array().unwrap();
    assert_eq!(engines.len(), 5);
    for engine in engines { assert_eq!(engine["available"], true, "{engine}"); }

    for engine in ["tectonic", "pdflatex", "xelatex"] {
        let args = if engine == "tectonic" { json!({}) } else { json!({"engine":engine}) };
        let response = mcp_tool_text(&state, token, "matrix-paper", "compile", args).await;
        assert!(response.contains("status: Ok"), "{engine}: {response}");
        assert!(response.contains(&format!("engine: {engine}")), "{response}");
        let id = response.lines().find_map(|line| line.strip_prefix("id: ").and_then(|raw| raw.parse::<u64>().ok())).unwrap();
        let (status, build, _) = rest(&state, &admin, "GET", "/api/projects/matrix-paper/build", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(build["last"]["id"], id);
        assert_eq!(build["last"]["engine"], engine);
        assert_eq!(build["pdf_engine"], engine);
        assert_eq!(build["engine"], "tectonic");
    }
    assert_eq!(state.registry.meta("matrix-paper").unwrap().engine, galley_build::EngineKind::Tectonic);
    let restarted = AppState::new(dir.path(), &config).await.unwrap();
    let (status, build, _) = rest(&restarted, &admin, "GET", "/api/projects/matrix-paper/build", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(build["engine"], "tectonic");
    assert_eq!(build["pdf_engine"], "xelatex");
    assert_eq!(build["last"]["engine"], "xelatex");
}

async fn mcp_tool_text(state: &AppState, token: &str, project: &str, name: &str, args: Value) -> String {
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
    let request = Request::builder().method("POST").uri(format!("/mcp/{project}"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string())).unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let reply: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    reply["result"]["content"][0]["text"].as_str().unwrap().to_string()
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

    // An open tab keeps its project loaded. Once every tab has gone, the project leaves memory,
    // and the next tab gets the same text from disk.
    assert_eq!(server.state.unload_idle(Duration::ZERO).await, 0, "a connected project is never unloaded");
    drop(owner);
    drop(viewer);
    drop(events);
    let unloaded = tokio::time::timeout(Duration::from_secs(5), async {
        while server.state.unload_idle(Duration::ZERO).await == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(unloaded.is_ok(), "the project unloads once its tabs close");
    let again = Client::connect(&server.addr, &ana, "thesis", "main.tex").await.unwrap();
    assert!(again.text().starts_with("% Hello from Ana\n"));
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

    // Revoking the link locks new visitors out and removes the guest who joined through it.
    let share = format!("/api/projects/paper/shares/{}", link["id"].as_str().unwrap());
    let (status, out, ana) = rest(&server.state, &ana, "DELETE", &share, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out, json!({ "ok": true, "removed_guests": 1 }));
    let (status, _, _) = rest(&server.state, &Auth::default(), "GET", &format!("/api/share/{token}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = rest(&server.state, &guest, "GET", "/api/projects/paper", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the guest's session is gone");
    let (status, _, _) =
        rest(&server.state, &Auth::default(), "POST", "/api/auth/landing", Some(json!({ "token": token, "name": "Late" }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, out, ana) = rest(&server.state, &ana, "DELETE", &share, None).await;
    assert_eq!(status, StatusCode::OK, "a repeat revoke succeeds and changes nothing");
    assert_eq!(out, json!({ "ok": true, "removed_guests": 0 }));
    let (status, _, _) = rest(&server.state, &ana, "DELETE", "/api/projects/paper/shares/no-such-link", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a link the project never had is not found");
}

#[tokio::test]
async fn revoking_a_link_closes_its_guests_sockets() {
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    let (status, meta, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Revoke" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{meta}");
    let id = meta["id"].as_str().unwrap().to_string();
    let (status, link, ana) =
        rest(&server.state, &ana, "POST", &format!("/api/projects/{id}/shares"), Some(json!({ "role": "editor" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let token = link["path"].as_str().unwrap().strip_prefix("/s/").unwrap().to_string();
    let (status, landed, guest) =
        rest(&server.state, &Auth::default(), "POST", "/api/auth/landing", Some(json!({ "token": token, "name": "Guest" }))).await;
    assert_eq!(status, StatusCode::OK, "{landed}");
    let mut events = event_socket(&server, &guest, &id).await;
    assert_eq!(first_event(&mut events).await, json!({ "type": "role_changed", "role": "editor" }));

    let (status, out, _) =
        rest(&server.state, &ana, "DELETE", &format!("/api/projects/{id}/shares/{}", link["id"].as_str().unwrap()), None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    next_event(&mut events, "access_removed").await;
    assert_eq!(next_close(&mut events).await, Some(4403));
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

    // Each admin proposes lowering themself, and both get approved. The second would leave no
    // admin, so its vote resolves as cancelled and nothing changes.
    let ana_id = user_id(&server.state, &ana).await;
    let bob_id = user_id(&server.state, &bob).await;
    let (status, lower_ana, ana) =
        rest(&server.state, &ana, "PUT", &format!("/api/projects/gov/members/{ana_id}"), Some(json!({ "role": "editor" }))).await;
    assert_eq!(status, StatusCode::OK, "{lower_ana}");
    assert_eq!(lower_ana["status"], "pending");
    let (status, lower_bob, bob) =
        rest(&server.state, &bob, "PUT", &format!("/api/projects/gov/members/{bob_id}"), Some(json!({ "role": "editor" }))).await;
    assert_eq!(status, StatusCode::OK, "{lower_bob}");
    assert_eq!(lower_bob["status"], "pending");
    let lower_bob_id = lower_bob["request_id"].as_str().unwrap().to_string();
    let (status, out, _) = rest(
        &server.state,
        &bob,
        "POST",
        &format!("/api/projects/gov/requests/{}/vote", lower_ana["request_id"].as_str().unwrap()),
        Some(json!({ "approve": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["status"], "applied");

    let mut events = event_socket(&server, &bob, "gov").await;
    assert_eq!(first_event(&mut events).await, json!({ "type": "role_changed", "role": "admin" }));
    let (status, out, _) =
        rest(&server.state, &bob, "POST", &format!("/api/projects/gov/requests/{lower_bob_id}/vote"), Some(json!({ "approve": true }))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out, json!({ "status": "cancelled" }));
    next_event(&mut events, "requests_changed").await;
    let (_, gov, _) = rest(&server.state, &bob, "GET", "/api/projects/gov/governance", None).await;
    assert!(gov["requests"].as_array().unwrap().is_empty(), "{gov}");
    assert_eq!(server.state.store.role_of("gov", &bob_id).unwrap(), Some(galley_server::auth::Role::Admin));
    let audited: i64 = server
        .state
        .store
        .db()
        .conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE project_id = 'gov' AND action = 'governance.cancel' AND detail = ?1",
            [&lower_bob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);
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

// The project roster must work before any document is opened, across files, and across tabs.
// Comparing authenticated ids also proves two people with the same display name stay distinct.
type EventSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn event_socket(server: &Server, auth: &Auth, project: &str) -> EventSocket {
    let mut request = format!("ws://{}/ws/{project}/events", server.addr).into_client_request().unwrap();
    request.headers_mut().insert("Cookie", auth.cookie_header().unwrap().parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

/// Skip ping, text and binary frames until the server closes. Returns the close code, or `None`
/// when the socket ends without one. Fails after 5 s.
async fn next_close(ws: &mut EventSocket) -> Option<u16> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(Some(frame)))) => return Some(u16::from(frame.code)),
                Some(Ok(Message::Close(None))) | None | Some(Err(_)) => return None,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .expect("the socket did not close in time")
}

/// The next event of type `kind`, skipping other events. Fails if the socket closes first.
async fn next_event(socket: &mut EventSocket, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await.expect("event socket closed").unwrap() {
                Message::Text(text) => {
                    let event: Value = serde_json::from_str(&text).unwrap();
                    if event["type"] == kind { return event; }
                }
                Message::Close(frame) => panic!("event socket closed before {kind}: {frame:?}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} event in time"))
}

/// The first frame an event socket sends.
async fn first_event(socket: &mut EventSocket) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(5), socket.next()).await.unwrap().unwrap().unwrap();
    let Message::Text(text) = msg else { panic!("the first frame is not text: {msg:?}") };
    serde_json::from_str(&text).unwrap()
}

async fn roster(socket: &mut EventSocket, expected: &[&str]) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await.expect("event socket closed").unwrap() {
                Message::Text(text) => {
                    let event: Value = serde_json::from_str(&text).unwrap();
                    if event["type"] != "presence" { continue; }
                    let mut ids: Vec<_> = event["users"].as_array().unwrap().iter().map(|u| u["id"].as_str().unwrap()).collect();
                    ids.sort_unstable();
                    let mut expected = expected.to_vec();
                    expected.sort_unstable();
                    if ids == expected { return event; }
                }
                Message::Ping(_) => socket.flush().await.unwrap(),
                _ => {}
            }
        }
    }).await.expect("presence roster did not converge")
}

#[tokio::test]
async fn project_presence_tracks_people_across_files_tabs_and_reconnections() {
    let server = start().await;
    let ana = signup(&server.state, "presence-a@uni.edu", "Same Name").await;
    let bob = signup(&server.state, "presence-b@uni.edu", "Same Name").await;
    let (_, meta, _) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({"name":"Presence"}))).await;
    let id = meta["id"].as_str().unwrap();
    let (status, _, _) = rest(&server.state, &ana, "POST", &format!("/api/projects/{id}/members"), Some(json!({"email":"presence-b@uni.edu","role":"viewer"}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, a, _) = rest(&server.state, &ana, "GET", "/api/auth/me", None).await;
    let (_, b, _) = rest(&server.state, &bob, "GET", "/api/auth/me", None).await;
    let a = a["user"]["id"].as_str().unwrap();
    let b = b["user"]["id"].as_str().unwrap();

    let mut first = event_socket(&server, &ana, id).await;
    roster(&mut first, &[a]).await;
    let mut second = event_socket(&server, &bob, id).await;
    let snapshot = roster(&mut second, &[a, b]).await;
    assert!(snapshot["users"].as_array().unwrap().iter().all(|u| u["name"] == "Same Name"));
    roster(&mut first, &[a, b]).await;
    let mut extra_tab = event_socket(&server, &ana, id).await;
    roster(&mut extra_tab, &[a, b]).await;

    let _main = Client::connect(&server.addr, &ana, id, "main.tex").await.unwrap();
    let _refs = Client::connect(&server.addr, &bob, id, "refs.bib").await.unwrap();
    // A replacement socket connects before the old one closes (a normal reconnect race).
    let mut replacement = event_socket(&server, &bob, id).await;
    roster(&mut replacement, &[a, b]).await;
    second.close(None).await.unwrap();
    drop(second);
    // A rename produces a later snapshot, after the old socket's close has been processed.
    let (status, _, _) = rest(&server.state, &bob, "POST", "/api/auth/name", Some(json!({"name":"Bob Renamed"}))).await;
    assert_eq!(status, StatusCode::OK);
    replacement.send(Message::Text(json!({"type":"presence_refresh","id":a,"name":"Spoofed"}).to_string().into())).await.unwrap();
    let renamed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = roster(&mut replacement, &[a, b]).await;
            if snapshot["users"].as_array().unwrap().iter().any(|u| u["id"] == b && u["name"] == "Bob Renamed") { break snapshot; }
        }
    }).await.unwrap();
    assert!(renamed["users"].as_array().unwrap().iter().any(|u| u["id"] == a && u["name"] == "Same Name"));
    first.close(None).await.unwrap();
    drop(first);
    extra_tab.send(Message::Text(json!({"type":"presence_refresh"}).to_string().into())).await.unwrap();
    roster(&mut extra_tab, &[a, b]).await;
    extra_tab.close(None).await.unwrap();
    drop(extra_tab);
    roster(&mut replacement, &[b]).await;

    // A second project has an independent roster, even for the same account.
    let (_, other, _) = rest(&server.state, &bob, "POST", "/api/projects", Some(json!({"name":"Other presence"}))).await;
    let mut other = event_socket(&server, &bob, other["id"].as_str().unwrap()).await;
    roster(&mut other, &[b]).await;
}

#[tokio::test]
async fn deleting_folders_and_projects_enforces_roles_and_cleans_persistent_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.server.public_signup = true;
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let admin = signup(&state, "delete-admin@example.com", "Admin").await;
    let editor = signup(&state, "delete-editor@example.com", "Editor").await;
    let viewer = signup(&state, "delete-viewer@example.com", "Viewer").await;
    let outsider = signup(&state, "delete-outsider@example.com", "Outsider").await;
    let (_, meta, _) = rest(&state, &admin, "POST", "/api/projects", Some(json!({"name":"Delete paper"}))).await;
    let id = meta["id"].as_str().unwrap();
    let url = format!("/api/projects/{id}");
    for (email, role) in [("delete-editor@example.com", "editor"), ("delete-viewer@example.com", "viewer")] {
        assert_eq!(rest(&state, &admin, "POST", &format!("{url}/members"), Some(json!({"email": email, "role": role}))).await.0, StatusCode::CREATED);
    }
    let project = state.registry.open(id).await.unwrap();
    project.create_folder("drafts/empty").unwrap();
    project.put_file("drafts/nested/text.tex", b"draft", false, "Admin").await.unwrap();
    for (auth, expected) in [(&viewer, StatusCode::FORBIDDEN), (&outsider, StatusCode::NOT_FOUND)] {
        assert_eq!(rest(&state, auth, "DELETE", &format!("{url}/folders?path=drafts"), None).await.0, expected);
    }
    assert!(project.exists("drafts/nested/text.tex"));
    let (status, deleted, _) = rest(&state, &editor, "DELETE", &format!("{url}/folders?path=drafts"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted_files"], json!(["drafts/nested/text.tex"]));
    assert!(project.list_folders().unwrap().is_empty());
    project.put_file("sections/main.tex", b"main", false, "Admin").await.unwrap();
    assert_eq!(rest(&state, &admin, "PATCH", &format!("{url}/settings"), Some(json!({"main_file":"sections/main.tex"}))).await.0, StatusCode::OK);
    assert_eq!(rest(&state, &admin, "DELETE", &format!("{url}/folders?path=sections"), None).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(rest(&state, &admin, "DELETE", &format!("{url}/folders?path=../escape"), None).await.0, StatusCode::BAD_REQUEST);
    for (auth, expected) in [(&editor, StatusCode::FORBIDDEN), (&viewer, StatusCode::FORBIDDEN), (&outsider, StatusCode::NOT_FOUND)] {
        assert_eq!(rest(&state, auth, "DELETE", &url, None).await.0, expected);
    }
    // Populate every project-scoped table, including votes whose project is indirect.
    let conn = state.store.db().conn().unwrap();
    conn.execute_batch(&format!(r#"
        INSERT INTO share_links VALUES ('delete-share','{id}','viewer','delete-hash',NULL,NULL,'u','now',0);
        INSERT INTO comments VALUES ('delete-comment','{id}','main.tex','anchor',NULL,'u','User','body',0,'now');
        INSERT INTO suggestions VALUES ('delete-suggestion','{id}','main.tex','a','b','old','new','u','User','open','now');
        INSERT INTO project_config VALUES ('{id}','solo');
        INSERT INTO role_requests VALUES ('delete-request','{id}','invite',NULL,'{{}}','invite','u','User','open','now',NULL);
        INSERT INTO role_request_votes VALUES ('delete-request','u','approve','now');
        INSERT INTO checkpoints VALUES ('delete-checkpoint','{id}','sha','label',NULL,'now');
        INSERT INTO agent_runs VALUES ('delete-run','{id}','u','test',NULL,'summary',1,'now');
    "#)).unwrap();
    drop(conn);
    let builds = state.builds(id).await.unwrap();
    let doc = project.doc("sections/main.tex").await.unwrap();
    assert_eq!(rest(&state, &admin, "DELETE", &url, None).await.0, StatusCode::NO_CONTENT);
    assert!(project.is_deleted());
    assert!(!dir.path().join("data").join(id).exists());
    assert!(state.registry.open(id).await.is_err());
    assert!(state.builds(id).await.is_err());
    assert!(project.create_file("resurrect.tex").await.is_err());
    doc.reseed("stale edit").await;
    builds.request(galley_build::BuildRequest::default()).await;
    assert!(!builds.is_running().await);
    assert_eq!(rest(&state, &admin, "GET", &url, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(rest(&state, &admin, "GET", "/api/projects", None).await.1, json!([]));
    for table in ["members", "share_links", "comments", "suggestions", "project_config", "role_requests", "checkpoints", "agent_runs"] {
        let count: i64 = state.store.db().conn().unwrap().query_row(&format!("SELECT COUNT(*) FROM {table} WHERE project_id = ?1"), [id], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "left records in {table}");
    }
    let votes: i64 = state.store.db().conn().unwrap().query_row("SELECT COUNT(*) FROM role_request_votes WHERE request_id = 'delete-request'", [], |r| r.get(0)).unwrap();
    assert_eq!(votes, 0);
    let reopened = AppState::new(dir.path(), &config).await.unwrap();
    assert!(reopened.registry.open(id).await.is_err());
    assert_eq!(rest(&reopened, &admin, "GET", "/api/projects", None).await.1, json!([]));
    assert!(!dir.path().join("data").join(id).exists(), "stale writers must not recreate the project");
}

/// POST JSON with no session, and return the status and the raw body bytes.
async fn post_raw(state: &AppState, path: &str, body: Value) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    (status, res.into_body().collect().await.unwrap().to_bytes().to_vec())
}

#[tokio::test]
async fn failed_logins_look_the_same_for_unknown_and_known_emails() {
    let server = start().await;
    signup(&server.state, "known@uni.edu", "Known").await;
    server.state.store.create_oidc_user("oidc@uni.edu", "O", "sub-1", false).unwrap();

    let mut bodies = Vec::new();
    for (email, password) in [("nobody@uni.edu", "supersecret"), ("known@uni.edu", "wrong-password"), ("oidc@uni.edu", "supersecret")] {
        let (status, body) = post_raw(&server.state, "/api/auth/login", json!({ "email": email, "password": password })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{email}");
        bodies.push(body);
    }
    assert_eq!(bodies[0], bodies[1], "an unknown email answers like a wrong password");
    assert_eq!(bodies[0], bodies[2], "a single sign-on account answers like a wrong password");
    let error: Value = serde_json::from_slice(&bodies[0]).unwrap();
    assert_eq!(error, json!({ "error": "That email and password don't match." }));

    let (status, _) = post_raw(&server.state, "/api/auth/login", json!({ "email": "known@uni.edu", "password": "supersecret" })).await;
    assert_eq!(status, StatusCode::OK, "the right password still signs in");
}

#[tokio::test]
async fn a_login_waits_for_a_free_password_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    config.limits.enabled = false;
    let state = AppState::new(dir.path(), &config).await.unwrap();
    let slots = std::thread::available_parallelism().map_or(2, |n| n.get());
    assert_eq!(state.password_slots.available_permits(), slots);

    let held = state.password_slots.clone().acquire_many_owned(slots as u32).await.unwrap();
    let login = {
        let state = state.clone();
        tokio::spawn(async move {
            post_raw(&state, "/api/auth/login", json!({ "email": "nobody@uni.edu", "password": "supersecret" })).await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!login.is_finished(), "no argon2 work starts while every slot is taken");
    drop(held);
    let (status, _) = tokio::time::timeout(Duration::from_secs(10), login).await.expect("login finishes once a slot frees").unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(state.password_slots.available_permits(), slots, "the login gave its slot back");
}

/// A JSON-RPC ping to a project's MCP route, with extra headers. Returns the status and JSON body.
async fn mcp_ping(state: &AppState, project: &str, headers: &[(&str, String)]) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/mcp/{project}"))
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    let body = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
    let res = router(state.clone()).oneshot(builder.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn guests_cannot_create_or_use_device_tokens() {
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    let (_, _, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Paper" }))).await;
    let (status, link, ana) =
        rest(&server.state, &ana, "POST", "/api/projects/paper/shares", Some(json!({ "role": "editor", "label": "Team" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let token = link["path"].as_str().unwrap().strip_prefix("/s/").unwrap().to_string();
    let (status, landed, guest) =
        rest(&server.state, &Auth::default(), "POST", "/api/auth/landing", Some(json!({ "token": token, "name": "Guest" }))).await;
    assert_eq!(status, StatusCode::OK, "{landed}");
    assert_eq!(landed["user"]["is_guest"], true);
    let guest_id = landed["user"]["id"].as_str().unwrap().to_string();

    // A guest cannot mint a token.
    let (status, refused, _) = rest(&server.state, &guest, "POST", "/api/auth/tokens", Some(json!({ "label": "cli" }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refused, json!({ "error": "Guests can't create access tokens. Create an account to connect AI tools." }));
    assert!(server.state.store.device_tokens(&guest_id).unwrap().is_empty());

    // A guest token made some other way does not authenticate, and is never touched.
    let (_, guest_token) = server.state.store.create_device_token(&guest_id, "x").unwrap();
    let bearer = [("authorization", format!("Bearer {guest_token}"))];
    let res = router(server.state.clone())
        .oneshot(Request::builder().uri("/api/projects").header("authorization", format!("Bearer {guest_token}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let (status, _) = mcp_ping(&server.state, "paper", &bearer).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let tokens = server.state.store.device_tokens(&guest_id).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].last_used_at, None, "a refused token is never marked used");

    // An account's token still works.
    let (status, made, _) = rest(&server.state, &ana, "POST", "/api/auth/tokens", Some(json!({ "label": "cli" }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, pong) = mcp_ping(&server.state, "paper", &[("authorization", format!("Bearer {}", made["token"].as_str().unwrap()))]).await;
    assert_eq!(status, StatusCode::OK, "{pong}");
}

/// Try to open a project's event socket with the given headers.
async fn try_event_socket(
    addr: &str,
    project: &str,
    headers: &[(&'static str, String)],
) -> Result<EventSocket, tokio_tungstenite::tungstenite::Error> {
    let mut request = format!("ws://{addr}/ws/{project}/events").into_client_request().unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await.map(|(socket, _)| socket)
}

const CROSS_SITE: &str = "Cross-site connections are not allowed.";

fn assert_socket_refused(result: Result<EventSocket, tokio_tungstenite::tungstenite::Error>) {
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::FORBIDDEN);
            let body: Value = serde_json::from_slice(r.body().as_deref().unwrap_or_default()).unwrap();
            assert_eq!(body, json!({ "error": CROSS_SITE }));
        }
        Err(other) => panic!("expected a 403 handshake, got {other}"),
        Ok(_) => panic!("a cross-site socket connected"),
    }
}

#[tokio::test]
async fn cross_site_origins_are_refused_on_sockets_and_mcp() {
    let evil = || ("origin", "https://evil.example".to_string());
    let server = start().await;
    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    let (status, _, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Paper" }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = || ("cookie", ana.cookie_header().unwrap());

    // Sockets: another site is refused, the page's own origin connects.
    assert_socket_refused(try_event_socket(&server.addr, "paper", &[cookie(), evil()]).await);
    let own = ("origin", format!("http://{}", server.addr));
    try_event_socket(&server.addr, "paper", &[cookie(), own.clone()]).await.expect("same-origin socket");
    try_event_socket(&server.addr, "paper", &[cookie()]).await.expect("a socket without an Origin");
    // Sockets authenticate by cookie only, so a Bearer header never skips the check.
    assert_socket_refused(
        try_event_socket(&server.addr, "paper", &[cookie(), evil(), ("authorization", "Bearer x".into())]).await,
    );

    // MCP with the session cookie: another site is refused, before the session is even looked up.
    let refusal = json!({ "error": CROSS_SITE });
    assert_eq!(mcp_ping(&server.state, "paper", &[cookie(), evil()]).await, (StatusCode::FORBIDDEN, refusal.clone()));
    assert_eq!(
        mcp_ping(&server.state, "paper", &[("cookie", "galley_session=not-a-session".into()), evil()]).await,
        (StatusCode::FORBIDDEN, refusal.clone())
    );
    let (status, _) = mcp_ping(&server.state, "paper", &[cookie(), own.clone(), ("host", server.addr.clone())]).await;
    assert_eq!(status, StatusCode::OK, "the page's own origin");
    // A Bearer call cannot be ridden by another site, so any Origin passes.
    let (status, made, _) = rest(&server.state, &ana, "POST", "/api/auth/tokens", Some(json!({ "label": "cli" }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let bearer = ("authorization", format!("Bearer {}", made["token"].as_str().unwrap()));
    let (status, pong) = mcp_ping(&server.state, "paper", &[bearer, evil()]).await;
    assert_eq!(status, StatusCode::OK, "{pong}");

    // With a domain set, only https://{domain} is accepted, whatever the Host.
    let public = start_with(|c| {
        c.server.domain = "galley.example".into();
        c.build.allow_unsandboxed = true;
    })
    .await;
    let admin = signup(&public.state, "admin@galley.example", "Admin").await;
    let (status, _, admin) = rest(&public.state, &admin, "POST", "/api/projects", Some(json!({ "name": "Paper" }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = ("cookie", admin.cookie_header().unwrap());
    try_event_socket(&public.addr, "paper", &[cookie.clone(), ("origin", "https://galley.example".into())])
        .await
        .expect("the public origin connects");
    assert_socket_refused(try_event_socket(&public.addr, "paper", &[cookie.clone(), ("origin", format!("http://{}", public.addr))]).await);
    assert_socket_refused(try_event_socket(&public.addr, "paper", &[cookie, evil()]).await);
}

/// The signed-in user's id.
async fn user_id(state: &AppState, auth: &Auth) -> String {
    let (_, me, _) = rest(state, auth, "GET", "/api/auth/me", None).await;
    me["user"]["id"].as_str().unwrap().to_string()
}

/// An owner's project with a second account invited at `role`. Returns the owner, the member,
/// the member's id and the project id.
async fn project_with_member(server: &Server, role: &str) -> (Auth, Auth, String, String) {
    let ana = signup(&server.state, "ana@uni.edu", "Ana").await;
    let bob = signup(&server.state, "bob@uni.edu", "Bob").await;
    let (status, meta, ana) = rest(&server.state, &ana, "POST", "/api/projects", Some(json!({ "name": "Access" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{meta}");
    let id = meta["id"].as_str().unwrap().to_string();
    let (status, out, ana) =
        rest(&server.state, &ana, "POST", &format!("/api/projects/{id}/members"), Some(json!({ "email": "bob@uni.edu", "role": role }))).await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["status"], "applied");
    let bob_id = user_id(&server.state, &bob).await;
    (ana, bob, bob_id, id)
}

#[tokio::test]
async fn a_removed_member_loses_both_sockets() {
    let server = start().await;
    let (ana, bob, bob_id, id) = project_with_member(&server, "editor").await;
    let mut events = event_socket(&server, &bob, &id).await;
    assert_eq!(first_event(&mut events).await, json!({ "type": "role_changed", "role": "editor" }));
    let mut doc = Client::connect(&server.addr, &bob, &id, "main.tex").await.unwrap();

    let (status, out, _) = rest(&server.state, &ana, "DELETE", &format!("/api/projects/{id}/members/{bob_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["status"], "applied");
    next_event(&mut events, "access_removed").await;
    assert_eq!(next_close(&mut events).await, Some(4403));
    assert_eq!(next_close(&mut doc.ws).await, Some(4403));
}

/// Insert marker `P{n};` on `client`'s socket, prove the server handled it, and report whether
/// the server's document has it. The update carries the client's whole state, so markers the
/// server dropped while the client was read-only travel with it.
async fn probe(server: &Server, owner: &Auth, client: &mut Client, id: &str, n: u32) -> bool {
    let marker = format!("P{n};");
    {
        let text = client.doc.get_or_insert_text("content");
        text.insert(&mut client.doc.transact_mut(), 0, &marker);
    }
    let update = client.doc.transact().encode_state_as_update_v1(&yrs::StateVector::default());
    client.send(YMessage::Sync(SyncMessage::Update(update)).encode_v1()).await;
    // The server answers sync step 1 with step 2 on the same socket, after the update before it.
    let sv = client.doc.transact().state_vector();
    client.send(YMessage::Sync(SyncMessage::SyncStep1(sv)).encode_v1()).await;
    loop {
        let frame = client.recv_binary().await;
        let mut dec = DecoderV1::new(Cursor::new(&frame));
        if MessageReader::new(&mut dec).any(|m| matches!(m, Ok(YMessage::Sync(SyncMessage::SyncStep2(_))))) {
            break;
        }
    }
    let fresh = Client::connect(&server.addr, owner, id, "main.tex").await.unwrap();
    fresh.text().contains(&marker)
}

#[tokio::test]
async fn a_lowered_editor_turns_read_only_while_connected() {
    let server = start().await;
    let (ana, bob, bob_id, id) = project_with_member(&server, "editor").await;
    let mut events = event_socket(&server, &bob, &id).await;
    assert_eq!(first_event(&mut events).await, json!({ "type": "role_changed", "role": "editor" }));
    let mut doc = Client::connect(&server.addr, &bob, &id, "main.tex").await.unwrap();
    let mut n = 1;
    assert!(probe(&server, &ana, &mut doc, &id, n).await, "an editor's edit lands");

    let member = format!("/api/projects/{id}/members/{bob_id}");
    let (status, out, ana) = rest(&server.state, &ana, "PUT", &member, Some(json!({ "role": "viewer" }))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(next_event(&mut events, "role_changed").await["role"], "viewer");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        n += 1;
        if !probe(&server, &ana, &mut doc, &id, n).await { break; }
        assert!(tokio::time::Instant::now() < deadline, "the lowered editor's edits still land");
    }
    n += 1;
    assert!(!probe(&server, &ana, &mut doc, &id, n).await, "edits stay dropped");

    let (status, out, ana) = rest(&server.state, &ana, "PUT", &member, Some(json!({ "role": "editor" }))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(next_event(&mut events, "role_changed").await["role"], "editor");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        n += 1;
        if probe(&server, &ana, &mut doc, &id, n).await { break; }
        assert!(tokio::time::Instant::now() < deadline, "edits are not accepted again");
    }
}

#[tokio::test]
async fn signing_out_closes_only_that_sessions_sockets() {
    let server = start().await;
    let first = signup(&server.state, "ana@uni.edu", "Ana").await;
    let (status, _, second) = rest(
        &server.state,
        &Auth::default(),
        "POST",
        "/api/auth/login",
        Some(json!({ "email": "ana@uni.edu", "password": "supersecret" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(first.session, second.session, "two sessions");
    let uid = user_id(&server.state, &first).await;
    let (status, meta, first) = rest(&server.state, &first, "POST", "/api/projects", Some(json!({ "name": "Sessions" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{meta}");
    let id = meta["id"].as_str().unwrap().to_string();
    let mut one = event_socket(&server, &first, &id).await;
    let mut two = event_socket(&server, &second, &id).await;
    roster(&mut one, &[uid.as_str()]).await;
    roster(&mut two, &[uid.as_str()]).await;

    let (status, _, _) = rest(&server.state, &first, "POST", "/api/auth/logout", None).await;
    assert_eq!(status, StatusCode::OK);
    next_event(&mut one, "signed_out").await;
    assert_eq!(next_close(&mut one).await, Some(4401));

    // The other session's socket stays open: drain what is queued, then ask for a fresh roster.
    tokio::time::sleep(Duration::from_secs(1)).await;
    while let Ok(msg) = tokio::time::timeout(Duration::from_millis(200), two.next()).await {
        match msg.expect("the other session's socket ended").unwrap() {
            Message::Close(frame) => panic!("the other session's socket closed: {frame:?}"),
            Message::Ping(_) => two.flush().await.unwrap(),
            _ => {}
        }
    }
    two.send(Message::Text(json!({ "type": "presence_refresh" }).to_string().into())).await.unwrap();
    roster(&mut two, &[uid.as_str()]).await;

    let cookie = |auth: &Auth| [("cookie", auth.cookie_header().unwrap())];
    try_event_socket(&server.addr, &id, &cookie(&second)).await.expect("the other session still connects");
    match try_event_socket(&server.addr, &id, &cookie(&first)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), StatusCode::UNAUTHORIZED),
        Err(other) => panic!("expected a 401 handshake, got {other}"),
        Ok(_) => panic!("a signed-out session connected"),
    }
}

#[tokio::test]
async fn a_signed_in_member_raised_by_a_share_link_gets_role_changed() {
    let server = start().await;
    let (ana, bob, _, id) = project_with_member(&server, "viewer").await;
    let (status, link, _) = rest(&server.state, &ana, "POST", &format!("/api/projects/{id}/shares"), Some(json!({ "role": "editor", "label": "Team" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{link}");
    let token = link["path"].as_str().unwrap().strip_prefix("/s/").unwrap().to_string();
    let mut events = event_socket(&server, &bob, &id).await;
    assert_eq!(first_event(&mut events).await, json!({ "type": "role_changed", "role": "viewer" }));

    let (status, landed, _) = rest(&server.state, &bob, "POST", "/api/auth/landing", Some(json!({ "token": token, "name": "Bob" }))).await;
    assert_eq!(status, StatusCode::OK, "{landed}");
    assert_eq!(landed["role"], "editor");
    assert_eq!(next_event(&mut events, "role_changed").await["role"], "editor");
}
