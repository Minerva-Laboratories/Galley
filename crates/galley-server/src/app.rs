use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRef;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use galley_build::{Builder, Hints, ProjectBuilds, Sandbox, SandboxKind};
use serde::Serialize;
use serde_json::json;
use tokio::sync::{broadcast, RwLock};
use tower_http::trace::TraceLayer;

use crate::auth::{current, Role};
use crate::config::Config;
use crate::db::Db;
use crate::registry::{Registry, RegistryError};
use crate::store::{Store, StoreError, User};
use crate::{
    assets, auth_routes, bib_routes, build_routes, collab_routes, file_routes, history_routes, mcp_routes, pack_routes, routes, share_routes,
    token_routes, ws,
};

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<Registry>,
    pub builder: Arc<Builder>,
    pub store: Store,
    /// Cookies get the Secure flag when the server is reachable over HTTPS.
    pub secure: bool,
    /// Whether anyone may create an account. If not, only the first-run admin and invitees can.
    pub public_signup: bool,
    /// Address sent to the catalogues' polite pools, when the operator set one.
    pub contact_email: Option<String>,
    /// Grammar checking, off unless the operator points at a LanguageTool server.
    pub grammar: crate::config::GrammarConfig,
    builds: Arc<RwLock<HashMap<String, Arc<ProjectBuilds>>>>,
    collab: Arc<RwLock<HashMap<String, broadcast::Sender<String>>>>,
}

impl FromRef<AppState> for Store {
    fn from_ref(state: &AppState) -> Store {
        state.store.clone()
    }
}

/// Live collaboration events, pushed onto the project event socket so peers update without a poll.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollabEvent {
    CommentAdded { comment: crate::store::Comment },
    CommentResolved { id: String, resolved: bool },
    SuggestionAdded { suggestion: crate::store::Suggestion },
    SuggestionUpdated { id: String, status: String },
    MembersChanged,
    /// The open role-change requests changed. Admins re-fetch the governance state.
    RequestsChanged,
    /// A checkpoint was created. Peers re-fetch the checkpoint list.
    CheckpointsChanged,
}

impl AppState {
    pub async fn new(data_dir: &Path, config: &Config) -> std::io::Result<AppState> {
        let b = &config.build;
        let sandbox = Sandbox::detect(&b.sandbox, &b.docker_image, b.memory_mb, b.cpus).await;
        let public = !config.server.domain.is_empty();
        match sandbox.kind {
            SandboxKind::None => {
                // Security rule. An unsandboxed build must never run in public mode, that is,
                // with a domain set. The one exception is an operator who has accepted the risk
                // for a trusted test group.
                if public && !config.build.allow_unsandboxed {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "refusing to start: server.domain is set (public mode) but no sandbox is \
                         available. Install bubblewrap (or Docker), or unset server.domain to run \
                         on localhost only. To run publicly without a sandbox anyway (only for a \
                         group you trust), set build.allow_unsandboxed = true.",
                    ));
                }
                if public {
                    tracing::warn!("sandbox: none in PUBLIC mode via allow_unsandboxed. Compiles run unconfined; only expose this to people you trust.");
                    eprintln!("\x1b[31mWarning: running publicly with NO sandbox (allow_unsandboxed). Compiles run unconfined. Only for a trusted group.\x1b[0m");
                } else {
                    tracing::warn!("sandbox: none. Compiles run unconfined; do not expose this server beyond localhost.");
                    eprintln!("\x1b[31mWarning: no sandbox available (bubblewrap and docker both unusable). Compiles run unconfined. Fine for local development; never expose this server.\x1b[0m");
                }
            }
            kind => tracing::info!(%kind, "sandbox ready"),
        }
        let hints = Hints::bundled().map_err(std::io::Error::other)?;
        let builder = Arc::new(Builder::new(
            sandbox,
            hints,
            Duration::from_secs(b.timeout_s),
            data_dir,
            Some(b.tectonic_path.clone()).filter(|s| !s.is_empty()),
        ));
        match builder.engine_path().await {
            Some(p) => tracing::info!(path = %p.display(), "tectonic found"),
            None => tracing::info!("tectonic not installed yet; it is downloaded on the first build"),
        }
        let db = Db::open(&data_dir.join("galley.db")).map_err(std::io::Error::other)?;
        Ok(AppState {
            registry: Arc::new(Registry::new(data_dir, config.sync_config())?),
            builder,
            store: Store::new(db),
            secure: !config.server.domain.is_empty(),
            public_signup: config.server.public_signup,
            contact_email: (!config.server.contact_email.is_empty()).then(|| config.server.contact_email.clone()),
            grammar: config.grammar.clone(),
            builds: Arc::new(RwLock::new(HashMap::new())),
            collab: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// The role a user has on a project, or `None` if they have no access.
    pub async fn access(&self, user: &User, project_id: &str) -> Result<Option<Role>, AppError> {
        let store = self.store.clone();
        let id = project_id.to_string();
        let uid = user.id.clone();
        let role = tokio::task::spawn_blocking(move || store.role_of(&id, &uid))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(role)
    }

    /// Require at least the `check` capability. Return the caller's role, or the right error.
    pub async fn require(
        &self,
        user: &User,
        project_id: &str,
        check: fn(Role) -> bool,
        what: &str,
    ) -> Result<Role, AppError> {
        match self.access(user, project_id).await? {
            Some(role) if check(role) => Ok(role),
            Some(_) => Err(AppError::Forbidden(format!("Your role does not allow {what}."))),
            None => Err(AppError::Registry(RegistryError::NotFound(project_id.to_string()))),
        }
    }

    pub async fn collab_channel(&self, id: &str) -> broadcast::Sender<String> {
        if let Some(tx) = self.collab.read().await.get(id) {
            return tx.clone();
        }
        let mut map = self.collab.write().await;
        map.entry(id.to_string())
            .or_insert_with(|| broadcast::channel(128).0)
            .clone()
    }

    pub async fn emit(&self, id: &str, event: CollabEvent) {
        let tx = self.collab_channel(id).await;
        if let Ok(text) = serde_json::to_string(&event) {
            let _ = tx.send(text);
        }
    }

    /// The build queue for a project, created on first use with a flush-before-build hook.
    pub async fn builds(&self, id: &str) -> Result<Arc<ProjectBuilds>, AppError> {
        if let Some(b) = self.builds.read().await.get(id) {
            return Ok(Arc::clone(b));
        }
        let project = self.registry.open(id).await?;
        let meta = self.registry.meta(id)?;
        let mut map = self.builds.write().await;
        if let Some(b) = map.get(id) {
            return Ok(Arc::clone(b));
        }
        let flush_project = Arc::clone(&project);
        let before: galley_build::runner::BeforeBuild = Arc::new(move || {
            let p = Arc::clone(&flush_project);
            Box::pin(async move {
                if let Err(e) = p.flush_now().await {
                    tracing::warn!(error = %e, "flush before build failed; compiling the last written tree");
                }
            })
        });
        let builds = ProjectBuilds::new(Arc::clone(&self.builder), project.workdir(), &meta.main_file, Some(before));
        map.insert(id.to_string(), Arc::clone(&builds));
        Ok(builds)
    }
}

/// Every error a route can return, mapped to a status and a sentence-case message with a next step.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Registry(#[from] RegistryError),
    #[error("{0}")]
    Sync(#[from] galley_sync::Error),
    #[error("{0}")]
    Build(#[from] galley_build::Error),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Registry(RegistryError::NotFound(id)) => (
                StatusCode::NOT_FOUND,
                format!("No project named {id}. Check the link, or create it from the projects page."),
            ),
            AppError::Registry(RegistryError::BadName) => (
                StatusCode::BAD_REQUEST,
                "Project names need at least one letter or digit.".to_string(),
            ),
            AppError::Sync(galley_sync::Error::InvalidPath(p)) => (
                StatusCode::BAD_REQUEST,
                format!("{p} is not a valid project path. Use a relative path without .. or reserved folders."),
            ),
            AppError::Sync(galley_sync::Error::NotFound(p)) => (
                StatusCode::NOT_FOUND,
                format!("{p} is not in this project any more. Reload the file list."),
            ),
            AppError::Sync(galley_sync::Error::NotText(p)) => (
                StatusCode::BAD_REQUEST,
                format!("{p} is not a text file. Only text files open in the editor."),
            ),
            AppError::Sync(galley_sync::Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                (StatusCode::CONFLICT, format!("{e}. Pick another name."))
            }
            AppError::Store(StoreError::Invalid(m)) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Store(StoreError::Conflict(m)) => (StatusCode::CONFLICT, m.clone()),
            AppError::Store(StoreError::NotFound(m)) => (StatusCode::NOT_FOUND, m.clone()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m.clone()),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m.clone()),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            other => {
                tracing::error!(error = %other, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong on the server. The details are in the server log.".to_string(),
                )
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/health", get(routes::health))
        .route("/api/auth/me", get(auth_routes::me))
        .route("/api/auth/signup", post(auth_routes::signup))
        .route("/api/auth/login", post(auth_routes::login))
        .route("/api/auth/logout", post(auth_routes::logout))
        .route("/api/auth/landing", post(auth_routes::landing))
        .route("/api/auth/tokens", get(token_routes::list).post(token_routes::create))
        .route("/api/auth/tokens/{tid}", axum::routing::delete(token_routes::revoke))
        .route("/api/projects/{id}/agent-runs", get(mcp_routes::runs))
        .route("/api/share/{token}", get(share_routes::preview))
        .route("/api/projects", get(routes::list_projects).post(routes::create_project))
        .route("/api/projects/{id}", get(routes::get_project))
        .route("/api/projects/{id}/settings", axum::routing::patch(routes::update_settings))
        .route("/api/venues", get(pack_routes::list_venues))
        .route("/api/templates", get(routes::list_templates))
        .route("/api/projects/{id}/grammar", post(routes::check_grammar))
        .route("/api/projects/{id}/pack", post(pack_routes::pack))
        .route("/api/projects/{id}/pack/download", get(pack_routes::download))
        .route("/api/projects/{id}/submitted", post(pack_routes::freeze))
        .route("/api/projects/{id}/files", get(routes::list_files).post(routes::create_file).delete(file_routes::delete))
        .route("/api/projects/{id}/files/rename", post(file_routes::rename))
        .route(
            "/api/projects/{id}/files/content",
            get(file_routes::download)
                .put(file_routes::upload)
                .layer(axum::extract::DefaultBodyLimit::max(file_routes::MAX_UPLOAD)),
        )
        .route("/api/projects/{id}/bib", get(bib_routes::library).post(bib_routes::add))
        .route(
            "/api/projects/{id}/bib/{key}",
            axum::routing::patch(bib_routes::edit).delete(bib_routes::delete),
        )
        .route("/api/projects/{id}/bib/{key}/merge", post(bib_routes::merge))
        .route("/api/projects/{id}/bib/{key}/lookup", post(bib_routes::suggest))
        .route("/api/projects/{id}/citations", get(bib_routes::citations))
        .route("/api/projects/{id}/citations/candidates", post(bib_routes::candidates))
        .route("/api/projects/{id}/history", get(routes::history))
        .route("/api/projects/{id}/history/file", get(history_routes::file_at))
        .route("/api/projects/{id}/history/diff", get(history_routes::diff))
        .route(
            "/api/projects/{id}/checkpoints",
            get(history_routes::list_checkpoints).post(history_routes::create_checkpoint),
        )
        .route("/api/projects/{id}/restore", post(history_routes::restore))
        .route("/api/projects/{id}/latexdiff", post(history_routes::latexdiff))
        .route("/api/projects/{id}/latexdiff/pdf", get(history_routes::latexdiff_pdf))
        .route("/api/projects/{id}/flush", post(routes::flush))
        .route("/api/projects/{id}/build", get(build_routes::status).post(build_routes::request))
        .route("/api/projects/{id}/build/pdf", get(build_routes::pdf))
        .route("/api/projects/{id}/figures/clear", post(build_routes::clear_figures))
        .route("/api/projects/{id}/build/log", get(build_routes::log))
        .route("/api/projects/{id}/build/synctex/forward", get(build_routes::synctex_forward))
        .route("/api/projects/{id}/build/synctex/inverse", get(build_routes::synctex_inverse))
        .route("/api/projects/{id}/members", get(share_routes::members).post(share_routes::invite))
        .route(
            "/api/projects/{id}/members/{uid}",
            axum::routing::put(share_routes::set_role).delete(share_routes::remove_member),
        )
        .route("/api/projects/{id}/shares", get(share_routes::list_shares).post(share_routes::create_share))
        .route("/api/projects/{id}/shares/{sid}", axum::routing::delete(share_routes::revoke_share))
        .route(
            "/api/projects/{id}/governance",
            get(share_routes::governance_state).put(share_routes::set_governance),
        )
        .route("/api/projects/{id}/requests/{rid}/vote", post(share_routes::vote))
        .route("/api/projects/{id}/requests/{rid}", axum::routing::delete(share_routes::cancel_request))
        .route("/api/projects/{id}/comments", get(collab_routes::comments).post(collab_routes::add_comment))
        .route("/api/projects/{id}/comments/{cid}/resolve", post(collab_routes::resolve_comment))
        .route("/api/projects/{id}/suggestions", get(collab_routes::suggestions).post(collab_routes::add_suggestion))
        .route("/api/projects/{id}/suggestions/{sid}/status", post(collab_routes::set_suggestion_status))
        .layer(middleware::from_fn(current::csrf_guard));

    Router::new()
        .merge(api)
        .route("/mcp/{id}", get(mcp_routes::info).post(mcp_routes::rpc))
        .route("/ws/{id}/events", get(ws::events))
        .route("/ws/{id}/doc/{*path}", get(ws::doc))
        .fallback(assets::serve)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub struct ServeOptions {
    pub bind: SocketAddr,
    pub dev: bool,
}

pub async fn serve(state: AppState, opts: ServeOptions) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "galley listening");
    println!("Galley is running at http://{addr}");
    if opts.dev {
        println!("Dev mode: start `cd web && npm run dev` and open the Vite URL; it proxies here.");
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
}
