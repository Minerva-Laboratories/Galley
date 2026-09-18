use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::CurrentUser;
use crate::auth::Role;
use crate::registry::ProjectMeta;

pub async fn health(State(app): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": crate::VERSION,
        "sandbox": app.builder.sandbox.kind,
        "engine": app.builder.engine_path().await.map(|p| p.display().to_string()),
    }))
}

/// A project plus the caller's role on it.
#[derive(Serialize)]
pub struct ProjectView {
    #[serde(flatten)]
    meta: ProjectMeta,
    role: Role,
}

pub async fn list_projects(
    State(app): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<ProjectView>>, AppError> {
    let store = app.store.clone();
    let uid = user.id.clone();
    let memberships = tokio::task::spawn_blocking(move || store.project_ids_for(&uid))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let mut out = Vec::new();
    for (id, role) in memberships {
        match app.registry.meta(&id) {
            Ok(meta) => out.push(ProjectView { meta, role }),
            Err(_) => tracing::warn!(project = %id, "membership points at a missing project"),
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.meta.updated_at));
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateProject {
    name: String,
    /// A bundled template id. The default is the blank article.
    #[serde(default)]
    template: Option<String>,
}

/// POST /api/projects/{id}/grammar. It checks one file's prose with LanguageTool, on demand.
/// It is never part of a build. A grammar server that is down must not make a document fail to compile.
pub async fn check_grammar(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<GrammarRequest>,
) -> Result<Json<Vec<galley_build::log::Diagnostic>>, AppError> {
    app.require(&user, &id, Role::can_view, "checking grammar").await?;
    let base = crate::grammar::endpoint(&app.grammar.languagetool).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let project = app.registry.open(&id).await?;
    let text = project.doc(&body.path).await?.text().await;
    let language = app.grammar.language.clone();
    let found = crate::grammar::check_file(&base, &body.path, &text, &language)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(found))
}

#[derive(Deserialize)]
pub struct GrammarRequest {
    path: String,
}

/// GET /api/templates. It lists what "New project" can start from.
pub async fn list_templates() -> Json<&'static [crate::templates::Template]> {
    Json(crate::templates::BUNDLED)
}

pub async fn create_project(
    State(app): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateProject>,
) -> Result<(StatusCode, Json<ProjectView>), AppError> {
    if user.is_guest {
        return Err(AppError::Forbidden("Guests from a share link can't create projects. Sign up for an account.".into()));
    }
    if body.name.trim().len() > 120 {
        return Err(AppError::BadRequest("Project names are limited to 120 characters.".into()));
    }
    let template = match body.template.as_deref().filter(|t| !t.is_empty()) {
        Some(id) => crate::templates::find(id).ok_or_else(|| AppError::BadRequest(format!("There is no template called {id}.")))?,
        None => crate::templates::default(),
    };
    let meta = app.registry.create_from(&body.name, template).await?;
    let store = app.store.clone();
    let (pid, uid) = (meta.id.clone(), user.id.clone());
    tokio::task::spawn_blocking(move || store.set_member(&pid, &uid, Role::Admin))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    app.store.audit(Some(&meta.id), Some(&user), "project.create", Some(&meta.name));
    Ok((StatusCode::CREATED, Json(ProjectView { meta, role: Role::Admin })))
}

pub async fn get_project(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<ProjectView>, AppError> {
    let role = app.require(&user, &id, Role::can_view, "opening this project").await?;
    Ok(Json(ProjectView { meta: app.registry.meta(&id)?, role }))
}

#[derive(Deserialize, Default)]
pub struct SettingsPatch {
    /// A date as YYYY-MM-DD. An empty string clears it.
    deadline: Option<String>,
    /// Empty string clears it.
    venue: Option<String>,
    /// Replaces the whole map when present.
    budgets: Option<std::collections::BTreeMap<String, u32>>,
    lint_disabled: Option<Vec<String>>,
    figure_cache: Option<bool>,
    literature: Option<bool>,
    /// A .tex file in the project that builds should compile.
    main_file: Option<String>,
}

/// PATCH /api/projects/{id}/settings. It sets the deadline, venue, word budgets and lint toggles
/// (SPEC §13.7).
pub async fn update_settings(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SettingsPatch>,
) -> Result<Json<ProjectView>, AppError> {
    let role = app.require(&user, &id, Role::can_edit, "changing project settings").await?;
    let deadline = match body.deadline.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(d) => {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map_err(|_| AppError::BadRequest("Enter the deadline as YYYY-MM-DD.".into()))?;
            Some(Some(d.to_string()))
        }
    };
    let venue = body.venue.map(|v| v.trim().chars().take(60).collect::<String>()).map(|v| if v.is_empty() { None } else { Some(v) });
    let lint_disabled = body.lint_disabled.map(|l| {
        l.into_iter().filter(|r| galley_build::lint::RULES.contains(&r.as_str())).collect::<Vec<_>>()
    });
    let main_file = match body.main_file.as_deref() {
        None => None,
        Some(p) => {
            let rel = galley_sync::paths::clean_rel_path(p).filter(|r| r.ends_with(".tex"));
            let project = app.registry.open(&id).await?;
            match rel {
                Some(r) if project.exists(&r) => Some(r),
                _ => return Err(AppError::BadRequest(format!("{p} is not a .tex file in this project."))),
            }
        }
    };
    if let (Some(m), Ok(builds)) = (&main_file, app.builds(&id).await) {
        builds.set_main_file(m).await;
    }
    let meta = app.registry.update_meta(&id, |m| {
        if let Some(f) = main_file {
            m.main_file = f;
        }
        if let Some(d) = deadline {
            m.deadline = d;
        }
        if let Some(v) = venue {
            m.venue = v;
        }
        if let Some(b) = body.budgets {
            m.budgets = b.into_iter().filter(|(_, n)| *n > 0).collect();
        }
        if let Some(l) = lint_disabled {
            m.lint_disabled = l;
        }
        if let Some(f) = body.figure_cache {
            m.figure_cache = f;
        }
        if let Some(l) = body.literature {
            m.literature = l;
        }
    })?;
    app.store.audit(Some(&id), Some(&user), "project.settings", None);
    Ok(Json(ProjectView { meta, role }))
}

pub async fn list_files(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<galley_sync::FileEntry>>, AppError> {
    app.require(&user, &id, Role::can_view, "seeing the files").await?;
    let project = app.registry.open(&id).await?;
    Ok(Json(project.list_files()?))
}

#[derive(Deserialize)]
pub struct CreateFile {
    path: String,
}

pub async fn create_file(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateFile>,
) -> Result<(StatusCode, Json<galley_sync::FileEntry>), AppError> {
    app.require(&user, &id, Role::can_edit, "adding files").await?;
    let project = app.registry.open(&id).await?;
    let entry = project.create_file(&body.path).await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    50
}

pub async fn history(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<galley_history::CommitInfo>>, AppError> {
    app.require(&user, &id, Role::can_view, "viewing history").await?;
    let project = app.registry.open(&id).await?;
    Ok(Json(project.history(q.limit.clamp(1, 500)).await?))
}

/// Land pending edits in git now. Builds call this before they compile. The tests call it too.
#[derive(Deserialize, Default)]
pub struct FlushBody {
    /// Names the commit for a deliberate operation, such as a label rename, instead of `edit: ...`.
    #[serde(default)]
    message: Option<String>,
}

pub async fn flush(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    body: Option<Json<FlushBody>>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "saving").await?;
    let project = app.registry.open(&id).await?;
    let message = body
        .and_then(|Json(b)| b.message)
        .map(|m| m.lines().next().unwrap_or("").trim().chars().take(200).collect::<String>())
        .filter(|m| !m.is_empty());
    let commit = project.flush_with(message).await?;
    if commit.is_some() {
        app.registry.touch(&id);
    }
    Ok(Json(json!({ "commit": commit })))
}
