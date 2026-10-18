//! Membership invitations grant access to existing accounts, after governance approval when required.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use galley_server::{router, AppState, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.build.sandbox = "none".into();
    let state = AppState::new(dir.path(), &config).await.unwrap();
    (dir, state)
}

fn account(state: &AppState, email: &str) -> String {
    let user = state.store.create_user(email, email, "supersecret", false).unwrap();
    let token = state.store.create_session(&user.id, 30).unwrap();
    format!("galley_session={token}; galley_csrf=invitation-test")
}

async fn rest(state: &AppState, cookie: &str, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router(state.clone()).oneshot(Request::builder()
        .method(method).uri(path)
        .header("cookie", cookie)
        .header("x-csrf-token", "invitation-test")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn invitations_are_visible_and_retries_do_not_change_roles() {
    let (_dir, state) = state().await;
    let admin = account(&state, "admin@example.com");
    let invitee = account(&state, "Invitee@Example.com");
    let (status, project) = rest(&state, &admin, "POST", "/api/projects", json!({"name": "Invitation test"})).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = project["id"].as_str().unwrap();
    let members = format!("/api/projects/{id}/members");
    let (_, projects) = rest(&state, &invitee, "GET", "/api/projects", Value::Null).await;
    assert!(projects.as_array().unwrap().is_empty());

    let (status, result) = rest(&state, &admin, "POST", &members, json!({"email": "  INVITEE@EXAMPLE.COM  ", "role": "editor"})).await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    assert_eq!(result["status"], "applied");
    let (_, projects) = rest(&state, &invitee, "GET", "/api/projects", Value::Null).await;
    assert_eq!(projects[0]["id"], id);
    assert_eq!(projects[0]["role"], "editor");

    let (status, result) = rest(&state, &admin, "POST", &members, json!({"email": "invitee@example.com", "role": "editor"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "applied");
    // The invite form cannot silently downgrade members or remove the only admin.
    for email in ["invitee@example.com", "admin@example.com"] {
        let (status, result) = rest(&state, &admin, "POST", &members, json!({"email": email, "role": "viewer"})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{result}");
        assert!(result["error"].as_str().unwrap().contains("already a member"));
    }
    assert_eq!(state.store.members(id).unwrap().len(), 2);
    assert_eq!(state.store.admin_ids(id).unwrap().len(), 1);
    // Editors cannot invite, and unknown accounts produce a useful explanation.
    let (status, _) = rest(&state, &invitee, "POST", &members, json!({"email": "missing@example.com", "role": "viewer"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, result) = rest(&state, &admin, "POST", &members, json!({"email": "missing@example.com", "role": "viewer"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(result["error"].as_str().unwrap().contains("sign in once"));
}

#[tokio::test]
async fn governed_invitation_retries_reuse_the_pending_request_until_approval() {
    let (_dir, state) = state().await;
    let admin = account(&state, "admin@example.com");
    let second = account(&state, "second@example.com");
    let invitee = account(&state, "invitee@example.com");
    let (_, project) = rest(&state, &admin, "POST", "/api/projects", json!({"name": "Governed invitations"})).await;
    let id = project["id"].as_str().unwrap();
    let members = format!("/api/projects/{id}/members");
    let (status, _) = rest(&state, &admin, "POST", &members, json!({"email": "second@example.com", "role": "admin"})).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = rest(&state, &admin, "PUT", &format!("/api/projects/{id}/governance"), json!({"mode": "unanimous"})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, result) = rest(&state, &admin, "POST", &members, json!({"email": "invitee@example.com", "role": "viewer"})).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(result["status"], "pending");
    let request_id = result["request_id"].as_str().unwrap();
    let (status, retried) = rest(&state, &admin, "POST", &members, json!({"email": "INVITEE@EXAMPLE.COM", "role": "viewer"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retried["request_id"], request_id);
    assert_eq!(state.store.open_requests(id).unwrap().len(), 1);
    let (status, _) = rest(&state, &admin, "POST", &members, json!({"email": "invitee@example.com", "role": "editor"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, projects) = rest(&state, &invitee, "GET", "/api/projects", Value::Null).await;
    assert!(projects.as_array().unwrap().is_empty(), "pending is not access");
    let (status, result) = rest(&state, &second, "POST", &format!("/api/projects/{id}/requests/{request_id}/vote"), json!({"approve": true})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["status"], "applied");
    let (_, projects) = rest(&state, &invitee, "GET", "/api/projects", Value::Null).await;
    assert_eq!(projects[0]["id"], id);
    assert_eq!(projects[0]["role"], "viewer");
    assert!(state.store.open_requests(id).unwrap().is_empty());
}
