//! The MCP server (SPEC §7.7). It gives one Streamable-HTTP endpoint per project. The endpoint
//! exposes the closed agent tool set to any MCP client over a bearer device token. It speaks
//! JSON-RPC 2.0 and serves tools only. The server never starts a message, so it declines `GET`,
//! the SSE stream, and answers every `POST` with plain JSON. `propose_patch` is the only write
//! path, and it lands as an anchored suggestion.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use galley_build::runner::BuildEvent;
use galley_build::BuildRequest;
use galley_sync::paths::{clean_rel_path, is_text_path};
use serde_json::{json, Value};

use crate::app::{AppError, AppState, CollabEvent};
use crate::auth::current::CurrentUser;
use crate::auth::Role;
use crate::store::{AgentRun, User};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const READ_CAP: usize = 200_000;
const SEARCH_CAP: usize = 200;
const COMPILE_WAIT: Duration = Duration::from_secs(240);

/// Streamable HTTP lets a server decline the GET stream. This server has nothing to push.
pub async fn info() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "error": "This MCP endpoint is request/response only. POST JSON-RPC here." })),
    )
        .into_response()
}

pub async fn rpc(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<Value>,
) -> Response {
    // This checks project access once. Each tool checks its own capability later.
    if app.require(&user, &id, Role::can_view, "using this project").await.is_err() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "No such project." }))).into_response();
    }
    match body {
        Value::Array(msgs) => {
            let mut out = Vec::new();
            for m in msgs {
                if let Some(r) = handle(&app, &id, &user, m).await {
                    out.push(r);
                }
            }
            if out.is_empty() {
                StatusCode::ACCEPTED.into_response()
            } else {
                Json(Value::Array(out)).into_response()
            }
        }
        m => match handle(&app, &id, &user, m).await {
            Some(r) => Json(r).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

/// Turn one JSON-RPC message into its response. Return `None` for a notification.
async fn handle(app: &AppState, project: &str, user: &User, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let is_notification = id.is_none() || matches!(id, Some(Value::Null));
    if is_notification {
        return None;
    }
    let id = id.unwrap();
    let result: Result<Value, (i64, String)> = match method.as_str() {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if PROTOCOL_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSIONS[0] };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false }, "prompts": { "listChanged": false } },
                "serverInfo": { "name": "galley", "version": crate::VERSION },
                "instructions": with_conventions(app, project, INSTRUCTIONS).await,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_schemas() })),
        "prompts/list" => Ok(json!({ "prompts": prompt_list() })),
        "prompts/get" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match prompt_text(name, &args) {
                Some((description, text)) => Ok(json!({
                    "description": description,
                    "messages": [{ "role": "user", "content": { "type": "text", "text": with_conventions(app, project, &text).await } }],
                })),
                None => Err((-32602, format!("Unknown prompt: {name}"))),
            }
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(app, project, user, name, args).await {
                Ok(text) => Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false })),
                Err(ToolError::Unknown(n)) => Err((-32602, format!("Unknown tool: {n}"))),
                Err(ToolError::Failed(text)) => {
                    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": true }))
                }
            }
        }
        other => Err((-32601, format!("Method not found: {other}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

const INSTRUCTIONS: &str = "This is a Galley LaTeX project. Call map first for the structure: sections, \
labels, figures, tables and citations. Then read files and the last build log, search, \
find passages about a topic with context, read files and the last build log, search for exact strings, \
compile, and propose edits with propose_patch. Proposals become suggestions the authors review in the \
editor; nothing you do writes to the document directly. Keep each edit's `find` text unique in the file.";

fn tool_schemas() -> Value {
    json!([
        { "name": "list_files", "description": "List the project's files with sizes.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "read_file", "description": "Read a text file as the authors currently see it (the live document, including unsaved edits), optionally a 1-based line range. This is the text propose_patch matches against.",
          "inputSchema": { "type": "object", "required": ["path"], "properties": {
              "path": { "type": "string" }, "start_line": { "type": "integer" }, "end_line": { "type": "integer" } } } },
        { "name": "map", "description": "The paper's structure: sections with their word counts, labelled figures, tables and equations, what cites what, undefined references and uncited bibliography entries. Read this before searching. `focus` (a label, citation key, section title or file) ranks the map around one thing.",
          "inputSchema": { "type": "object", "properties": {
              "focus": { "type": "string" }, "budget_tokens": { "type": "integer" } } } },
        { "name": "context", "description": "Find the passages that discuss something, ranked, each with the section it is in. One line per passage; pass detail for the passages themselves. Use this to locate an argument or a claim; use search only when you need an exact string or a regex.",
          "inputSchema": { "type": "object", "required": ["query"], "properties": {
              "query": { "type": "string" }, "limit": { "type": "integer" }, "budget_tokens": { "type": "integer" },
              "detail": { "type": "boolean" } } } },
        { "name": "bib", "description": "The bibliography's health: keys cited with no entry, the same work entered twice, entries nothing cites, missing fields, and arXiv preprints that may have been published since.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "literature", "description": "Look the bibliography up in open catalogues. `kind`: \"enrich\" fills in missing fields and finds published versions of preprints; \"coverage\" lists work that several of your own references cite and you do not. Off unless the project turns catalogue lookups on. Never invent a citation from this. Every candidate carries a DOI to check.",
          "inputSchema": { "type": "object", "properties": {
              "kind": { "type": "string", "enum": ["enrich", "coverage"] }, "limit": { "type": "integer" } } } },
        { "name": "math", "description": "Search the paper's formulas by structure, not by text: an expression finds formulas of the same shape even when the symbols differ, and a subexpression finds the formulas containing it. With no query, returns what the mathematics is made of: formula counts, the symbols used, and the labelled equations. One line per hit; pass detail for the full source.",
          "inputSchema": { "type": "object", "properties": {
              "query": { "type": "string" }, "limit": { "type": "integer" }, "detail": { "type": "boolean" } } } },
        { "name": "search", "description": "Regex search across the project's text files (pending edits are saved first); returns file:line matches.",
          "inputSchema": { "type": "object", "required": ["pattern"], "properties": { "pattern": { "type": "string" } } } },
        { "name": "read_log", "description": "The last build's outcome: structured errors and warnings with hints.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "compile", "description": "Save pending edits and compile the project (or one .tex file); returns the structured result. Slow; use sparingly. Proposals are not applied until an author accepts them, so a compile will not reflect your own propose_patch.",
          "inputSchema": { "type": "object", "properties": { "file": { "type": "string" } } } },
        { "name": "propose_patch", "description": "Propose edits as reviewable suggestions. Each edit replaces one unique occurrence of `find` in `path` with `replace`. The only way to change a document.",
          "inputSchema": { "type": "object", "required": ["path", "edits", "summary"], "properties": {
              "path": { "type": "string" },
              "edits": { "type": "array", "minItems": 1, "items": { "type": "object", "required": ["find", "replace"],
                  "properties": { "find": { "type": "string" }, "replace": { "type": "string" } } } },
              "summary": { "type": "string", "description": "One paragraph for the reviewers." },
              "agent": { "type": "string", "description": "Which agent this is, e.g. fix-build." },
              "model": { "type": "string" } } } },
        { "name": "comment", "description": "Leave a review comment anchored to a unique piece of text. Never changes the document; the reviewer agent uses this instead of propose_patch.",
          "inputSchema": { "type": "object", "required": ["path", "find", "body"], "properties": {
              "path": { "type": "string" }, "find": { "type": "string", "description": "Text to anchor to; must occur exactly once." },
              "body": { "type": "string" }, "agent": { "type": "string" } } } }
    ])
}

enum ToolError {
    Unknown(String),
    Failed(String),
}

impl From<AppError> for ToolError {
    fn from(e: AppError) -> Self {
        ToolError::Failed(e.to_string())
    }
}

impl From<crate::registry::RegistryError> for ToolError {
    fn from(e: crate::registry::RegistryError) -> Self {
        ToolError::from(AppError::from(e))
    }
}

async fn call_tool(app: &AppState, project: &str, user: &User, name: &str, args: Value) -> Result<String, ToolError> {
    match name {
        "list_files" => list_files(app, project).await,
        "read_file" => read_file(app, project, &args).await,
        "search" => search(app, project, &args).await,
        "map" => map(app, project, &args).await,
        "context" => context(app, project, &args).await,
        "bib" => bib(app, project, &args).await,
        "math" => math(app, project, &args).await,
        "literature" => literature(app, project, &args).await,
        "read_log" => read_log(app, project).await,
        "compile" => compile(app, project, user, &args).await,
        "propose_patch" => propose_patch(app, project, user, &args).await,
        "comment" => comment(app, project, user, &args).await,
        other => Err(ToolError::Unknown(other.to_string())),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn text_path(args: &Value) -> Result<String, ToolError> {
    let raw = arg_str(args, "path").ok_or_else(|| ToolError::Failed("`path` is required.".into()))?;
    let rel = clean_rel_path(raw).ok_or_else(|| ToolError::Failed(format!("{raw} is not a valid project path.")))?;
    if !is_text_path(&rel) {
        return Err(ToolError::Failed(format!("{rel} is not a text file.")));
    }
    Ok(rel)
}

async fn list_files(app: &AppState, project: &str) -> Result<String, ToolError> {
    let p = app.registry.open(project).await?;
    let files = tokio::task::spawn_blocking(move || p.list_files())
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let meta = app.registry.meta(project).map_err(AppError::from)?;
    let mut out = format!("main file: {}\n", meta.main_file);
    for f in files {
        out.push_str(&format!("{}\t{} bytes\t{:?}\n", f.path, f.size, f.kind));
    }
    Ok(out)
}

async fn read_file(app: &AppState, project: &str, args: &Value) -> Result<String, ToolError> {
    let rel = text_path(args)?;
    let p = app.registry.open(project).await?;
    let doc = p.doc(&rel).await.map_err(|e| ToolError::Failed(e.to_string()))?;
    let text = doc.text().await;
    let start = args.get("start_line").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let end = args.get("end_line").and_then(Value::as_u64).map(|e| e as usize);
    let lines: Vec<&str> = text.lines().collect();
    let end = end.unwrap_or(lines.len()).min(lines.len());
    if start > end && !lines.is_empty() {
        return Err(ToolError::Failed(format!("{rel} has {} lines.", lines.len())));
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(end).skip(start - 1) {
        out.push_str(&format!("{:>5}\t{line}\n", i + 1));
        if out.len() > READ_CAP {
            out.push_str("… (truncated; ask for a narrower line range)\n");
            break;
        }
    }
    Ok(out)
}

/// The paper map (docs/RETRIEVAL.md §3). It holds ranked and budgeted structure and cross-references.
async fn map(app: &AppState, project: &str, args: &Value) -> Result<String, ToolError> {
    let p = app.registry.open(project).await?;
    // This follows the same rule as search. Land pending edits so the map describes the live text.
    let _ = p.flush_now().await;
    let main_file = app.registry.meta(project).map_err(AppError::from)?.main_file;
    let dir = p.workdir().to_path_buf();
    let focus = galley_index::Focus::parse(arg_str(args, "focus").unwrap_or(""));
    let budget = args.get("budget_tokens").and_then(Value::as_u64).unwrap_or(1500).clamp(200, 8000) as usize;
    tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &main_file);
        let ranked = galley_index::rank(&paper, &focus);
        galley_index::render(&paper, &ranked, budget)
    })
    .await
    .map_err(|e| ToolError::Failed(e.to_string()))
}

/// Ranked passages for a question (docs/RETRIEVAL.md §4).
async fn context(app: &AppState, project: &str, args: &Value) -> Result<String, ToolError> {
    let query = arg_str(args, "query").ok_or_else(|| ToolError::Failed("`query` is required.".into()))?.to_string();
    let p = app.registry.open(project).await?;
    let _ = p.flush_now().await;
    let main_file = app.registry.meta(project).map_err(AppError::from)?.main_file;
    let dir = p.workdir().to_path_buf();
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5).clamp(1, 20) as usize;
    let budget = args.get("budget_tokens").and_then(Value::as_u64).unwrap_or(1200).clamp(60, 8000) as usize;
    let detail = args.get("detail").and_then(Value::as_bool).unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &main_file);
        let hits = galley_index::search(&paper, &query, limit);
        galley_index::render_hits(&paper, &query, &hits, budget, detail)
    })
    .await
    .map_err(|e| ToolError::Failed(e.to_string()))
}

/// Bibliography audit (docs/RETRIEVAL.md §5), from the project's own files.
async fn bib(app: &AppState, project: &str, _args: &Value) -> Result<String, ToolError> {
    let p = app.registry.open(project).await?;
    let _ = p.flush_now().await;
    let main_file = app.registry.meta(project).map_err(AppError::from)?.main_file;
    let dir = p.workdir().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &main_file);
        let findings = galley_index::bib::audit(&paper);
        galley_index::bib::render(&paper, &findings)
    })
    .await
    .map_err(|e| ToolError::Failed(e.to_string()))
}

/// Catalogue lookups (docs/RETRIEVAL.md §5). They are opt-in per project. They never send document text.
async fn literature(app: &AppState, project: &str, args: &Value) -> Result<String, ToolError> {
    let meta = app.registry.meta(project).map_err(AppError::from)?;
    if !meta.literature {
        return Err(ToolError::Failed(
            "Catalogue lookups are off for this project. An editor can turn them on in project settings; \
             they send citation keys, DOIs and titles to Crossref and OpenAlex, never the document."
                .into(),
        ));
    }
    let kind = arg_str(args, "kind").unwrap_or("coverage").to_string();
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10).clamp(1, 25) as usize;
    let p = app.registry.open(project).await?;
    let _ = p.flush_now().await;
    let dir = p.workdir().to_path_buf();
    let contact = app.contact_email.clone();
    let paper = {
        let (dir, main_file) = (dir.clone(), meta.main_file.clone());
        tokio::task::spawn_blocking(move || galley_index::scan(&dir, &main_file))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?
    };
    let client = galley_lit::Client::new(&dir.join(".galley").join("lit"), contact);
    Ok(match kind.as_str() {
        "enrich" => {
            let (suggestions, problems) = galley_lit::enrich(&client, &paper).await;
            galley_lit::enrich_report(&suggestions, &problems)
        }
        _ => {
            let (candidates, from, problems) = galley_lit::coverage(&client, &paper, limit).await;
            galley_lit::coverage_report(&candidates, from, &problems)
        }
    })
}

/// Structural formula search (docs/RETRIEVAL.md §6).
async fn math(app: &AppState, project: &str, args: &Value) -> Result<String, ToolError> {
    let p = app.registry.open(project).await?;
    let _ = p.flush_now().await;
    let main_file = app.registry.meta(project).map_err(AppError::from)?.main_file;
    let dir = p.workdir().to_path_buf();
    let query = arg_str(args, "query").unwrap_or("").to_string();
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(8).clamp(1, 40) as usize;
    let detail = args.get("detail").and_then(Value::as_bool).unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        let paper = galley_index::scan(&dir, &main_file);
        let formulas = galley_index::math::formulas(&dir, &paper);
        if query.trim().is_empty() {
            return galley_index::math::render_overview(&paper, &formulas);
        }
        let hits = galley_index::math::search(&formulas, &query, limit);
        galley_index::math::render_matches(&paper, &formulas, &query, &hits, detail)
    })
    .await
    .map_err(|e| ToolError::Failed(e.to_string()))
}

async fn search(app: &AppState, project: &str, args: &Value) -> Result<String, ToolError> {
    let pattern = arg_str(args, "pattern").ok_or_else(|| ToolError::Failed("`pattern` is required.".into()))?;
    let re = regex::Regex::new(pattern).map_err(|e| ToolError::Failed(format!("Bad regex: {e}")))?;
    let p = app.registry.open(project).await?;
    // Search the working tree after landing pending edits, so results match the live text.
    let _ = p.flush_now().await;
    let dir = p.workdir().to_path_buf();
    let files = tokio::task::spawn_blocking(move || p.list_files())
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let mut out = String::new();
    let mut hits = 0usize;
    for f in files.iter().filter(|f| is_text_path(&f.path)) {
        let Ok(content) = tokio::fs::read_to_string(dir.join(&f.path)).await else { continue };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                out.push_str(&format!("{}:{}: {}\n", f.path, i + 1, line.trim_end()));
                hits += 1;
                if hits >= SEARCH_CAP {
                    out.push_str("… (more matches; narrow the pattern)\n");
                    return Ok(out);
                }
            }
        }
    }
    if hits == 0 {
        out.push_str("No matches.\n");
    }
    Ok(out)
}

fn summarize(result: &galley_build::BuildResult) -> String {
    let mut out = format!(
        "status: {:?}\nfile: {}\nerrors: {}  warnings: {}\n",
        result.status, result.main_file, result.error_count, result.warning_count
    );
    if let Some(m) = &result.message {
        out.push_str(&format!("message: {m}\n"));
    }
    for d in &result.errors {
        let loc = match (&d.file, d.line) {
            (Some(f), Some(l)) => format!("{f}:{l}"),
            (Some(f), None) => f.clone(),
            _ => String::from("-"),
        };
        out.push_str(&format!("\n[{:?}] {loc}\n  {}\n", d.level, d.message));
        if let Some(h) = &d.hint {
            out.push_str(&format!("  hint: {h}\n"));
        }
        if let Some(fix) = &d.fix {
            out.push_str(&format!("  one-click fix available: {}\n", fix_label(fix)));
        }
    }
    out
}

fn fix_label(fix: &galley_build::Fix) -> String {
    match fix {
        galley_build::Fix::Insert { label, .. } | galley_build::Fix::Replace { label, .. } => label.clone(),
    }
}

async fn read_log(app: &AppState, project: &str) -> Result<String, ToolError> {
    let builds = app.builds(project).await?;
    match builds.last().await {
        Some(r) => Ok(summarize(&r)),
        None => Ok("No build yet. Call `compile` first.".into()),
    }
}

async fn compile(app: &AppState, project: &str, user: &User, args: &Value) -> Result<String, ToolError> {
    app.require(user, project, Role::can_compile, "building").await?;
    let builds = app.builds(project).await?;
    let mut rx = builds.subscribe();
    let file = arg_str(args, "file").map(str::to_string);
    let (lint_disabled, figure_cache) = app.registry.meta(project).map(|m| (m.lint_disabled, m.figure_cache)).unwrap_or((Vec::new(), true));
    builds.request(BuildRequest { draft: false, file, lint_disabled, figure_cache }).await;
    let finished = tokio::time::timeout(COMPILE_WAIT, async {
        loop {
            match rx.recv().await {
                Ok(BuildEvent::BuildFinished(r)) => break Some(r),
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break None,
            }
        }
    })
    .await;
    match finished {
        Ok(Some(r)) => Ok(summarize(&r)),
        Ok(None) => Err(ToolError::Failed("The build queue closed before finishing.".into())),
        Err(_) => Err(ToolError::Failed("The build did not finish in time; check read_log later.".into())),
    }
}

async fn propose_patch(app: &AppState, project: &str, user: &User, args: &Value) -> Result<String, ToolError> {
    app.require(user, project, Role::can_suggest, "proposing edits").await?;
    let rel = text_path(args)?;
    let summary = arg_str(args, "summary").ok_or_else(|| ToolError::Failed("`summary` is required.".into()))?;
    let agent = arg_str(args, "agent").unwrap_or("agent").to_string();
    let model = arg_str(args, "model").map(str::to_string);
    let edits = args
        .get("edits")
        .and_then(Value::as_array)
        .filter(|e| !e.is_empty())
        .ok_or_else(|| ToolError::Failed("`edits` must be a non-empty array of {find, replace}.".into()))?;

    let p = app.registry.open(project).await?;
    let doc = p.doc(&rel).await.map_err(|e| ToolError::Failed(e.to_string()))?;
    let text = doc.text().await;

    // Validate every edit before creating anything, so a bad one does not leave half a proposal.
    let mut planned: Vec<(usize, usize, String, String)> = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let find = e.get("find").and_then(Value::as_str).unwrap_or("");
        let replace = e.get("replace").and_then(Value::as_str).unwrap_or("");
        if find.is_empty() {
            return Err(ToolError::Failed(format!("edit {}: `find` is empty.", i + 1)));
        }
        let count = text.matches(find).count();
        if count != 1 {
            return Err(ToolError::Failed(format!(
                "edit {}: `find` occurs {count} times in {rel}; it must occur exactly once. Include more surrounding text.",
                i + 1
            )));
        }
        let start = text.find(find).unwrap();
        planned.push((start, start + find.len(), find.to_string(), replace.to_string()));
    }

    // Author the suggestions as the member, marked as coming through an agent.
    let mut as_agent = user.clone();
    as_agent.name = format!("{} · via {agent}", user.name);
    let mut ids = Vec::new();
    for (start, end, find, replace) in &planned {
        let (Some(anchor), Some(anchor_end)) = (doc.anchor_at(*start as u32).await, doc.anchor_at(*end as u32).await)
        else {
            return Err(ToolError::Failed("Could not anchor the edit; the text may have just changed. Re-read and retry.".into()));
        };
        let store = app.store.clone();
        let (pid, file, author, q, r) = (project.to_string(), rel.clone(), as_agent.clone(), find.clone(), replace.clone());
        let suggestion = tokio::task::spawn_blocking(move || {
            store.add_suggestion(&pid, &file, &anchor, &anchor_end, &q, &r, &author)
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .map_err(|e| ToolError::Failed(e.to_string()))?;
        ids.push(suggestion.id.clone());
        app.emit(project, CollabEvent::SuggestionAdded { suggestion }).await;
    }

    let store = app.store.clone();
    let (pid, u, a, m, s, n) = (project.to_string(), user.clone(), agent.clone(), model.clone(), summary.to_string(), planned.len() as i64);
    let run: AgentRun = tokio::task::spawn_blocking(move || store.record_agent_run(&pid, &u, &a, m.as_deref(), &s, n))
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    write_run_log(p.workdir(), &run, &rel, &planned, &ids).await;
    app.store.audit(Some(project), Some(user), "agent.propose", Some(&format!("{agent}: {} edit(s) to {rel}", ids.len())));

    Ok(format!(
        "Proposed {} suggestion(s) in {rel}: {}. The authors will see them in the editor and can accept or reject each.",
        ids.len(),
        ids.join(", ")
    ))
}

/// The run record that every proposal leaves behind (SPEC §7.4). It is masked from the compile sandbox.
async fn write_run_log(workdir: &std::path::Path, run: &AgentRun, file: &str, edits: &[(usize, usize, String, String)], ids: &[String]) {
    let dir = workdir.join(".galley").join("agents");
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let line = json!({
        "run": run.id, "agent": run.agent, "model": run.model, "author": run.author_name,
        "at": run.created_at, "file": file, "summary": run.summary,
        "edits": edits.iter().zip(ids).map(|((s, e, f, r), id)| json!({ "suggestion": id, "start": s, "end": e, "find": f, "replace": r })).collect::<Vec<_>>(),
    });
    let _ = tokio::fs::write(dir.join(format!("{}.jsonl", run.id)), format!("{line}\n")).await;
}

async fn comment(app: &AppState, project: &str, user: &User, args: &Value) -> Result<String, ToolError> {
    app.require(user, project, Role::can_comment, "commenting").await?;
    let rel = text_path(args)?;
    let find = arg_str(args, "find").ok_or_else(|| ToolError::Failed("`find` is required.".into()))?;
    let body = arg_str(args, "body").ok_or_else(|| ToolError::Failed("`body` is required.".into()))?;
    let agent = arg_str(args, "agent").unwrap_or("reviewer").to_string();
    let p = app.registry.open(project).await?;
    let doc = p.doc(&rel).await.map_err(|e| ToolError::Failed(e.to_string()))?;
    let text = doc.text().await;
    let count = text.matches(find).count();
    if count != 1 {
        return Err(ToolError::Failed(format!("`find` occurs {count} times in {rel}; it must occur exactly once.")));
    }
    let start = text.find(find).unwrap() as u32;
    let Some(anchor) = doc.anchor_at(start).await else {
        return Err(ToolError::Failed("Could not anchor the comment; re-read the file and retry.".into()));
    };
    let mut as_agent = user.clone();
    as_agent.name = format!("{} · via {agent}", user.name);
    let store = app.store.clone();
    let (pid, file, quote, text_body) = (project.to_string(), rel.clone(), find.to_string(), body.to_string());
    let c = tokio::task::spawn_blocking(move || store.add_comment(&pid, &file, &anchor, Some(&quote), &as_agent, &text_body))
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let id = c.id.clone();
    app.emit(project, CollabEvent::CommentAdded { comment: c }).await;
    Ok(format!("Comment {id} added to {rel}."))
}

/// Project conventions that every run sees first. `AGENTS.md` at the project root is the cross-tool
/// convention, and it is version-controlled. `.galley/agent.md` is accepted for the spec's original name.
async fn with_conventions(app: &AppState, project: &str, text: &str) -> String {
    let Ok(p) = app.registry.open(project).await else { return text.to_string() };
    for rel in ["AGENTS.md", ".galley/agent.md"] {
        if let Ok(conv) = tokio::fs::read_to_string(p.workdir().join(rel)).await {
            let conv: String = conv.chars().take(8_000).collect();
            if !conv.trim().is_empty() {
                return format!("{text}\n\nProject conventions (from {rel}):\n{conv}");
            }
        }
    }
    text.to_string()
}

const COMMON_RULES: &str = "Rules: never invent citations, labels or results. Preserve LaTeX commands, math and \\cite keys \
unless the task is about them. Every `find` you pass must occur exactly once in the file, so include surrounding text \
to make it unique. Pass your agent name in `agent` and, if you know it, your model id in `model`. Both appear on the \
proposal so the authors know who wrote it. Finish with a one-paragraph summary for the authors.";

const CHECKOUT_RULES: &str = "Rules: never invent citations, labels or results. Preserve LaTeX commands, math and \\cite keys \
unless the task is about them. Change only what the task needs. When you are finished, call done with a one-paragraph \
summary for the authors.";

fn prompt_list() -> Value {
    json!([
        { "name": "fix-build", "description": "Read the last build's errors and propose the smallest edits that make the document compile, verifying with compile.", "arguments": [] },
        { "name": "proofread", "description": "Grammar, clarity and consistency for a file or line range, as reviewable suggestions.",
          "arguments": [ { "name": "path", "required": true }, { "name": "start_line", "required": false }, { "name": "end_line", "required": false } ] },
        { "name": "tighten", "description": "Cut words to a target (a percentage or a page count) without changing meaning.",
          "arguments": [ { "name": "path", "required": true }, { "name": "target", "description": "e.g. 15% or 'fit 8 pages'", "required": true } ] },
        { "name": "cite", "description": "Find support for a claim using the project's bibliography; never fabricate keys.",
          "arguments": [ { "name": "claim", "required": true } ] },
        { "name": "table", "description": "Turn CSV/TSV data into a booktabs table (siunitx when numeric) and optionally propose inserting it.",
          "arguments": [ { "name": "data", "required": true }, { "name": "path", "required": false }, { "name": "after", "description": "Unique text to insert after", "required": false } ] },
        { "name": "reviewer", "description": "Review as a sceptical referee: anchored comments on weak claims, missing references and undefined terms. Never edits.",
          "arguments": [ { "name": "path", "required": true } ] },
        { "name": "explain", "description": "Explain a LaTeX error or macro in plain language. No edits.",
          "arguments": [ { "name": "message", "required": false } ] }
    ])
}

/// The same task text goes to two surfaces. An MCP client has the server's tools `read_log`,
/// `compile`, `propose_patch` and `comment`. The local runner (SPEC §7.6) has a checkout and file
/// tools, already holds the build log in context, and reports through `done`. The `surface`
/// argument selects the order of the steps. The task text itself is written once.
fn prompt_text(name: &str, a: &Value) -> Option<(String, String)> {
    let g = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let checkout = g("surface") == "checkout";
    let rules = if checkout { CHECKOUT_RULES } else { COMMON_RULES };
    // "make the change" phrased for the surface's write path.
    let propose = |agent: &str| {
        if checkout { "make it with edit_file".to_string() } else { format!("propose it with propose_patch (agent \"{agent}\")") }
    };
    let (desc, body) = match name {
        "fix-build" => ("Fix the build".to_string(), if checkout { format!(
            "You are Galley's fix-build agent. The last build's errors are listed above. For each error, open the file around \
the reported line with read_file, decide the smallest change that fixes the cause (not the symptom — e.g. a missing \
\\usepackage, an unbalanced brace, an undefined label), and make it with edit_file. Then call done. If the build has no \
errors, call done and say nothing needs fixing. Do not stop after only reading: keep going until you have edited a file \
or confirmed there is nothing to fix.\n{rules}") } else { format!(
            "You are Galley's fix-build agent. Call read_log. For each error, read the file around the reported line with \
read_file, decide the smallest change that fixes the cause (not the symptom — e.g. a missing \\usepackage, an unbalanced brace, \
an undefined label), and propose it with propose_patch using agent \"fix-build\". Then call compile once to verify; if new errors \
appear, fix those too, but never call compile more than three times. If the log is clean, say so and stop. Do not \
stop after only reading the log: keep going until you have called propose_patch, or you have confirmed there is nothing \
to fix.\n{rules}") }),
        "proofread" => (format!("Proofread {}", g("path")), format!(
            "You are Galley's proofread agent. Read {p} with read_file{range}. Fix grammar, clarity, and consistency of terminology \
and tense. Do not change the argument, the maths, or any command. For each change, {prop}, \
grouping nearby fixes into one edit where the text is contiguous.\n{rules}",
            p=g("path"), prop=propose("proofread"),
            range=if g("start_line").is_empty() {String::new()} else {format!(" lines {}–{}", g("start_line"), if g("end_line").is_empty(){"end".into()} else {g("end_line")})})),
        "tighten" => (format!("Tighten {}", g("path")), format!(
            "You are Galley's tighten agent. Read {p}. Cut it to {t}: remove redundancy, hedging, and throat-clearing; merge \
sentences; keep every claim, number, citation and definition. For each cut, {prop}, and report \
the before/after word counts in your summary.\n{rules}", p=g("path"), t=g("target"), prop=propose("tighten"))),
        "cite" => (format!("Cite: {}", g("claim")), format!(
            "You are Galley's cite agent. The claim is: \"{c}\". First {look} for an entry \
that supports it; if one fits, {prop} adding `\\cite{{key}}` at the claim. If nothing in the bibliography \
fits, do not invent a key: say what kind of source would support the claim and ask the author for a DOI or title.\n{rules}",
            c=g("claim"), prop=propose("cite"),
            look=if checkout {"read the project's .bib files"} else {"search the project's .bib files (search over `@`)"})),
        "table" => ("Make a table".to_string(), format!(
            "You are Galley's table agent. Turn this data into a LaTeX booktabs table (\\toprule/\\midrule/\\bottomrule, no vertical rules, \
siunitx S columns for numeric data, a \\caption and \\label):\n{d}\n{ins}\n{rules}",
            d=g("data"), ins=if g("path").is_empty() {"Return the table in your reply.".to_string()} else {format!("Insert it in {} immediately after the text \"{}\": {}.", g("path"), g("after"), propose("table"))})),
        "reviewer" => (format!("Review {}", g("path")), if checkout { format!(
            "You are Galley's reviewer agent: a sceptical but fair referee. Read {p}. Do not edit anything. Call done with your \
review as a numbered list: for each point quote the exact sentence, then say what is wrong — claims made without support, \
references the text needs but lacks, terms used before they are defined, results whose method is unclear. Aim for the five \
most important points.\n{rules}", p=g("path")) } else { format!(
            "You are Galley's reviewer agent: a sceptical but fair referee. Read {p}. Leave comments with the comment tool (agent \
\"reviewer\") on: claims made without support, references the text needs but lacks, terms used before they are defined, and \
results whose method is unclear. Anchor each comment to the exact sentence. Do not edit anything and do not call propose_patch. \
Aim for the five most important points.\n{rules}", p=g("path")) }),
        "explain" => ("Explain an error".to_string(), format!(
            "You are Galley's explain agent. {src} Explain in plain language what it means, what usually causes it, and what to check \
— in that order, briefly. Do not propose edits.{fin}",
            src=if !g("message").is_empty() {format!("The message is: \"{}\".", g("message"))} else if checkout {"Take the first error of the last build, listed above.".to_string()} else {"Call read_log and take the first error.".to_string()},
            fin=if checkout {" Put the explanation in done's summary."} else {""})),
        _ => return None,
    };
    Some((desc, body))
}

/// GET /api/projects/{id}/agent-runs. It gives attribution for the History drawer and the disclosure report.
pub async fn runs(
    State(app): State<AppState>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<Json<Vec<AgentRun>>, AppError> {
    app.require(&user, &id, Role::can_view, "viewing agent runs").await?;
    let store = app.store.clone();
    let pid = id.clone();
    let list = tokio::task::spawn_blocking(move || store.agent_runs(&pid))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(list))
}
