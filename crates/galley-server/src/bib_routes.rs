//! The bibliography as data for the editor. It holds the co-citation graph that the project already
//! contains. It also holds candidates from OpenAlex when the project has turned catalogue lookups on.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::app::{AppError, AppState};
use crate::auth::current::CurrentUser;
use crate::auth::Role;

#[derive(Serialize)]
pub struct GraphView {
    #[serde(flatten)]
    graph: galley_index::graph::Graph,
    /// Whether catalogue lookups are on, so the UI can offer them or explain that they are off.
    literature: bool,
}

/// GET /api/projects/{id}/citations. It makes no network calls and is always available.
pub async fn citations(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<GraphView>, AppError> {
    app.require(&user, &id, Role::can_view, "seeing the bibliography").await?;
    let meta = app.registry.meta(&id)?;
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let dir = project.workdir().to_path_buf();
    let graph = tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &meta.main_file);
        galley_index::graph::citation_graph(&paper)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    let literature = app.registry.meta(&id)?.literature;
    Ok(Json(GraphView { graph, literature }))
}

/// POST /api/projects/{id}/citations/candidates. It asks OpenAlex what the references cite.
/// The call takes seconds and is opt-in. The client shows progress and can leave it out.
pub async fn candidates(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "looking work up in catalogues").await?;
    let meta = app.registry.meta(&id)?;
    if !meta.literature {
        return Err(AppError::BadRequest(
            "Catalogue lookups are off for this project. Turn them on in project settings; they send \
             citation keys, DOIs and titles to Crossref and OpenAlex, never the document."
                .into(),
        ));
    }
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let dir = project.workdir().to_path_buf();
    let main_file = meta.main_file.clone();
    let paper = tokio::task::spawn_blocking(move || galley_index::scan(&dir, &main_file))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let client =
        galley_lit::Client::new(&project.workdir().join(".galley").join("lit"), app.contact_email.clone());
    let (candidates, from, problems) = galley_lit::coverage(&client, &paper, 12).await;
    app.store.audit(Some(&id), Some(&user), "literature.coverage", Some(&format!("{} candidates", candidates.len())));
    Ok(Json(json!({
        "candidates": candidates,
        "from": from,
        "notes": problems.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------------------------
// The bibliography manager. It holds entries as data, and the edits that fix what the audit found.
//
// Every write goes through the file's live document, so collaborators see an ordinary edit. Each
// write lands as a named commit.

use axum::http::StatusCode;
use galley_index::bib::Entry;
use serde::Deserialize;

#[derive(Serialize)]
pub struct ManagedEntry {
    key: String,
    kind: String,
    /// Every field the file holds, in its own order.
    fields: Vec<(String, String)>,
    file: String,
    line: u32,
    cited: usize,
    sections: Vec<String>,
    issues: Vec<String>,
    duplicate_of: Option<String>,
}

#[derive(Serialize)]
pub struct Library {
    entries: Vec<ManagedEntry>,
    /// The `.bib` files that the document names. New entries go into the first one.
    files: Vec<String>,
    missing: Vec<String>,
    literature: bool,
}

/// Read the project's bibliography as editable data.
pub async fn library(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Library>, AppError> {
    app.require(&user, &id, Role::can_view, "reading the bibliography").await?;
    let meta = app.registry.meta(&id)?;
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let dir = project.workdir().to_path_buf();
    let (graph, entries, files) = tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &meta.main_file);
        let graph = galley_index::graph::citation_graph(&paper);
        let mut files: Vec<String> = Vec::new();
        let mut entries = Vec::new();
        for e in &paper.bib {
            if !files.contains(&e.file) {
                files.push(e.file.clone());
            }
            let text = std::fs::read_to_string(dir.join(&e.file)).unwrap_or_default();
            let fields = galley_index::bib::entry_span(&text, &e.key)
                .map(|(s, t)| galley_index::bib::fields_of(&text[s..t]))
                .unwrap_or_default();
            entries.push((e.key.clone(), e.kind.clone(), fields, e.file.clone(), e.line));
        }
        (graph, entries, files)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let entries = entries
        .into_iter()
        .map(|(key, kind, fields, file, line)| {
            let node = graph.nodes.iter().find(|n| n.key == key);
            ManagedEntry {
                key,
                kind,
                fields,
                file,
                line,
                cited: node.map(|n| n.cited).unwrap_or(0),
                sections: node.map(|n| n.sections.clone()).unwrap_or_default(),
                issues: node.map(|n| n.issues.clone()).unwrap_or_default(),
                duplicate_of: node.and_then(|n| n.duplicate_of.clone()),
            }
        })
        .collect();
    Ok(Json(Library { entries, files, missing: graph.missing, literature: app.registry.meta(&id)?.literature }))
}

#[derive(Deserialize)]
pub struct NewEntry {
    /// A DOI, an arXiv id, or either one's URL.
    #[serde(default)]
    identifier: String,
    /// Pasted BibTeX. It is an alternative to looking an identifier up.
    #[serde(default)]
    bibtex: String,
    /// Which `.bib` file to write to. The default is the first one.
    #[serde(default)]
    file: Option<String>,
}

/// Add an entry from a pasted identifier or from pasted BibTeX. A pasted identifier makes one
/// catalogue call, because the author asked for it. Pasted BibTeX makes no network call.
pub async fn add(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewEntry>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    app.require(&user, &id, Role::can_edit, "adding a bibliography entry").await?;
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let file = bib_file(&app, &id, body.file.clone()).await?;
    let doc = project.doc(&file).await?;
    let text = doc.text().await;

    let entry = if !body.bibtex.trim().is_empty() {
        let mut e = galley_index::bib::from_bibtex(&body.bibtex)
            .ok_or_else(|| AppError::BadRequest("That does not look like a BibTeX entry.".into()))?;
        if galley_index::bib::entry_span(&text, &e.key).is_some() {
            e.key = galley_index::bib::suggest_key(&text, e.field("author"), e.field("year"), e.field("title"));
        }
        e
    } else {
        let client = galley_lit::Client::new(&project.workdir().join(".galley").join("lit"), app.contact_email.clone());
        let found = galley_lit::lookup(&client, &body.identifier).await.map_err(|e| match e {
            galley_lit::Error::NotFound(what) => {
                AppError::NotFound(format!("{what} has no record of {}. Check the identifier, or paste the BibTeX.", body.identifier.trim()))
            }
            other => AppError::BadRequest(other.to_string()),
        })?;
        let key = galley_index::bib::suggest_key(
            &text,
            found.field("author"),
            found.field("year"),
            found.field("title"),
        );
        Entry { kind: found.kind, key, fields: found.fields }
    };

    let updated = galley_index::bib::upsert(&text, &entry);
    doc.reseed(&updated).await;
    project.set_last_editor(&user.name);
    project.flush_with(Some(format!("bib: add {}", entry.key))).await?;
    app.store.audit(Some(&id), Some(&user), "bib.add", Some(&entry.key));
    Ok((StatusCode::CREATED, Json(json!({ "key": entry.key, "file": file, "fields": entry.fields, "kind": entry.kind }))))
}

#[derive(Deserialize)]
pub struct EditEntry {
    /// Fields to set. An empty value removes the field.
    fields: BTreeMap<String, String>,
    #[serde(default)]
    kind: Option<String>,
}

/// Change an entry's fields, keeping every field the editor did not touch.
pub async fn edit(
    State(app): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<EditEntry>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "editing a bibliography entry").await?;
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let (file, mut entry, text, doc) = find_entry(&app, &project, &id, &key).await?;

    for (name, value) in body.fields {
        entry.fields.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
        if !value.trim().is_empty() {
            entry.fields.push((name, value.trim().to_string()));
        }
    }
    if let Some(kind) = body.kind.filter(|k| k.chars().all(|c| c.is_ascii_alphabetic()) && !k.is_empty()) {
        entry.kind = kind.to_ascii_lowercase();
    }
    let updated = galley_index::bib::upsert(&text, &entry);
    doc.reseed(&updated).await;
    project.set_last_editor(&user.name);
    project.flush_with(Some(format!("bib: edit {key}"))).await?;
    app.store.audit(Some(&id), Some(&user), "bib.edit", Some(&key));
    Ok(Json(json!({ "key": entry.key, "file": file, "fields": entry.fields, "kind": entry.kind })))
}

/// Remove an entry. Refused while the document still cites it, unless `force` says otherwise.
pub async fn delete(
    State(app): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<ForceQuery>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "deleting a bibliography entry").await?;
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let meta = app.registry.meta(&id)?;
    let dir = project.workdir().to_path_buf();
    let main_file = meta.main_file.clone();
    let cited = {
        let (dir, key) = (dir.clone(), key.clone());
        tokio::task::spawn_blocking(move || {
            let paper = galley_index::scan(&dir, &main_file);
            paper.cite_counts().get(key.as_str()).copied().unwrap_or(0)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    };
    if cited > 0 && !q.force {
        return Err(AppError::BadRequest(format!(
            "{key} is cited in {cited} section(s). Remove the citations first, or delete it anyway."
        )));
    }
    let (file, _, text, doc) = find_entry(&app, &project, &id, &key).await?;
    let updated = galley_index::bib::remove(&text, &key)
        .ok_or_else(|| AppError::NotFound(format!("{key} is not in {file} any more.")))?;
    doc.reseed(&updated).await;
    project.set_last_editor(&user.name);
    project.flush_with(Some(format!("bib: delete {key}"))).await?;
    app.store.audit(Some(&id), Some(&user), "bib.delete", Some(&key));
    Ok(Json(json!({ "ok": true, "was_cited": cited })))
}

#[derive(Deserialize, Default)]
pub struct ForceQuery {
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
pub struct MergeInto {
    /// The key to keep.
    into: String,
}

/// Merge a duplicate. The server points every `\cite` of `key` at `into`, then removes `key`.
pub async fn merge(
    State(app): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<MergeInto>,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "merging bibliography entries").await?;
    if body.into == key {
        return Err(AppError::BadRequest("An entry cannot be merged into itself.".into()));
    }
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let files = project.list_files()?;

    // Point the citations at the surviving key, file by file, through the live documents.
    let mut moved = 0usize;
    for f in files.iter().filter(|f| f.path.ends_with(".tex")) {
        let doc = project.doc(&f.path).await?;
        let text = doc.text().await;
        let (updated, n) = galley_index::bib::rename_cite(&text, &key, &body.into);
        if n > 0 {
            doc.reseed(&updated).await;
            moved += n;
        }
    }
    let (file, _, text, doc) = find_entry(&app, &project, &id, &key).await?;
    if let Some(updated) = galley_index::bib::remove(&text, &key) {
        doc.reseed(&updated).await;
    }
    project.set_last_editor(&user.name);
    project.flush_with(Some(format!("bib: merge {key} → {}", body.into))).await?;
    app.store.audit(Some(&id), Some(&user), "bib.merge", Some(&format!("{key} → {}", body.into)));
    Ok(Json(json!({ "ok": true, "moved": moved, "removed_from": file })))
}

/// The `.bib` file to write to. It is the file that the caller asked for, or the file that the
/// bibliography already uses, or `refs.bib` when there is no such file yet.
async fn bib_file(app: &AppState, id: &str, asked: Option<String>) -> Result<String, AppError> {
    if let Some(f) = asked {
        let rel = galley_sync::paths::clean_rel_path(&f).filter(|r| r.ends_with(".bib"));
        return rel.ok_or_else(|| AppError::BadRequest(format!("{f} is not a .bib file in this project.")));
    }
    let meta = app.registry.meta(id)?;
    let project = app.registry.open(id).await?;
    let dir = project.workdir().to_path_buf();
    let found = tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &meta.main_file);
        paper.bib.first().map(|b| b.file.clone())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    if let Some(f) = found {
        return Ok(f);
    }
    // There are no entries yet. Use a .bib file that exists, or start refs.bib.
    let files = project.list_files()?;
    Ok(files
        .iter()
        .find(|f| f.path.ends_with(".bib"))
        .map(|f| f.path.clone())
        .unwrap_or_else(|| "refs.bib".to_string()))
}

/// Locate an entry, and return its file, its parsed form, the file's text and its document.
async fn find_entry(
    app: &AppState,
    project: &std::sync::Arc<galley_sync::ProjectSync>,
    id: &str,
    key: &str,
) -> Result<(String, Entry, String, std::sync::Arc<galley_sync::DocHandle>), AppError> {
    let meta = app.registry.meta(id)?;
    let dir = project.workdir().to_path_buf();
    let main_file = meta.main_file.clone();
    let (key2, dir2) = (key.to_string(), dir.clone());
    let found = tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir2, &main_file);
        paper.bib.iter().find(|b| b.key == key2).map(|b| (b.file.clone(), b.kind.clone()))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    let (file, kind) = found.ok_or_else(|| AppError::NotFound(format!("No entry named {key} in this bibliography.")))?;
    let doc = project.doc(&file).await?;
    let text = doc.text().await;
    let (start, end) = galley_index::bib::entry_span(&text, key)
        .ok_or_else(|| AppError::NotFound(format!("{key} is not in {file} any more.")))?;
    let fields = galley_index::bib::fields_of(&text[start..end]);
    Ok((file, Entry { kind, key: key.to_string(), fields }, text, doc))
}

/// POST /api/projects/{id}/bib/{key}/lookup. It asks the catalogues what this entry should say.
/// The route returns suggestions only. The server writes nothing until the author applies them.
pub async fn suggest(
    State(app): State<AppState>,
    Path((id, key)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Value>, AppError> {
    app.require(&user, &id, Role::can_edit, "looking an entry up").await?;
    let project = app.registry.open(&id).await?;
    let _ = project.flush_now().await;
    let (_, entry, _, _) = find_entry(&app, &project, &id, &key).await?;
    let client = galley_lit::Client::new(&project.workdir().join(".galley").join("lit"), app.contact_email.clone());

    // A DOI says what the work is. An arXiv id may have become a publication since. If the entry
    // has neither, the title is the only thing to search on.
    let identifier = entry
        .field("doi")
        .map(str::to_string)
        .or_else(|| entry.field("eprint").map(str::to_string))
        .unwrap_or_default();
    let found = if identifier.is_empty() {
        None
    } else {
        match galley_lit::lookup(&client, &identifier).await {
            Ok(l) => Some(l),
            Err(galley_lit::Error::NotFound(_)) => None,
            Err(e) => return Err(AppError::BadRequest(e.to_string())),
        }
    };
    // Only offer what the entry does not already say.
    let suggestions: Vec<(String, String)> = found
        .map(|f| {
            f.fields
                .into_iter()
                .filter(|(name, value)| entry.field(name).is_none_or(|have| have != value))
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(json!({ "key": key, "suggestions": suggestions })))
}
