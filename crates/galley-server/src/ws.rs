//! WebSockets. There is one y-protocol socket for each client and document, and one JSON event
//! socket for each project. Both authenticate from the session cookie and enforce the caller's
//! role. The document socket is wire-compatible with `y-websocket`. A user who cannot edit
//! connects read-only.

use std::sync::Arc;

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
    let mut announced: HashSet<yrs::block::ClientID> = HashSet::new();
    let mut edited_any = false;

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
                    Message::Binary(bytes) => match doc.handle(&bytes, may_edit).await {
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
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
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
    Ok(ws.on_upgrade(move |socket| events_session(socket, project, builds, collab)))
}

/// One socket carries sync events for commits and files, build events, and collaboration events.
/// Every frame is a JSON object with a `type` field.
async fn events_session(
    mut socket: WebSocket,
    project: Arc<ProjectSync>,
    builds: Arc<galley_build::ProjectBuilds>,
    collab: tokio::sync::broadcast::Sender<String>,
) {
    let mut rx = project.subscribe_events();
    let mut brx = builds.subscribe();
    let mut crx = collab.subscribe();
    loop {
        let frame = tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => continue,
            },
            ev = rx.recv() => match ev {
                Ok(ev) => serde_json::to_string(&ev).unwrap_or_default(),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
            ev = brx.recv() => match ev {
                Ok(ev) => serde_json::to_string(&ev).unwrap_or_default(),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
            ev = crx.recv() => match ev {
                Ok(text) => text,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
        };
        if socket.send(Message::Text(frame.into())).await.is_err() {
            break;
        }
    }
}
