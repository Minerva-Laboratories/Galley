//! One run: check the project's text files out of Galley into a working directory, give the model
//! that directory and the task, and afterwards submit whatever changed as proposals.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::chat::{ChatBackend, Message};
use crate::mcp::McpClient;
use crate::patch;

const TEXT_EXTENSIONS: &[&str] = &["tex", "bib", "cls", "sty", "bst", "md", "txt", "csv", "tsv"];

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Mcp(#[from] crate::mcp::McpError),
    #[error(transparent)]
    Chat(#[from] crate::chat::ChatError),
    #[error("could not use the working directory {0}: {1}")]
    Workdir(PathBuf, std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub struct RunOptions {
    pub mcp: McpClient,
    pub chat: ChatBackend,
    /// A built-in prompt name (`fix-build`, `proofread`, …) and its arguments.
    pub prompt: String,
    pub prompt_args: Value,
    pub max_turns: usize,
    pub workdir: PathBuf,
}

#[derive(Debug, Default)]
pub struct RunReport {
    pub turns: usize,
    pub summary: String,
    /// Maps a path to what the server said when the proposal was submitted.
    pub proposals: Vec<(String, String)>,
    /// Files the model changed for which the server refused the proposal, with the reason.
    pub refused: Vec<(String, String)>,
}

/// Runs the task. `log` receives one line per step so a CLI can show progress.
pub async fn run(opts: RunOptions, log: impl Fn(&str)) -> Result<RunReport, RunError> {
    let RunOptions { mcp, chat, prompt, prompt_args, max_turns, workdir } = opts;
    let mut checkout = Checkout::default();
    std::fs::create_dir_all(&workdir).map_err(|e| RunError::Workdir(workdir.clone(), e))?;

    mcp.initialize().await?;
    let listing = mcp.call("list_files", json!({})).await?;
    if listing.is_error {
        return Err(RunError::Other(listing.text));
    }
    let (main_file, files) = parse_listing(&listing.text);

    checkout.main_file = main_file.clone();
    let base = &mut checkout.base;
    for (path, _size) in &files {
        let text = fetch_whole(&mcp, path).await?;
        let dest = workdir.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RunError::Workdir(dest.clone(), e))?;
        }
        std::fs::write(&dest, &text).map_err(|e| RunError::Workdir(dest.clone(), e))?;
        base.insert(path.clone(), text);
    }
    log(&format!("checked out {} text file(s) into {}", base.len(), workdir.display()));

    let mut args = prompt_args.clone();
    args["surface"] = json!("checkout");
    let task = mcp.prompt(&prompt, args).await?;
    let build = mcp.call("read_log", json!({})).await?.text;

    let mut file_list = String::new();
    for (path, size) in &files {
        let mark = if *path == main_file { " — main file" } else { "" };
        file_list.push_str(&format!("- {path} ({size} bytes){mark}\n"));
    }
    let system = format!(
        "You are working on a LaTeX project on behalf of its authors, using a local copy of its files. \
Change a file with edit_file (an exact `find` that occurs once in the file, and its `replace`) or, for \
larger rewrites, write_file with the complete new content. Read a file with read_file. When you have \
finished, call done with a one-paragraph summary for the authors. Every change you make becomes a \
suggestion the authors review in their editor, so change only what the task needs and leave everything \
else exactly as it is. Never invent citations, labels or results.\n\nProject files:\n{file_list}\nLast build:\n{build}"
    );
    let mut messages = vec![Message::system(system), Message::user(task)];
    let tools = tool_schemas();

    let mut report = RunReport::default();
    let mut nudged = false;
    // Small models often put the answer in narration and then call done with an empty summary.
    let mut last_text = String::new();
    while report.turns < max_turns {
        report.turns += 1;
        let reply = chat.complete(&messages, &tools).await?;
        messages.push(reply.clone());
        if let Some(t) = reply.content.as_deref().map(strip_thinking).filter(|t| !t.is_empty()) {
            last_text = t;
        }
        let Some(calls) = reply.tool_calls.clone() else {
            let text = reply.content.unwrap_or_default();
            let changed = checkout.base.iter().any(|(p, t)| read_local(&workdir, p) != *t);
            if !changed && !nudged && report.turns < max_turns {
                // Small models often narrate the plan and stop. One push gets most of them moving.
                nudged = true;
                log("model ended its turn without editing; asking it to act");
                messages.push(Message::user(
                    "You have not changed any file or called done. Continue: make the change with edit_file, or call done with your summary.",
                ));
                continue;
            }
            report.summary = strip_thinking(&text);
            break;
        };
        let mut finished = false;
        for call in calls {
            let args: Value = serde_json::from_str(&call.function.arguments).unwrap_or(json!({}));
            let name = call.function.name.as_str();
            let out = match name {
                "read_file" => tool_read(&workdir, &checkout, &args),
                "edit_file" => tool_edit(&workdir, &checkout, &args),
                "write_file" => tool_write(&workdir, &checkout, &args),
                "done" => {
                    finished = true;
                    let given = args.get("summary").and_then(Value::as_str).unwrap_or("").trim();
                    report.summary = if given.is_empty() { last_text.clone() } else { given.to_string() };
                    "ok".to_string()
                }
                other => format!("Unknown tool {other}. Use read_file, edit_file, write_file or done."),
            };
            log(&format!("{name}({}) → {}", brief_args(&args), out.lines().next().unwrap_or("")));
            messages.push(Message::tool(&call.id, out));
        }
        if finished {
            break;
        }
    }

    for (path, before) in &checkout.base {
        let after = read_local(&workdir, path);
        if after == *before {
            continue;
        }
        let edits: Vec<Value> =
            patch::edits(before, &after).into_iter().map(|e| json!({ "find": e.find, "replace": e.replace })).collect();
        let summary = if report.summary.trim().is_empty() { format!("Changes from {prompt}.") } else { report.summary.clone() };
        let res = mcp
            .call(
                "propose_patch",
                json!({ "path": path, "edits": edits, "summary": summary, "agent": prompt, "model": chat.model() }),
            )
            .await?;
        if res.is_error {
            report.refused.push((path.clone(), res.text));
        } else {
            report.proposals.push((path.clone(), res.text));
        }
    }
    Ok(report)
}

fn parse_listing(text: &str) -> (String, Vec<(String, u64)>) {
    let mut main = String::new();
    let mut files = Vec::new();
    for line in text.lines() {
        if let Some(m) = line.strip_prefix("main file: ") {
            main = m.trim().to_string();
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(path), Some(size)) = (parts.next(), parts.next()) else { continue };
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !TEXT_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let size = size.trim_end_matches(" bytes").parse().unwrap_or(0);
        files.push((path.to_string(), size));
    }
    (main, files)
}

/// `read_file` returns numbered lines and truncates long files. Page through by line range.
async fn fetch_whole(mcp: &McpClient, path: &str) -> Result<String, RunError> {
    let mut out = String::new();
    let mut start = 1u64;
    loop {
        let r = mcp.call("read_file", json!({ "path": path, "start_line": start })).await?;
        if r.is_error {
            return Err(RunError::Other(format!("{path}: {}", r.text)));
        }
        let mut last = start.saturating_sub(1);
        let mut truncated = false;
        for line in r.text.split_inclusive('\n') {
            if line.starts_with('…') {
                truncated = true;
                break;
            }
            let Some((n, body)) = line.split_once('\t') else { continue };
            last = n.trim().parse().unwrap_or(last);
            out.push_str(body);
        }
        if !truncated || last < start {
            break;
        }
        start = last + 1;
    }
    Ok(out)
}

/// The files as fetched, keyed by project path. The diff at the end is taken against these.
#[derive(Default)]
struct Checkout {
    main_file: String,
    base: BTreeMap<String, String>,
}

fn read_local(workdir: &Path, rel: &str) -> String {
    std::fs::read_to_string(workdir.join(rel)).unwrap_or_default()
}

/// Resolve the `path` argument. Small models drop it when one file is obvious and sometimes send
/// a bare file name, so a missing path means the main file and a basename that matches one
/// project file is accepted.
fn known(c: &Checkout, args: &Value) -> Result<String, String> {
    let p = args.get("path").and_then(Value::as_str).unwrap_or("").trim().trim_start_matches("./");
    if p.is_empty() {
        return Ok(c.main_file.clone());
    }
    if c.base.contains_key(p) {
        return Ok(p.to_string());
    }
    let by_name: Vec<&String> = c.base.keys().filter(|k| k.rsplit('/').next() == Some(p)).collect();
    if by_name.len() == 1 {
        return Ok(by_name[0].clone());
    }
    Err(format!("No such project file: {p:?}. The files are: {}", c.base.keys().cloned().collect::<Vec<_>>().join(", ")))
}

fn tool_read(workdir: &Path, c: &Checkout, args: &Value) -> String {
    match known(c, args) {
        Ok(p) => read_local(workdir, &p),
        Err(e) => e,
    }
}

fn tool_edit(workdir: &Path, c: &Checkout, args: &Value) -> String {
    let p = match known(c, args) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let find = args.get("find").and_then(Value::as_str).unwrap_or("");
    let replace = args.get("replace").and_then(Value::as_str).unwrap_or("");
    if find.is_empty() {
        return "`find` is empty. Give the exact text to replace.".into();
    }
    let text = read_local(workdir, &p);
    match text.matches(find).count() {
        1 => {
            let new = text.replacen(find, replace, 1);
            match std::fs::write(workdir.join(&p), &new) {
                Ok(()) => format!("Edited {p}."),
                Err(e) => format!("Could not write {p}: {e}"),
            }
        }
        0 => format!("`find` does not occur in {p}. Read the file and copy the text exactly."),
        n => format!("`find` occurs {n} times in {p}; include more surrounding text so it occurs once."),
    }
}

fn tool_write(workdir: &Path, c: &Checkout, args: &Value) -> String {
    let p = match known(c, args) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    if content.trim().is_empty() {
        return "`content` is empty; write_file replaces the whole file.".into();
    }
    match std::fs::write(workdir.join(&p), content) {
        Ok(()) => format!("Wrote {p} ({} lines).", content.lines().count()),
        Err(e) => format!("Could not write {p}: {e}"),
    }
}

/// Reasoning models may leak `<think>…</think>` into content. The authors never need it.
fn strip_thinking(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn brief_args(args: &Value) -> String {
    let mut parts = Vec::new();
    for k in ["path", "find", "summary"] {
        if let Some(v) = args.get(k).and_then(Value::as_str) {
            let v: String = v.chars().take(40).collect();
            parts.push(format!("{k}={v:?}"));
        }
    }
    parts.join(", ")
}

fn tool_schemas() -> Value {
    json!([
        { "type": "function", "function": { "name": "read_file", "description": "Read a project file (default: the main file).",
          "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } } },
        { "type": "function", "function": { "name": "edit_file", "description": "Replace one exact occurrence of `find` in a file with `replace`. `find` must occur exactly once. `path` defaults to the main file.",
          "parameters": { "type": "object", "required": ["find", "replace"], "properties": {
              "path": { "type": "string" }, "find": { "type": "string" }, "replace": { "type": "string" } } } } },
        { "type": "function", "function": { "name": "write_file", "description": "Replace a file's whole content (`path` defaults to the main file). Prefer edit_file for small changes.",
          "parameters": { "type": "object", "required": ["content"], "properties": {
              "path": { "type": "string" }, "content": { "type": "string" } } } } },
        { "type": "function", "function": { "name": "done", "description": "Finish, with a one-paragraph summary of what you changed and why, for the authors.",
          "parameters": { "type": "object", "required": ["summary"], "properties": { "summary": { "type": "string" } } } } }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_bare_paths_resolve() {
        let mut c = Checkout { main_file: "paper/main.tex".into(), base: BTreeMap::new() };
        c.base.insert("paper/main.tex".into(), String::new());
        c.base.insert("paper/refs.bib".into(), String::new());
        assert_eq!(known(&c, &json!({})).unwrap(), "paper/main.tex");
        assert_eq!(known(&c, &json!({ "path": "refs.bib" })).unwrap(), "paper/refs.bib");
        assert_eq!(known(&c, &json!({ "path": "./paper/main.tex" })).unwrap(), "paper/main.tex");
        assert!(known(&c, &json!({ "path": "intro.tex" })).is_err());
    }

    #[test]
    fn thinking_is_stripped() {
        assert_eq!(strip_thinking("<think>\nplan\n</think>\n\nThe answer."), "The answer.");
        assert_eq!(strip_thinking("plain"), "plain");
        assert_eq!(strip_thinking("<think>unterminated"), "");
    }

    #[test]
    fn listing_keeps_text_files_and_main() {
        let (main, files) = parse_listing("main file: main.tex\nmain.tex\t120 bytes\tTex\nfig.png\t9000 bytes\tImage\nrefs.bib\t40 bytes\tBib\n");
        assert_eq!(main, "main.tex");
        assert_eq!(files, vec![("main.tex".to_string(), 120), ("refs.bib".to_string(), 40)]);
    }
}
