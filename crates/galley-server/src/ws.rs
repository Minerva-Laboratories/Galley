//! WebSockets. There is one y-protocol socket for each client and document, and one JSON event
//! socket for each project. Both authenticate from the session cookie and enforce the caller's
//! role. The document socket is wire-compatible with `y-websocket`. A user who cannot edit
//! connects read-only.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum_extra::extract::cookie::CookieJar;
use galley_sync::project::Housekeeping;
use galley_sync::{DocHandle, ProjectSync};
use tokio::sync::broadcast::error::RecvError;

use crate::app::{AppError, AppState};
use crate::auth::current::SESSION_COOKIE;
use crate::auth::Role;
use crate::store::User;

/// Presence belongs to the project event connection, independently of open documents. Track
/// each socket separately so an old socket closing after a reconnect cannot remove the new one.
#[derive(Default)]
pub(crate) struct ProjectPresence {
    next: u64,
    projects: HashMap<String, BTreeMap<u64, (String, String)>>,
}

struct PresenceConnection {
    state: Arc<Mutex<ProjectPresence>>,
    project: String,
    connection: u64,
    events: tokio::sync::broadcast::Sender<String>,
}

impl ProjectPresence {
    pub(crate) fn forget_project(&mut self, project: &str) {
        self.projects.remove(project);
    }

    fn snapshot(&self, project: &str) -> String {
        let mut users = BTreeMap::new();
        if let Some(connections) = self.projects.get(project) {
            for (id, name) in connections.values() {
                users.insert(id, serde_json::json!({ "id": id, "name": name }));
            }
        }
        serde_json::json!({ "type": "presence", "users": users.values().collect::<Vec<_>>() }).to_string()
    }
}

impl PresenceConnection {
    fn join(state: Arc<Mutex<ProjectPresence>>, project: String, user: &User, events: tokio::sync::broadcast::Sender<String>) -> (Self, tokio::sync::broadcast::Receiver<String>) {
        let mut presence = state.lock().expect("presence poisoned");
        // Subscribe under the roster lock, before joining. No earlier roster can be queued
        // behind our initial snapshot, including when several sockets connect together.
        let receiver = events.subscribe();
        presence.next += 1;
        let connection = presence.next;
        presence.projects.entry(project.clone()).or_default().insert(connection, (user.id.clone(), user.name.clone()));
        let snapshot = presence.snapshot(&project);
        // Send under the same lock as the mutation, preserving roster order for every receiver.
        let _ = events.send(snapshot);
        drop(presence);
        (Self { state, project, connection, events }, receiver)
    }

    fn rename(&self, user: &User) {
        let mut presence = self.state.lock().expect("presence poisoned");
        if let Some(connections) = presence.projects.get_mut(&self.project) {
            for (id, name) in connections.values_mut() {
                if id == &user.id { *name = user.name.clone(); }
            }
        }
        let _ = self.events.send(presence.snapshot(&self.project));
    }
}

impl Drop for PresenceConnection {
    fn drop(&mut self) {
        let mut presence = self.state.lock().expect("presence poisoned");
        if let Some(connections) = presence.projects.get_mut(&self.project) {
            connections.remove(&self.connection);
            if connections.is_empty() { presence.projects.remove(&self.project); }
        }
        let _ = self.events.send(presence.snapshot(&self.project));
    }
}

/// Resolve the session cookie to a user, or 401.
async fn authenticate(app: &AppState, headers: &HeaderMap) -> Result<User, AppError> {
    let token = CookieJar::from_headers(headers)
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::Unauthorized("Sign in to connect.".into()))?;
    let store = app.store.clone();
    let user = tokio::task::spawn_blocking(move || store.session_user(&token))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    user.ok_or_else(|| AppError::Unauthorized("Your session expired. Reload the page.".into()))
}

pub async fn doc(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let user = authenticate(&app, &headers).await?;
    let role = app.require(&user, &id, Role::can_view, "opening this document").await?;
    let project = app.registry.open(&id).await?;
    // Only a file that exists gets a live document. An editor of a deleted file must not recreate it.
    if !project.exists(&path) {
        return Err(AppError::NotFound(format!("{path} is not in this project any more.")));
    }
    let doc = project.doc(&path).await?;
    let may_edit = role.can_edit();
    Ok(ws.on_upgrade(move |socket| async move {
        let edited = doc_session(socket, Arc::clone(&project), doc, user.name.clone(), may_edit).await;
        if edited {
            app.registry.touch(&id);
        }
    }))
}

/// Returns whether this client edited the document.
async fn doc_session(
    mut socket: WebSocket,
    project: Arc<ProjectSync>,
    doc: Arc<DocHandle>,
    name: String,
    may_edit: bool,
) -> bool {
    use std::collections::HashSet;
    let mut inbox = doc.subscribe();
    let mut project_events = project.subscribe_events();
    let mut announced: HashSet<yrs::block::ClientID> = HashSet::new();
    let mut edited_any = false;
    if project.is_deleted() || !project.exists(doc.rel_path()) { return false; }

    match doc.start_message().await {
        Ok(start) => {
            if socket.send(Message::Binary(start.into())).await.is_err() {
                return false;
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not start sync");
            return false;
        }
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                match msg {
                    Message::Binary(bytes) => {
                        if project.is_deleted() || !project.exists(doc.rel_path()) { break; }
                        match doc.handle(&bytes, may_edit).await {
                        Ok(handled) => {
                            if handled.rejected {
                                // The server drops the updates from a read-only client. The client
                                // stays connected, so it keeps receiving edits and shows presence.
                                tracing::debug!(path = doc.rel_path(), "dropped an update from a read-only client");
                            }
                            if handled.edited {
                                edited_any = true;
                                project.set_last_editor(&name);
                            }
                            announced.extend(handled.awareness_clients);
                            let mut failed = false;
                            for reply in handled.replies {
                                if socket.send(Message::Binary(reply.into())).await.is_err() {
                                    failed = true;
                                    break;
                                }
                            }
                            if failed { break; }
                        }
                        Err(e) => {
                            tracing::warn!(path = doc.rel_path(), error = %e, "bad sync frame; closing");
                            break;
                        }
                        }
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            event = project_events.recv() => match event {
                Ok(galley_sync::project::ProjectEvent::ProjectDeleted) => break,
                Ok(galley_sync::project::ProjectEvent::FileDeleted { path }) if path == doc.rel_path() => break,
                Err(RecvError::Closed) => break,
                Err(RecvError::Lagged(_)) if project.is_deleted() || !project.exists(doc.rel_path()) => break,
                _ => {}
            },
            outgoing = inbox.recv() => match outgoing {
                Ok(bytes) => {
                    if socket.send(Message::Binary(bytes.to_vec().into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(path = doc.rel_path(), skipped = n, "client lagged; closing for resync");
                    break;
                }
                Err(RecvError::Closed) => break,
            }
        }
    }

    for client in announced {
        doc.forget_client(client).await;
    }
    let _ = project.housekeeping().send(Housekeeping::Disconnected);
    edited_any
}

pub async fn events(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let user = authenticate(&app, &headers).await?;
    app.require(&user, &id, Role::can_view, "watching this project").await?;
    let project = app.registry.open(&id).await?;
    let builds = app.builds(&id).await?;
    let collab = app.collab_channel(&id).await;
    Ok(ws.on_upgrade(move |socket| events_session(socket, project, builds, collab, app, id, user)))
}

/// One socket carries sync events for commits and files, build events, and collaboration events.
/// Every frame is a JSON object with a `type` field.
async fn events_session(
    mut socket: WebSocket,
    project: Arc<ProjectSync>,
    builds: Arc<galley_build::ProjectBuilds>,
    collab: tokio::sync::broadcast::Sender<String>,
    app: AppState,
    project_id: String,
    user: User,
) {
    let mut rx = project.subscribe_events();
    let mut brx = builds.subscribe();
    if project.is_deleted() { return; }
    let (presence, mut crx) = PresenceConnection::join(app.presence.clone(), project_id, &user, collab);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_pong = tokio::time::Instant::now();
    loop {
        let frame = tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Pong(_))) => { last_pong = tokio::time::Instant::now(); continue; }
                Some(Ok(Message::Text(text))) => {
                    // A display-name save asks for a fresh authenticated name, never an id or
                    // name supplied by the browser. Update all this user's tabs together.
                    if serde_json::from_str::<serde_json::Value>(&text).ok().is_some_and(|v| v["type"] == "presence_refresh") {
                        let store = app.store.clone();
                        let id = user.id.clone();
                        if let Ok(Ok(Some(user))) = tokio::task::spawn_blocking(move || store.user_by_id(&id)).await {
                            presence.rename(&user);
                        }
                    }
                    continue;
                }
                _ => continue,
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() >= Duration::from_secs(45) { break; }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                continue;
            },
            ev = rx.recv() => match ev {
                Ok(galley_sync::project::ProjectEvent::ProjectDeleted) => {
                    let _ = socket.send(Message::Text(serde_json::json!({"type":"project_deleted"}).to_string().into())).await;
                    break;
                }
                Ok(ev) => serde_json::to_string(&ev).unwrap_or_default(),
                // Reconnect to recover a complete presence snapshot after dropped events.
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => break,
            },
            ev = brx.recv() => match ev {
                Ok(ev) => serde_json::to_string(&ev).unwrap_or_default(),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
            ev = crx.recv() => match ev {
                Ok(text) => text,
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => break,
            },
        };
        if socket.send(Message::Text(frame.into())).await.is_err() {
            break;
        }
    }
}
