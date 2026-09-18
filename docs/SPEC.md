# Galley product and engineering specification v1.1

> A self-hosted, single-binary collaborative LaTeX editor with git-backed history, CRDT sync, sandboxed compiles, and project-scoped LLM agents.
> Target: Ubuntu 22.04+/24.04, x86_64 and arm64. One command to install, one to run.

---

## 1. Identity

### 1.1 Name
**Galley.** A *galley proof* is the typeset draft you review before something goes to print. That is
what this tool produces. The name also refers to the nautical "everyone rowing together." It is short,
it works in lowercase, it is easy to type as a CLI (`galley`), and it has no ambiguous spelling.

*(Do a trademark and domain check before shipping. Candidates: `galley.dev`, `galleytex.org`, `getgalley.app`.)*

### 1.2 Tagline
**"Write together. Compile anywhere."**
Secondary: *Your TeX, on your terms.*

### 1.3 Product principles (used to settle every design argument)
1. **Saving never depends on anything.** Not compile, not network, not another user. If the editor is open, the text is safe.
2. **Boring infrastructure.** One binary, one data directory, SQLite, git. No queues, caches, or database servers.
3. **The project is the boundary.** Compiles and agents see the project directory and nothing else.
4. **Every automated edit is a diff.** Agents and tools propose. Humans accept.
5. **Errors are explained.** Galley does not dump raw logs by default.
6. **Works without an account.** Reviewers get a link. Owners get a password or OIDC.

### 1.4 Visual identity
- **Metaphor:** paper and proofreader's ink.
- **Palette**
  - Ink `#1B1A17` (text, dark bg)
  - Paper `#F6F3EC` (light bg) / Vellum `#ECE7DC` (panels)
  - Galley Red `#C6402A` (accent: cursor, primary buttons, error markers, the red pen)
  - Slate `#5B6470` (secondary text), Moss `#3E7C59` (success/compiled), Amber `#C98A1B` (warnings)
  - Dark theme: bg `#1B1A17`, panel `#23221E`, text `#EAE6DC`, accent unchanged
- **Type**
  - UI: *Inter* (variable)
  - Editor: *JetBrains Mono* (ligatures off by default)
  - Document-flavored headings (landing, onboarding, empty states): *Source Serif 4*
- **Logo:** a lowercase serif wordmark `galley` with the `y`'s descender ending in a red proofreader's caret (`^`). Mark-only version: a red caret inside a rounded paper-colored square. Favicon: the caret.
- **Voice:** calm and precise. Error copy says what happened and what to do. Never write "Oops!".

---

## 2. Scope

### 2.1 v1.0 scope: in
- Real-time collaborative editing (CRDT), offline-capable
- Git-backed history: auto-commits, named checkpoints, time-travel, diffs, PDF diff
- Sandboxed compile (Tectonic default, TeX Live optional), SyncTeX both ways, structured errors
- Comments, suggestions (track changes), presence
- Bibliography manager (a DOI, an arXiv id or a URL becomes an entry, dedupe, Zotero import)
- Agents with pluggable model backends: fix-build, proofread, tighten, table-from-data, cite, explain
- Grammar and spell checks via bundled LanguageTool (optional download)
- Templates gallery, snippets, image drop-in
- Share links (view / comment / edit), local accounts, optional OIDC
- Single-binary install, auto-HTTPS, Docker Compose
- Import from an Overleaf zip or any git repo. Export a zip or push to git

### 2.2 v1.0 scope: out (explicitly)
- WYSIWYG / rich-text mode
- Multi-tenant hosting
- Plugin marketplace (there is a plugin *API*, but no store)
- Mobile editing beyond read + comment

---

## 3. Architecture

```
┌───────────────────────────────── galley (single binary) ─────────────────────────────────┐
│                                                                                           │
│  HTTP/WS server (axum) ── embedded SPA (CodeMirror 6, PDF.js) ── auth (local / OIDC)      │
│         │                                                                                 │
│  ┌──────┴────────┐   ┌──────────────┐   ┌──────────────────┐   ┌──────────────────────┐   │
│  │ Sync (yrs)    │   │ History      │   │ Build runner     │   │ Agent runner         │   │
│  │ CRDT docs     │──▶│ git2 repos   │──▶│ sandbox + Tectonic│   │ sandbox + model API  │   │
│  │ awareness     │   │ checkpoints  │   │ log parser       │   │ tool loop, diff out  │   │
│  └───────────────┘   └──────────────┘   └──────────────────┘   └──────────────────────┘   │
│         │                    │                    │                       │               │
│  SQLite (users, projects, shares, comments, jobs, sessions)   ~/.galley/data/<project>/   │
└───────────────────────────────────────────────────────────────────────────────────────────┘
```

**Language/stack**
- Backend: Rust with `axum`, `tokio`, `yrs` (Yjs port), `git2`, `rusqlite`, `tower-sessions`, `rustls`, `instant-acme` (auto-TLS)
- Frontend: TypeScript, Vite, CodeMirror 6 (`@codemirror/lang-latex` or custom Lezer grammar), `yjs` + `y-codemirror.next`, PDF.js, Solid or Preact (small runtime), Tailwind with the design tokens above
- Sandbox: `bubblewrap` first, then `docker`, then `none` (warned, for dev)
- Compile: Tectonic (bundled download on first use), or a configured TeX Live path
- Grammar: LanguageTool server jar (optional, downloaded on demand, needs a JRE that the installer offers)

**Process model**: one process on the tokio runtime. Build and agent jobs run as child processes under the sandbox with cgroup limits (`systemd-run --scope` when available).

---

## 4. Data model

### 4.1 On disk
```
~/.galley/
  galley.toml            # config
  galley.db              # SQLite
  tectonic/              # engine + package cache
  languagetool/          # optional
  data/
    <project_id>/
      .git/
      main.tex
      sections/…
      figures/…
      .galley/
        doc-state.ybin    # CRDT state vectors (per file), fsync'd
        build/            # aux files, last-good.pdf, last.log, synctex
        agents/           # run transcripts (jsonl), proposed patches
```
Every project is a plain git repo. `git clone` of the directory works. You need nothing proprietary to recover a project.

### 4.2 SQLite tables (abridged)
```
users(id, email, name, pw_hash, oidc_sub, created_at)
projects(id, name, owner_id, engine, main_file, created_at, archived)
members(project_id, user_id, role)           -- owner|editor|commenter|viewer
share_links(id, project_id, role, token, expires_at, label)
comments(id, project_id, file, anchor_ybin, author_id, body, resolved, created_at)
suggestions(id, project_id, file, anchor_ybin, author_id, kind, payload, status)
checkpoints(id, project_id, commit_sha, label, author_id, created_at)
builds(id, project_id, commit_sha, status, started, finished, errors_json, pdf_path)
agent_runs(id, project_id, agent, user_id, status, prompt, patch_path, tokens, cost)
sessions(...) settings(...)
```

### 4.3 Roles
| Role | Edit | Comment | Suggest | Compile | Agents | History | Share |
|---|---|---|---|---|---|---|---|
| Owner | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ + revert | ✓ |
| Editor | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | – |
| Commenter | – | ✓ | ✓ | ✓ (read PDF) | – | view | – |
| Viewer | – | – | – | – | – | view | – |

---

## 5. Sync & versioning

### 5.1 Sync
- One Yjs document per text file (`Y.Text`) plus a project-level `Y.Map` for the file tree and metadata. Binary assets are not CRDT'd. They are content-addressed uploads committed to git directly.
- Transport: WebSocket, `y-protocols` sync and awareness. The server is the authoritative store, and clients merge with peer-to-peer semantics. A reconnect is a state-vector exchange, never a "reload to see changes".
- Persistence: the server applies updates to `yrs` in memory and appends to `doc-state.ybin` with fsync every 250 ms of activity (debounced) and on every client disconnect. It compacts on checkpoint.
- Offline: IndexedDB persistence (`y-indexeddb`) in the PWA. Edits made offline merge on reconnect. The UI shows a small "offline, changes stored locally" pill. Nothing is modal.

### 5.2 Git flush
- A **flusher** task writes CRDT text to the working tree and commits when: 4 s of quiet after an edit, or 60 s continuous editing, or before any build. The commit author is the last editor. The message is generated (`edit: main.tex, sections/2.tex`).
- **Checkpoints** are user-labeled tags on a commit ("Submitted to NeurIPS", "Before rewrite of §3"). Auto-commits are squashable. The history view shows checkpoints by default, and "show all" reveals auto-commits.
- **Time travel**: a scrubber over commits. Selecting one shows the file at that point (read-only) with a diff gutter against the current version. "Restore this version" creates a new commit. It never rewrites history.
- **PDF diff**: Galley runs `latexdiff` between two commits, compiles the result in the sandbox, and shows it side-by-side or as a marked-up PDF.
- **Remotes**: an optional `origin` (GitHub, GitLab or self-hosted). Push on checkpoint, or manually. A pull creates a merge commit and re-seeds the CRDT from the merged text. This is a rare, explicit operation with a confirmation dialog.

### 5.3 Conflicts
The CRDT means there are no textual conflicts in normal use. The only merge point is a git pull from an
external remote. Conflicts there appear in a three-pane merge UI, and Galley re-seeds the CRDT only
after resolution. Galley supports one branch per person as **workspaces**. A workspace is a git branch
with its own CRDT docs. "Merge workspace into main" uses the same UI.

---

## 6. Build pipeline

### 6.1 Flow
```
edit → flusher commit → build request → queue (per-project, latest-wins) → sandbox
   → engine (tectonic | latexmk) → log parser → artifacts (pdf, synctex, log, errors.json)
   → WS event → client swaps PDF (keeps last-good if failed)
```
- **Latest-wins queue**: a new request cancels a request that has not started. A running build finishes, but Galley marks its result stale if a newer request exists.
- **Auto-build** (default on): it triggers 1.5 s after the last edit, only if the previous build finished. A manual `Ctrl+Enter` is always allowed.
- **Draft mode**: passes the `draft` class option and replaces figures with boxes, for 3–5× faster iteration. Toggle it in the build bar.
- **Timeout**: 120 s by default (config), memory 2 GB, no network. Tectonic package fetches go through the host-side cache, not from inside the sandbox.

### 6.2 Sandbox
`bwrap --ro-bind /usr /usr --ro-bind $TECTONIC_CACHE /cache --bind $PROJECT /work --unshare-all --die-with-parent --new-session --chdir /work tectonic -X compile main.tex`
Fallback: `docker run --rm --network none -v $PROJECT:/work galley/engine:tectonic`.

### 6.3 Structured errors
The log parser produces `errors.json`:
```json
{"level":"error","file":"sections/2.tex","line":41,"code":"undefined-control-sequence",
 "message":"Undefined control sequence \\citep",
 "hint":"\\citep comes from the natbib package. Add \\usepackage{natbib} to the preamble, or use \\cite.",
 "fix":{"kind":"insert","file":"main.tex","after_line":6,"text":"\\usepackage{natbib}"}}
```
About 60 common error codes ship with hints and, where safe, one-click fixes. An unknown error shows the raw excerpt and an "Ask Galley" button that hands the excerpt to the fix-build agent.

---

## 7. Agents

### 7.1 Model
An **agent** is a named prompt, a tool set and an output contract. All agents run through one runner in the project sandbox. They never get a shell.

**Tools available to agents (the complete list):**
| Tool | Description |
|---|---|
| `map(focus?, budget_tokens?)` | the paper map: sections with word counts, labelled objects with captions, citations per section, undefined references, uncited entries. It is ranked around `focus` and budgeted (docs/RETRIEVAL.md §3). The first call an agent should make |
| `context(query, limit?, budget_tokens?)` | ranked passages for a question, each with its section and label. It uses BM25 over the paragraphs of the paper, boosted by section names and phrase matches (docs/RETRIEVAL.md §4) |
| `bib()` | bibliography health: cited-but-missing keys, duplicate works, uncited entries, missing fields, preprints that may have been published (docs/RETRIEVAL.md §5) |
| `literature(kind, limit?)` | opt-in catalogue lookups. `enrich` fills missing fields and finds published versions of preprints (Crossref). `coverage` lists work several of your own references cite and you do not (OpenAlex). It sends keys, DOIs and titles only (docs/RETRIEVAL.md §5) |
| `math(query?, limit?, detail?)` | structural formula search. An expression matches formulas of the same shape whatever the letters. A subexpression matches the formulas that contain it. With no query, it returns a survey of the paper's mathematics (docs/RETRIEVAL.md §6) |
| `list_files()` | project tree |
| `read_file(path, range?)` | text files only, size-capped |
| `search(pattern)` | ripgrep over project |
| `read_log()` | the last build's structured errors and a log excerpt |
| `compile()` | run a build, return errors.json (rate-limited, 3/run) |
| `lookup_citation(query)` | Crossref/arXiv/DOI resolver via host proxy (allowlisted domains only) |
| `propose_patch(path, edits, summary)` | the *only* write path, and a terminal action. `edits` is a list of `{find, replace}`. Each `find` must occur exactly once in `path`, and each edit lands as one anchored suggestion |

**Output contract**: one or more `{find, replace}` edits and a one-paragraph summary. Each edit becomes an anchored suggestion in the existing review surface, accepted or rejected per edit. Galley attributes it to the member as "Name · via <agent>" and records it in `agent_runs` and `.galley/agents/<run>.jsonl`. Accepting applies the edit through the CRDT as the user's own edit.

### 7.2 Backends (`galley.toml`)
```toml
[agents]
enabled = true
default_backend = "ollama"

[agents.backends.ollama]
url = "http://127.0.0.1:11434"
model = "qwen2.5-coder:14b"

[agents.backends.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-6"

[agents.backends.openai_compatible]
url = "https://…/v1"
api_key_env = "MY_KEY"
model = "…"
```
A project may override these settings. A banner in the Agents panel states which backend is active and whether data leaves the machine.

### 7.3 Built-in agents
| Agent | Trigger | What it does |
|---|---|---|
| **Fix build** | error card → "Fix", or auto-offer on a failed build | reads the errors and the context, proposes a minimal patch, verifies it with `compile()` |
| **Proofread** | select text → `Ctrl+.` → Proofread | checks grammar, clarity and consistency. It preserves LaTeX and returns suggestion-mode edits rather than a patch |
| **Tighten** | same menu | cuts words to a target (a percentage, or "fit in 8 pages") |
| **Table** | paste CSV/TSV, or right-click a table | writes a `booktabs` table with alignment, and siunitx if the data is numeric |
| **Cite** | typing after `\cite{`, or "Find a citation for this claim" | searches Crossref and arXiv, inserts the bib entry and the key |
| **Explain** | hover an error or macro → Explain | gives a plain-language explanation and makes no edit |
| **Reviewer** | Agents panel → "Review as reviewer 2" | produces comments anchored to paragraphs, never edits: weak claims, missing refs, undefined terms |
| **Figure captions** | right-click `\includegraphics` | drafts the caption and alt text from the image (vision-capable backends only) |

### 7.4 Guardrails
- A token and cost budget per run and per day (config), shown in the panel.
- Agents cannot read `.git`, `.galley/agents`, or files matching `.galleyignore`.
- Every run is logged to `.galley/agents/<run>.jsonl` (prompt, tool calls, patch) and visible in History.

### 7.5 Agents in a shared project

V1 hosts no inference and runs no agent server-side. Galley never buys a token. In V1 every run executes on the member's own machine through the runner (§7.6) or an MCP client (§7.7). Server-side execution against a member's key is a post-V1 option. When it exists, a run executes in one of two places:

- **Server-side**, calling a remote endpoint with the member's own key (Anthropic, OpenAI, or any
  OpenAI-compatible base URL).
- **On the member's own machine**, through a local agent runner that pulls work from Galley (§7.6).
  This is how a local Ollama or llama.cpp model works even against hosted Galley. The member exposes
  nothing to the internet.

A server calling `localhost:11434` would dial its own loopback. The runner is therefore the supported
path for local models, and an inbound connection is not.

**Where a run executes**, resolved in this order and shown on the run card before it starts:
1. The triggering member's own runner, if one is online (§7.6).
2. The triggering member's remote endpoint and key (Settings → Model).
3. The project key, if an admin attached one and its budget remains.
4. If none of these apply, Galley refuses the run with "Connect a model in Settings, start your runner, or ask an admin to attach a project key."

A member's key is never used by anyone else, never leaves the server, is encrypted at rest, and never
appears in logs or run records.

**Project key.** An admin may attach one key to the project with a monthly cap and an optional
per-member share. This is how a lab lets students run agents without each buying a key. Members see
`runs on the project key, 120 of 500 left this month`.

**Concurrency is safe by construction.** `propose_patch` is the only write path, so two members
running agents at once produce two *proposals*, never two writes. Proposals land in the same anchored
suggestion store as human suggestions (§4), so they track concurrent typing. A user accepts or rejects
them one at a time. There is no agent-versus-agent conflict to resolve.

**Attribution.** Every proposal carries the agent name, the member who triggered it, the backend and
model, tokens, and duration. Accepting applies the edit as that member's own CRDT edit and tags the
commit `via galley:<agent>`. The run log lives in `.galley/agents/<run>.jsonl` and is listed in History.

**Shared conventions.** `AGENTS.md` at the project root is the cross-tool convention. It is
version-controlled and editable in Galley itself, and `.galley/agent.md` is also accepted. Galley
prepends it to the MCP instructions and to every built-in prompt: preferred spelling, citation
command, tone, "never touch §4". It makes several members' agents behave like one collaborator.

**Provenance and disclosure.** Every automated edit is an accepted diff attributed to a person and a
model, so Galley can produce an AI-assistance report for a submission: which passages came from an
agent, who accepted them, and with which model. More venues now require that disclosure. An editor
that applies AI edits invisibly cannot produce such a report.

**Off switch.** An admin can set `agents = off` for a project. Some labs and venues require it. Galley
records the change in the audit log.

### 7.6 Local agent runner (bring your own compute). V1, shipped as `galley agent run`

A member runs the model on their own machine without a client, a port, or a TeX installation:

```
galley agent run --url https://host/mcp/<project> --token <device-token> \
                 --model qwen2.5-coder:14b --endpoint http://localhost:11434/v1 --prompt fix-build
```

The `galley` binary is the runner. The Share panel prints this command next to the MCP one. What it does:

1. **Checks the project out** through the project's MCP endpoint (§7.7): `list_files`, then `read_file`
   for every text file, into a temporary directory. The live documents are the base, so the run sees
   exactly what the authors see, unsaved edits included.
2. **Builds the context itself.** The model gets a short system prompt (how the copy works, the file
   list, the last build log) and the task from `prompts/get` with `surface: checkout`, which phrases
   the built-in tasks for file editing and already carries `AGENTS.md`. It gets nothing else: no client
   boilerplate and no forty foreign tools. This lets a small local model complete the loop.
3. **Lets the model edit the copy** with the tools every model is trained on: `read_file`,
   `edit_file` (a unique find and replace), `write_file`, and `done(summary)`. A missing `path` means
   the main file. A bare file name resolves against the checkout. The runner nudges once if the model
   ends a turn without editing.
4. **Diffs the copy against the base** and submits each changed file through `propose_patch`, with the
   line-level hunks grown by whole lines of context until each `find` is unique (`galley-agents::patch`).
   The proposals arrive as anchored suggestions attributed "Name · via <task>", with the model recorded.

The model talks to an OpenAI-compatible endpoint through one code path: Ollama, LM Studio, llama.cpp,
vLLM, or a hosted provider with `--api-key`. Measured: a 4B Qwen over Ollama finishes fix-build in
three turns (read, edit, done) and one proposal. The same model never reached `propose_patch` through a
generic coding client (§7.7, client notes).

What this gives, and why it is the preferred local path:

- **No inbound exposure.** The runner dials out, so the member never opens a port, forwards localhost,
  or runs a tunnel.
- **No model vendor sees the manuscript** unless the member points `--endpoint` at one.
- **Zero marginal inference cost to Galley, permanently.** This lets agent runs stay unlimited
  for every user, without becoming a loss leader.
- **`compile()` still runs server-side** in the sandbox, so the runner needs no TeX. The authors' next
  build verifies the run's edits after they accept them, the same as any suggestion.
- **Safe when the model is weak.** A run that edits nothing says so and proposes nothing. It can
  never half-apply.

Post-V1: a long-lived `galley agent connect` that long-polls for jobs triggered from the web UI on
any device, so a member's laptop serves their whole team's runs. Same checkout-and-diff core.

Security: the device token is scoped to that member's own project access, is listed per device, and is
individually revocable. A runner can never write directly. `propose_patch` remains the only write path,
so a compromised runner can propose but not commit. Runs are rate-limited and logged like any other.

### 7.7 MCP server: bring your own AI client

Galley exposes the §7.1 tool set as an **MCP server**, so a member can drive their project from Claude
Desktop, Claude Code, Cursor or any MCP client instead of the in-app Agents panel. The tools are the
same: `list_files`, `read_file`, `search`, `read_log`, `compile`, `lookup_citation` and `propose_patch`.
A scoped, revocable per-client token authenticates them, exactly like a runner (§7.6).

The built-in agents of §7.3 ship as **MCP prompts** (`prompts/list`, `prompts/get`): fix-build, proofread,
tighten, cite, table, reviewer, explain. Each is a task description that uses the tools above, so any client
runs them with its own model. The reviewer uses a `comment` tool, which is anchored and never edits.

A `propose_patch` arriving over MCP produces the **same anchored suggestion** as an in-app run:
reviewable per hunk, attributed to that member and model, accepted into the CRDT as their own edit.

**Why we ship this.** Every project is a real git repository that any member can clone, so
a determined user can already run their own model over their own checkout and push the result back.
That door stays open. It is a founding promise, so an anti-circumvention posture would not work.
Shipping MCP officially is better for everyone: proposals land in the review surface as suggestions
with provenance rather than as opaque commits, co-authors keep their accept and reject workflow, and
Galley stays the system of record.

**Client notes, from testing real clients against this server** (2026-09-17):

- *Claude Code* (`claude mcp add --transport http galley <url> --header "Authorization: Bearer <token>"`)
  completes the fix-build prompt end to end: reads the log, proposes one edit with both missing
  packages, verifies with one compile, and notices when a compile cannot reflect its own unaccepted
  proposal. A run costs a few cents on the user's own key.
- *OpenCode* connects and lists all seven tools, but it exposes **no** MCP tools to a model
  unless the model declares tool calling, and it gives no warning. For a custom Ollama provider that means
  `"provider": {"ollama": {"models": {"<model>": {"tool_call": true}}}}` in `opencode.json`, and the
  file must sit at a **git repository root** to be read as project config.
- *Small local models* (a 4B Qwen via Ollama) reach the tools and diagnose correctly, then end their
  turn narrating the next step instead of calling it, even when given the exact call sequence. Through
  a generic client the loop needs a tool-capable model of roughly 7–14B or larger. The same 4B model
  completes the task through `galley agent run` (§7.6), whose context is only the task and the
  project. Recommend the runner for local models and the MCP path for hosted clients.
- The official `@modelcontextprotocol/sdk` client accepts the hand-rolled endpoint as it is (initialize,
  tools, prompts, ping, and a traversal attempt correctly refused).

**What the plan gate can and cannot do:**

- *Enforceable:* the server refusing to execute a server-side agent run for a plan that lacks it.
- *Not enforceable:* a client that reads a project (over the API or a clone), runs a model on the user's
  own machine, and posts the result through the ordinary suggestion endpoint. Galley cannot tell that
  apart from a fast human collaborator, and we will not build heuristics to guess.

Therefore, treat the agent layer as an integration with the model you already pay for, never as access to a model.

**Licensing.** The AGPL protects against a closed hosted fork of Galley itself. Anyone running a
modified Galley as a service must publish their changes. It does not prevent a third party from
publishing a permissively licensed client against our public endpoints, and it is not meant to.
Self-hosters get the complete feature set by design. That is a stated principle, not a leak to plug.

---

## 8. UI specification

### 8.1 Layout (desktop, ≥1100 px)
```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│ ▣ galley   Thesis ▾      ●●● 3 online          ⌘K Search/commands     🔗 Share   ⚙   👤 │  top bar 44px
├───┬──────────────────────────────────────────┬──────────────────────────────────────────┤
│ 📄│ main.tex  ×  sections/2.tex ×            │ ▸ Preview   ◐ Draft   ⇄ Sync   ⤓   1/12   │
│ ☰ │  38  \section{Method}                    │                                          │
│ 📚│  39  We ~cite~ …                         │            [ PDF.js canvas ]             │
│ 💬│  40  \begin{figure}                      │                                          │
│ ⏱ │  41  ▌\includegraphics{fig1}  ← Ana      │                                          │
│ ✨│  42                                      │                                          │
│   │                                          │                                          │
│   │   [gutter: git diff · errors · comments] │                                          │
├───┴──────────────────────────────────────────┴──────────────────────────────────────────┤
│ ● Compiled 2.1 s · 0 errors · 2 warnings ▴    Tectonic · main.tex   Auto ▾   ⌘⏎ Build   │  build bar 32px
└─────────────────────────────────────────────────────────────────────────────────────────┘
```
- **Rail** (48 px, icons): Files, Outline, Bibliography, Comments, History, Agents. Clicking opens a **drawer** (280 px, resizable, collapsible). Only one drawer opens at a time.
- **Editor** and **Preview** are a resizable split. The preview can be popped out to a separate window (for two monitors) or hidden.
- **Build bar** is the single source of build truth. Clicking the status expands the **Problems drawer** (bottom, 220 px) with error cards.
- **Command palette** (`Ctrl/Cmd+K`): files, commands, symbols (sections, labels, bib keys), recent checkpoints. Experienced users do everything from here.

### 8.2 Screens
1. **Sign in**: email and password, or "Continue with <OIDC provider>", or a share-link landing that asks only for a display name.
2. **Projects**: a card grid with title, last edited, collaborators' avatars and a build status dot. Buttons: *New project* (blank, template, import zip or clone git). Search. Archive.
3. **Editor**: see above.
4. **History** (drawer and full-screen mode): a vertical timeline of checkpoints with auto-commits collapsed, a scrubber, a file diff, *Compare PDFs*, *Restore*, *Create checkpoint* and *Push to remote*.
5. **Share** (modal): a members list with roles, a link generator (role, expiry, label), a copy button and an "Anyone with link" toggle.
6. **Settings**: Project has engine, main file, build options, `.galleyignore` and remote. User has theme, keymap (default, Vim or Emacs), font size and editor options. Admin has users, OIDC, agent backends, limits and backups.

### 8.2.1 Files drawer: file operations
- Each row has a ⋯ menu: Open (text) or View (PDF and raster images, inline in a new tab), Download,
  Rename or move…, Set as main file (`.tex`), Delete. Editors only for the last three; the main file
  cannot be deleted until another is set, and renaming it moves the setting.
- Upload from the header button, or by dropping files or whole folders on the list. Galley keeps the
  folder structure and skips desktop metadata files. The limit is 25 MB per file. An existing path
  asks before it is replaced.
- Every operation is a named commit (`upload:`, `replace:`, `rename: a → b`, `delete:`), so History can
  bring anything back. A restore refreshes everyone's file list.
- **Deleted text stays deleted.** Galley empties a deleted text file's CRDT and never discards it. A
  browser holding its history offline then merges into an empty document instead of reviving it, and a
  new file of the same name starts clean. Flushes skip documents whose file no longer exists, and the
  document socket refuses them, so a stale editor cannot recreate a file.
- Downloads serve text as the live document. Only PDF and raster images are ever inline. Every
  file response carries `nosniff` and a sandbox CSP.

### 8.2.2 Bibliography manager and citation graph
- The drawer lists every entry: key, title, year, venue, the sections that cite it, and a tag for each
  audit finding (duplicate, incomplete, preprint, uncited). Keys cited with no entry are called out at
  the top, because LaTeX prints `[?]` for them.
- **Adding**: paste a DOI, an arXiv id, either one's URL, or BibTeX itself. Galley looks an identifier
  up once. That single call needs no opt-in, because the author asked for it by pasting the identifier
  Galley generates the key from the author and year and keeps it unique in the file.
- **Editing**: a row expands into the fields, and saving rewrites only that entry. Comments, `@string`
  macros, other entries and fields Galley does not know stay exactly as they were.
- **Fixing what the audit found**: *Merge into X* points every `\cite` of a duplicate at the entry to
  keep and removes the other entry. *Check the catalogues* offers the fields that Crossref or arXiv
  hold and the entry does not, to apply or ignore. *Delete* is refused while the document still cites
  the entry, unless the author insists. Each is one named commit (`bib: merge a → b`), so History can
  undo it.
- **Graph** opens a full-screen canvas: a node per entry (sized by how many sections cite it, hollow
  when nothing cites it, outlined when the audit found something), and an edge between works cited in
  the same section, weighted by how many sections do that. Clusters are the author's own groupings.
  No catalogue and no extra dependency is needed. The layout is a few hundred ticks of force
  simulation, fitted to the canvas. Hover names the work. Clicking one opens its entry.
- With catalogue lookups on (§13 / docs/RETRIEVAL.md §5), **Find work you may be missing** adds dashed
  candidate nodes joined to the references that cite them. Clicking a candidate opens its DOI. They are
  candidates, never insertions.

### 8.3 Editor behavior
- Syntax highlighting, bracket and environment matching, auto-close of `\begin{}` with `\end{}`, and `\ref{` and `\cite{` autocompletion sourced live from the project.
- Inline math preview on hover (KaTeX). Section folding. The outline follows the cursor.
- Gutter: git change marks (added and modified), error and warning markers, comment pins.
- Collaborator cursors with name flags that fade after 3 s of inactivity. **Quiet mode** (`Ctrl+Shift+Q`) freezes incoming remote edits visually until the user turns it off. They still sync.
- Suggestion mode toggle (`Ctrl+Shift+S`): edits become tracked insertions and deletions, colored per author, accepted or rejected inline.
- Dropping an image uploads it to `figures/` and inserts a figure environment snippet with a placeholder caption.
- Pasting a DOI or arXiv URL anywhere shows a toast "Add to bibliography?" that takes one click.

### 8.4 Preview behavior
- The preview keeps the last good PDF when a build fails. The failed state is a red stripe over the build bar, and the pane never goes blank.
- Clicking in the PDF jumps the editor to that place (SyncTeX). `Ctrl+click` in the editor jumps the PDF.
- Zoom presets, page-fit, dark-invert mode, text search, download.

### 8.5 Keyboard (default keymap)
| Action | Keys |
|---|---|
| Build | `Ctrl+Enter` |
| Command palette | `Ctrl+K` |
| Toggle preview | `Ctrl+Shift+P` |
| Toggle drawer | `Ctrl+B` |
| Problems | `Ctrl+Shift+M` |
| Agent menu on selection | `Ctrl+.` |
| Suggestion mode | `Ctrl+Shift+S` |
| Quiet mode | `Ctrl+Shift+Q` |
| Create checkpoint | `Ctrl+Shift+C` |
| Go to section/label/bib | `Ctrl+T` |

### 8.6 Empty states & onboarding
- First launch: "Create your admin account" (name, email, password), then "Create a project" with three choices: *Blank article*, *From template*, *Import Overleaf zip*. There is no tour. The Projects screen shows a dismissible 5-item checklist (build, share, checkpoint, add a citation, try an agent).
- Every drawer has a one-line empty state with a single action ("No comments yet. Select text and press C.").

### 8.7 Responsive
- Below 1100 px the preview becomes a tab. Below 700 px (phone) the user gets a read-only editor, comments and the PDF, with a "Desktop editing only" note in the editor. Build and share still work.

### 8.8 Accessibility
- Full keyboard operability. ARIA on drawers and dialogs. Focus rings in Galley Red. Contrast of 4.5:1 or better in both themes. Reduced motion respected. The PDF text layer is exposed to screen readers.

---

## 9. Install & deployment

### 9.1 One-liner (Ubuntu)
```bash
curl -fsSL https://galley.dev/install.sh | sh
```
The script:
1. Detects arch, downloads the static binary to `/usr/local/bin/galley` (or `~/.local/bin` if not root).
2. Installs `bubblewrap` via apt if missing (asks first); offers a JRE for optional LanguageTool.
3. Creates `~/.galley/galley.toml` with default settings.
4. Optionally installs a `systemd --user` service (`galley.service`) and enables lingering.
5. Runs `galley doctor` and prints: `Run: galley serve   → http://localhost:7000`

### 9.2 CLI
```
galley serve [--config PATH] [--domain tex.example.org] [--port 7000]
galley admin create-user <email>           galley admin reset-password <email>
galley project import <zip|git-url> [--name]   galley project export <id> --zip
galley backup [--to PATH]   galley restore <archive>
galley doctor          # checks bwrap, tectonic, disk, ports, TLS
galley engine install tectonic|texlive     galley agents test
```

### 9.3 `galley.toml` (defaults)
```toml
[server]
bind = "0.0.0.0:7000"
domain = ""              # set to enable auto-HTTPS (ACME) on :443
data_dir = "~/.galley"
public_signup = false

[build]
engine = "tectonic"      # or "texlive"
texlive_path = ""
timeout_s = 120
memory_mb = 2048
auto_build = true
sandbox = "auto"         # bwrap | docker | none

[sync]
flush_quiet_ms = 4000
flush_max_ms = 60000

[agents]
enabled = true
default_backend = "ollama"
daily_token_budget = 2_000_000

[grammar]
languagetool = "off"     # off | auto (a LanguageTool on this machine, port 8081) | url
language = "auto"        # a LanguageTool code such as en-US, or auto
```

### 9.4 Bundled templates
Six templates are compiled into the binary, so a fresh install has them offline: **blank article**,
**conference paper** (two columns, results table, cross-references), **preprint** (theorems, appendix,
arXiv venue preset applied), **talk** (Beamer, 16:9), **thesis or report** (chapters in their own
files), and **response to reviewers**. `GET /api/templates` lists them and `POST /api/projects` takes a
`template` id. No venue class files ship with Galley. `neurips.sty` and its peers are the venue's to
distribute, and each paper template names the class to swap in.

### 9.5 Grammar (optional)
`[grammar] languagetool` is `off`, `auto` (a LanguageTool already running on this machine, port 8081)
or a URL. A check runs on demand from the Problems panel, one file at a time, and never during a
build. Galley masks LaTeX out by replacing it with spaces, one per character, so an offset that comes
back still points at the same character of the source. A match becomes a lint diagnostic with the
flagged span and a one-click replacement. With no server the button says the check is unavailable.
Document text reaches whatever server is configured, so the default is off.

### 9.6 Docker Compose
```yaml
services:
  galley:
    image: ghcr.io/galley/galley:1
    ports: ["7000:7000"]
    volumes: ["galley-data:/data"]
    environment: [GALLEY_DOMAIN=tex.example.org]
    cap_add: [SYS_ADMIN]        # only if using bwrap inside the container; otherwise sandbox=docker-socket
volumes: { galley-data: {} }
```

### 9.7 Backups
`galley backup` writes a tar of `data/` and `galley.db` (SQLite online backup API). An optional nightly systemd timer can send it to an `rclone` target. Every project is a git repo, so `origin` mirrors are a second backup at no extra cost.

### 9.8 Updates
`galley self-update` checks the signed release, swaps the binary and restarts the service. SQLite migrations run on start. Git repos never need migration.

---

## 10. Security
- Argon2id passwords. Sessions in HttpOnly SameSite cookies. CSRF on state-changing routes.
- Share tokens are 32-byte random, role-scoped and revocable, with an optional expiry.
- Sandbox: no network, no host filesystem, PID, UTS and IPC namespaces, seccomp (bwrap default), cgroup memory and CPU caps, and a kill on timeout.
- Uploads: type-sniffed, size-capped (50 MB default), stored content-addressed.
- Agents: allowlisted egress to the model endpoint and the citation resolvers, redaction of API keys from logs, and per-run budgets.
- Rate limits on auth and on the share-link landing. An audit log for admin actions and history restores.

---

## 11. Repository layout & milestones

### 11.1 Repo
```
galley/
  crates/
    galley-server/     axum app, routes, auth, ws
    galley-sync/       yrs docs, persistence, flusher
    galley-history/    git2 wrapper, checkpoints, diffs
    galley-build/      sandbox, engines, log parser, hints db
    galley-agents/     runner, tools, backends, prompts
    galley-cli/        commands, installer helpers
  web/                 Vite + TS app (CodeMirror, PDF.js, design system)
  templates/           bundled project templates
  hints/               error-hint YAML (community-editable)
  deploy/              install.sh, systemd, docker, fly.toml
  docs/
```

### 11.2 Milestones
| # | Milestone | Done when |
|---|---|---|
| M1 | Skeleton | `galley serve` opens a project, edits sync between two tabs, text lands in git |
| M2 | Build | Tectonic in bwrap, PDF preview swaps, SyncTeX both ways, error cards with hints, build profiler |
| M3 | Collaboration | roles, share links, comments, suggestion mode, presence, offline PWA |
| M4 | History | checkpoints, scrubber, diffs, restore, latexdiff PDF compare, remotes, `galley clone` and `galley push` |
| M5 | Agents | the runner with fix-build, proofread, cite, table and review, Ollama and Anthropic backends, review cards |
| M6 | QoL | §13: snippets with auto-labels, rename label, lint, tasks, targets/deadline, submission packer + compliance, figure cache |
| M7 | Cloud | accounts, project states (§14), budgets with visible counters |
| M8 | Polish | bib manager, LanguageTool, templates, installer, docs, `galley doctor`, 1.0 |

Estimate: about 5–7 months for one experienced full-stack engineer to reach 1.0. M1 and M2 are usable for solo work at week 4.

---

## 12. Risks & decisions log
- **Tectonic vs TeX Live**: Tectonic is better on install size and reproducibility. Some packages need TeX Live, such as rare fonts and `minted` with shell-escape. Ship both paths and default to Tectonic.
- **One Yjs doc per file vs per project**: one doc per file keeps memory bounded and lets large projects load lazily. Decided: per file.
- **CRDT re-seed on git pull** is the only operation that can feel lossy. Mitigations: an explicit confirmation, an automatic checkpoint before it, and a warning banner to online collaborators.
- **LanguageTool needs Java**: keep it optional. Offer it during install. Never block the first run on it.
- **Serverless**: true FaaS was rejected because of stateful WebSockets and long compiles. Galley expects a host that keeps a process and a disk.
- **Name**: verify the trademark and the domain before public release. Fallback candidates: *Quire*, *Recto*, *Folio*.

---

## 13. Quality-of-life features (added after prototype review)

All ten are implemented in the interactive prototype (`mockup/galley-prototype.html`). Use it as the behavioral reference.

### 13.1 Persistent figure cache
How it works, as built (M6):
- **Mechanism.** TikZ's `external` library in `mode=list and make` with `up to date check=md5`,
  loaded through a wrapper (`.galley/build/galley-fig.tex`) with `\AddToHook{package/tikz/after}`, so
  the user's source stays untouched and SyncTeX still points at it. There is no shell escape. Galley
  compiles each listed picture itself as `galley-fig-figure<N>.tex`
  (`\def\tikzexternalrealjob{galley-fig}\input{galley-fig}`), up to four at a time, into
  `.galley/build/galley-fig-figure<N>.pdf`. It does not use `figures/`, because Tectonic takes the job
  name from the file name, so the figures must sit beside the wrapper.
- **Staleness is Galley's job.** TikZ includes a figure PDF whenever it exists, even after the picture
  changed. It only rewrites the figure's `.md5`. Galley records the md5 each PDF was built from
  (`figcache.json`) and treats a mismatch as stale. A **project key** covers what the md5 cannot see:
  the main preamble, every file that is not a `.tex` or `.bib` file (styles, data, images) and the
  engine binary. Galley drops every figure when that key changes. Missing figures make the pass fail
  with "File not found", so Galley publishes a pass only when every figure is present and current.
- **Each build.** The wrapper pass runs first. If all figures are current, Galley publishes it. If some
  are stale, Galley regenerates them now when the timings of earlier builds say that beats a plain
  compile, or, before any timings exist, when no more than one round is needed. Otherwise Galley does a
  plain compile and caches the rest in the background after the build, yielding to any new build
  request between rounds.
- **Not used** for draft builds, the very first build on a host, beamer, projects that configure
  externalization themselves, and pictures that use `remember picture` or `overlay` or read files with
  `\input`. The build reports why. Galley does not retry a picture that fails on its own until its code
  changes.
- **Measured** (20 pgfplots, 12 pages, docker sandbox): plain 6.6–7.7 s, cached 1.3–2.5 s, one or two
  changed figures 5.7–5.9 s. A cold or invalidated cache costs one extra pass of about 1.9 s, then warms
  in about 9 s in the background.
- Galley never garbage-collects the cache by time. "Clear figure cache" (palette, Problems) wipes it.
  A project can turn it off (`figure_cache` in `.galley/project.toml`, the Problems toggle).
- A server may set a retention period for the cache after a project goes idle.
- Draft mode does not yet skip figures entirely; it compiles without the cache.

### 13.1.1 SVG figures without Inkscape
- `\includesvg` (the `svg` package) works unchanged. The package normally shells out to Inkscape;
  with shell escape off it uses files already converted into `svg-inkscape/`. Before every build
  Galley converts each changed `.svg` in-process (svg2pdf) into `.galley/svg/svg-inkscape/` as
  `<name>_svg-tex.pdf` with its `.pdf_tex` overlay and `<name>_svg-raw.pdf` (for `inkscapelatex=false`),
  and adds `.galley/svg` to the compile search path. Outputs of deleted SVGs are removed.
- Galley draws text inside an SVG with the host's fonts, and the image ships DejaVu. It does not use
  the document's font, as a PDF export would. It does not reproduce Inkscape's text-as-LaTeX overlay.
- An SVG never reads other files. Galley honours only images embedded as data URLs. Files over 20 MB
  and SVGs that fail to parse become a build warning with a hint. Galley reports two SVGs with the same
  file name in different folders, because the package names outputs by file name.
- The packer ships each `\includesvg` source with its `svg-inkscape/` files, which is what arXiv expects.
- The package's "Shell escape disabled" and `transparent` notices are shown as info, not warnings.

### 13.2 Compile profiler
- The build runner records wall time for these phases: parse, packages, figures, BibTeX or biber, and output. It stores them in `builds.profile_json`.
- UI: a small stacked bar in the build bar, which opens Problems when clicked. The Problems drawer shows the full breakdown with seconds per phase, the cache toggle, and a one-line explanation of the figure phase.

### 13.3 Snippets with auto-labels
- Trigger words plus Tab in the editor: `fig`, `tab`, `eq`, `sec`, `sub`, `cite`, `todo`. The expansion places the cursor in the caption or the heading.
- `fig`, `tab`, `sec` and `sub` insert `\label{prefix:} % auto`. While the trailing `% auto` comment is present, Galley derives the label slug on every edit from the nearest preceding `\caption{}` (figures and tables) or `\section{}` or `\subsection{}` (sections). The slug is lowercase ASCII words, joined by hyphens, up to 4 words. Deleting the comment pins the label.
- Auto-label rewrites never move the user's cursor, because the label always comes after the caption or the heading.

### 13.4 Rename label
- Use `Ctrl+Shift+R`, the command palette, or the Outline drawer. Galley prompts for the old and the new name, and defaults to the label under the cursor. It rewrites `\label`, `\ref`, `\eqref`, `\autoref` and `\cref` across all `.tex` files, reports the count, and creates a commit `rename label a → b`.

### 13.5 Lint
- A third severity, `lint`, never affects build status. Problems renders it as a separate group. The gutter uses a neutral dot.
- v1 rules: unused label, uncited bib entry, doubled word (with a fix), breakable "Section \ref" (fix: insert `~`), and overlong bold run. The rules live in `crates/galley-build/src/lint/` and each one can be turned off in project settings.

### 13.6 Tasks
- Galley collects `% TODO(@name): text`, `% FIXME(...)` and `\todo{@name text}` (todonotes) from all `.tex` files into a Tasks drawer with an assignee filter, Go to, and Done. Done removes the note from the source and commits.
- A rail badge shows when tasks exist. Open TODOs are a compliance check (§13.8).

### 13.7 Targets and deadline
- The Outline shows words per section with LaTeX commands stripped, and an optional per-section budget with a progress bar that turns red when the section is over budget. Galley stores budgets per project in `settings`.
- A deadline pill in the top bar counts down. It is amber at 7 days or fewer and red at 3 or fewer. The deadline and the venue are project settings.

### 13.8 Submission: compliance meter and packer
- Venue presets (`templates/venues/*.toml`) hold: name, page limit, required font size, anonymous review, and engine (TeX Live version). v1 ships arXiv, NeurIPS, ACL and IEEE TPAMI. Users can edit the presets.
- Compliance checks: estimated pages against the limit, font size (one-click fix), anonymization (author surnames from `\author` found in the body, and first-person references to prior work), open TODOs, errors and warnings from the last build, and a present bibliography.
- The packer flattens `\input` and `\include`, strips comments, keeps only referenced figures and cited bib entries, generates `.bbl`, externalizes TikZ to PDFs, writes `00README.XXX`, and compiles the result in a fresh sandbox with no cache. Galley disables the download if that build fails. A modal lists what was included and what was left out, with reasons.
- "Freeze as submitted" creates a checkpoint `Submitted to <venue>` and records it as `projects.submitted_checkpoint`. History then offers a latexdiff against it, which is the camera-ready diff.

### 13.9 Local-first CLI
- `galley clone <url>` produces a normal git repo with a `.galley/remote` file. `galley push` sends commits to the server. The server merges them into the CRDT with a three-way merge against the last synced state, and records a commit attributed to the CLI user. Online editors see the change arrive as a remote edit, and they get no conflicts by construction. Conflicts on the CLI side appear as ordinary git conflicts before the push.
- The Share modal shows both commands with copy buttons.

### 13.10 Reviewer agent and suggestion "Go to"
- The Reviewer agent produces anchored comments only. It never edits. Suggestion and comment cards both have Go to, and either Accept and Reject or Resolve.

---

## 14. Project states

| State | Edit | Build | Agents | Read / export / clone | Counts toward a project limit | Storage counts |
|---|---|---|---|---|---|---|
| Active | ✓ | ✓ | ✓ | ✓ | ✓ (owner only) | ✓ |
| Archived | – | – | – | ✓ | – | ✓ |

- Archiving keeps everything: files, history, comments, checkpoints, the last PDF, share links and roles. It only makes the project read-only.
- Collaborators on an archived project see a banner that names who archived it and when, with "Export" and "Copy to my projects". The copy creates their own active project from the snapshot.
- Reactivating takes one click. Where a server limits active projects, the dialog offers a swap: archive another project and reactivate this one.
- Only owned projects count toward such a limit. Being a collaborator never consumes a slot.
- Galley suggests archiving after 60 days without edits. It never archives a project on its own.
- Archived projects are excluded from build budgets and agent runs.
- Audit log records archive/unarchive with actor and timestamp.

