//! WebSockets. There is one y-protocol socket for each client and document, and one JSON event
//! socket for each project. Both authenticate from the session cookie and enforce the caller's
//! role. The document socket is wire-compatible with `y-websocket`. A user who cannot edit
//! connects read-only. Both re-check the role and then the session when an access change concerns
//! them, and every minute. A removed member's sockets close with 4403, a signed-out session's with
//! 4401, and a changed role applies in place.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum_extra::extract::cookie::CookieJar;
use galley_sync::project::Housekeeping;
use galley_sync::{DocHandle, ProjectSync};
use tokio::sync::broadcast::error::RecvError;

use crate::app::{AccessChanged, AppError, AppState};
use crate::auth::current::SESSION_COOKIE;
use crate::auth::{Role, Secret};
use crate::store::User;

/// How often an open socket re-checks its access when no change event arrives. This catches a
/// change made outside this process, such as a password reset from the CLI.
const ACCESS_RECHECK: Duration = Duration::from_secs(60);

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

/// Resolve the session cookie to a user, or 401, then count the connection against the account's
/// `socket_account` limit (429 when over). The session token comes back too, redacted on `Debug`,
/// so the socket can re-check that this session is still signed in.
async fn authenticate(app: &AppState, headers: &HeaderMap) -> Result<(User, Secret), AppError> {
    let token = CookieJar::from_headers(headers)
        .get(SESSION_COOKIE)
        .map(|c| Secret(c.value().to_string()))
        .ok_or_else(|| AppError::Unauthorized("Sign in to connect.".into()))?;
    let store = app.store.clone();
    let lookup = token.clone();
    let user = tokio::task::spawn_blocking(move || store.session_user(&lookup.0))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let user = user.ok_or_else(|| AppError::Unauthorized("Your session expired. Reload the page.".into()))?;
    // Stage 2 of the rate limits for sockets: per account, so opening more sessions buys nothing.
    app.limits
        .hit(crate::limits::RuleId::SocketAccount, crate::limits::Key::Account(user.id.clone()))
        .map_err(AppError::TooManyRequests)?;
    Ok((user, token))
}

/// The outcome of re-checking an open socket's access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recheck {
    /// The user is no longer a member of the project.
    Removed,
    /// The socket's session ended, or now belongs to someone else.
    SignedOut,
    /// Still a member, with this role.
    Role(Role),
}

/// Re-check the role first, then the session, in one blocking task. The role comes first so a
/// guest whose membership and session were both removed is told "removed". A database error
/// gives `None`: the socket keeps its current state and is never closed for it.
pub(crate) async fn recheck(app: &AppState, token: &Secret, user_id: &str, project_id: &str) -> Option<Recheck> {
    let store = app.store.clone();
    let (token, uid, pid) = (token.clone(), user_id.to_string(), project_id.to_string());
    let checked = tokio::task::spawn_blocking(move || -> Result<Recheck, crate::db::DbError> {
        let Some(role) = store.role_of(&pid, &uid)? else { return Ok(Recheck::Removed) };
        Ok(match store.session_user(&token.0)? {
            Some(user) if user.id == uid => Recheck::Role(role),
            _ => Recheck::SignedOut,
        })
    })
    .await;
    match checked {
        Ok(Ok(outcome)) => Some(outcome),
        Ok(Err(e)) => {
            tracing::warn!(project = project_id, error = %e, "could not re-check socket access; keeping it open");
            None
        }
        Err(e) => {
            tracing::warn!(project = project_id, error = %e, "could not re-check socket access; keeping it open");
            None
        }
    }
}

/// What an open socket needs to re-check its access: who it is for, its session, and the access
/// change events since before its connect check.
struct SocketAccess {
    app: AppState,
    user_id: String,
    token: Secret,
    project_id: String,
    changes: tokio::sync::broadcast::Receiver<AccessChanged>,
    /// Whether `changes` can still deliver. The sender lives as long as the process, so this
    /// stays true in practice; if it ever closes, the periodic re-check carries on alone.
    open: bool,
}

/// After an access re-check: the socket was closed, its state stays, or its role is this one.
enum Rechecked {
    Closed,
    Keep,
    Role(Role),
}

impl SocketAccess {
    /// Whether a received access change concerns this socket. A lagged or closed channel may
    /// have dropped one that did, so it counts.
    fn concerns(&mut self, changed: Result<AccessChanged, RecvError>) -> bool {
        match changed {
            Ok(AccessChanged::Project(p)) => p == self.project_id,
            Ok(AccessChanged::User(u)) => u == self.user_id,
            Err(RecvError::Lagged(_)) => true,
            Err(RecvError::Closed) => {
                self.open = false;
                true
            }
        }
    }

    /// Re-check, and close the socket when access is gone. The event socket first sends a JSON
    /// notice (`access_removed` or `signed_out`) so the page knows why.
    async fn recheck_or_close(&self, socket: &mut WebSocket, notice: bool) -> Rechecked {
        let (kind, code, reason) = match recheck(&self.app, &self.token, &self.user_id, &self.project_id).await {
            None => return Rechecked::Keep,
            Some(Recheck::Role(role)) => return Rechecked::Role(role),
            Some(Recheck::Removed) => ("access_removed", 4403, "access removed"),
            Some(Recheck::SignedOut) => ("signed_out", 4401, "signed out"),
        };
        if notice {
            let frame = serde_json::json!({ "type": kind }).to_string();
            let _ = socket.send(Message::Text(frame.into())).await;
        }
        let _ = socket.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
        // Wait briefly for the peer's close reply. Dropping the connection while the peer's
        // frames are unread resets it, and the peer could lose the notice and the close code.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(msg)) = socket.recv().await {
                if matches!(msg, Message::Close(_)) { break; }
            }
        })
        .await;
        Rechecked::Closed
    }
}

/// A periodic re-check timer whose first tick is one period away.
fn recheck_timer() -> tokio::time::Interval {
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + ACCESS_RECHECK, ACCESS_RECHECK);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer
}

fn role_changed(role: Role) -> String {
    serde_json::json!({ "type": "role_changed", "role": role.as_str() }).to_string()
}

pub async fn doc(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // Subscribe before the connect check, so a change made after it is never missed.
    let changes = app.subscribe_access();
    let (user, token) = authenticate(&app, &headers).await?;
    let role = app.require(&user, &id, Role::can_view, "opening this document").await?;
    let project = app.registry.open(&id).await?;
    // Only a file that exists gets a live document. An editor of a deleted file must not recreate it.
    if !project.exists(&path) {
        return Err(AppError::NotFound(format!("{path} is not in this project any more.")));
    }
    let doc = project.doc(&path).await?;
    let may_edit = role.can_edit();
    let access = SocketAccess { app: app.clone(), user_id: user.id.clone(), token, project_id: id.clone(), changes, open: true };
    Ok(ws.on_upgrade(move |socket| async move {
        let edited = doc_session(socket, Arc::clone(&project), doc, user.name.clone(), access, may_edit).await;
        if edited {
            app.registry.touch(&id);
        }
    }))
}

/// Returns whether this client edited the document. A role change sets `may_edit` in place; it
/// applies from the next incoming update.
async fn doc_session(
    mut socket: WebSocket,
    project: Arc<ProjectSync>,
    doc: Arc<DocHandle>,
    name: String,
    mut access: SocketAccess,
    mut may_edit: bool,
) -> bool {
    use std::collections::HashSet;
    let mut recheck_tick = recheck_timer();
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
            },
            changed = access.changes.recv(), if access.open => {
                if !access.concerns(changed) { continue; }
                match access.recheck_or_close(&mut socket, false).await {
                    Rechecked::Closed => break,
                    Rechecked::Role(role) => may_edit = role.can_edit(),
                    Rechecked::Keep => {}
                }
            },
            _ = recheck_tick.tick() => match access.recheck_or_close(&mut socket, false).await {
                Rechecked::Closed => break,
                Rechecked::Role(role) => may_edit = role.can_edit(),
                Rechecked::Keep => {}
            },
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
    // Subscribe before the connect check, so a change made after it is never missed.
    let changes = app.subscribe_access();
    let (user, token) = authenticate(&app, &headers).await?;
    let role = app.require(&user, &id, Role::can_view, "watching this project").await?;
    let project = app.registry.open(&id).await?;
    let builds = app.builds(&id).await?;
    let collab = app.collab_channel(&id).await;
    let access = SocketAccess { app, user_id: user.id.clone(), token, project_id: id, changes, open: true };
    Ok(ws.on_upgrade(move |socket| events_session(socket, project, builds, collab, access, user, role)))
}

/// One socket carries sync events for commits and files, build events, and collaboration events.
/// Every frame is a JSON object with a `type` field. The first frame is `role_changed` with the
/// caller's role at connect; later ones come only when a re-check finds a different role.
async fn events_session(
    mut socket: WebSocket,
    project: Arc<ProjectSync>,
    builds: Arc<galley_build::ProjectBuilds>,
    collab: tokio::sync::broadcast::Sender<String>,
    mut access: SocketAccess,
    user: User,
    mut role: Role,
) {
    let app = access.app.clone();
    let mut rx = project.subscribe_events();
    let mut brx = builds.subscribe();
    if project.is_deleted() { return; }
    if socket.send(Message::Text(role_changed(role).into())).await.is_err() { return; }
    let mut recheck_tick = recheck_timer();
    let (presence, mut crx) = PresenceConnection::join(app.presence.clone(), access.project_id.clone(), &user, collab);
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
            changed = access.changes.recv(), if access.open => {
                if !access.concerns(changed) { continue; }
                match access.recheck_or_close(&mut socket, true).await {
                    Rechecked::Closed => break,
                    Rechecked::Role(r) if r != role => { role = r; role_changed(r) }
                    Rechecked::Role(_) | Rechecked::Keep => continue,
                }
            },
            _ = recheck_tick.tick() => match access.recheck_or_close(&mut socket, true).await {
                Rechecked::Closed => break,
                Rechecked::Role(r) if r != role => { role = r; role_changed(r) }
                Rechecked::Role(_) | Rechecked::Keep => continue,
            },
        };
        if socket.send(Message::Text(frame.into())).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod recheck_tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn recheck_reports_the_role_then_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.build.sandbox = "none".into();
        config.limits.enabled = false;
        let app = AppState::new(dir.path(), &config).await.unwrap();
        let user = app.store.create_user("member@example.com", "Member", "supersecret", false).unwrap();
        let other = app.store.create_user("other@example.com", "Other", "supersecret", false).unwrap();
        app.store.set_member("paper", &user.id, Role::Editor).unwrap();
        let first = Secret(app.store.create_session(&user.id, 1).unwrap());
        let second = Secret(app.store.create_session(&user.id, 1).unwrap());
        let others = Secret(app.store.create_session(&other.id, 1).unwrap());

        assert_eq!(recheck(&app, &first, &user.id, "paper").await, Some(Recheck::Role(Role::Editor)));
        app.store.set_member("paper", &user.id, Role::Viewer).unwrap();
        assert_eq!(recheck(&app, &first, &user.id, "paper").await, Some(Recheck::Role(Role::Viewer)));

        // A session that belongs to someone else never passes as this user's.
        assert_eq!(recheck(&app, &others, &user.id, "paper").await, Some(Recheck::SignedOut));
        app.store.delete_session(&first.0).unwrap();
        assert_eq!(recheck(&app, &first, &user.id, "paper").await, Some(Recheck::SignedOut));
        assert_eq!(recheck(&app, &second, &user.id, "paper").await, Some(Recheck::Role(Role::Viewer)), "other sessions stay");

        app.store.remove_member("paper", &user.id).unwrap();
        assert_eq!(recheck(&app, &second, &user.id, "paper").await, Some(Recheck::Removed));
        assert_eq!(recheck(&app, &first, &user.id, "paper").await, Some(Recheck::Removed), "removal wins over a signed-out session");
    }
}
