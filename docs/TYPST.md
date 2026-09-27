# Typst as a second engine: what it takes

Status: scoping, 2026-09-25. Nothing is built. Facts about Typst were checked against its own
documentation on that date, against Typst 0.15.1.

## 1. Why Typst fits the existing design

- It is plain text, so CRDT sync, git history, diffs, checkpoints, comments, suggestions, share
  links and roles all work unchanged. Only the file extension is new.
- It reads BibLaTeX `.bib` files as well as Hayagriva YAML, so the bibliography manager, the
  bibliography audit and the citation graph keep their input format.
- It downloads packages on demand into a cache and then compiles offline, which is the same shape
  as the Tectonic bundle. The existing offline-then-fetch retry applies without redesign.
- It compiles in milliseconds and needs no 300 MB TeX installation.

## 2. What Typst gives us, checked

| Need | Typst 0.15.1 | Source |
|---|---|---|
| Single static binary per platform | Yes, from GitHub releases | typst/typst |
| Licence | Apache-2.0 | typst/typst |
| Diagnostics for machines | `--diagnostic-format` accepts `human` or `short`. No JSON yet. | typst-compile(1) |
| Package cache control | `--package-cache-path`, `TYPST_PACKAGE_CACHE_PATH`, plus `--package-path` for local packages | typst-compile(1) |
| Offline once cached | Yes. An already downloaded package needs no network. | typst issue 4441 thread |
| Font control | `--font-path`, `--ignore-system-fonts`, `--ignore-embedded-fonts` | typst-compile(1) |
| Project root | `--root`, or `TYPST_ROOT` | typst-compile(1) |
| Output formats | pdf, png, svg, html (experimental), bundle | typst-compile(1) |
| PDF conformance | `--pdf-standard`, including PDF/A and PDF/UA variants | typst-compile(1) |
| Bibliography input | BibLaTeX `.bib` and Hayagriva YAML; 80+ built-in styles and CSL files | typst docs, bibliography |
| Source mapping for click-to-source | Not exposed by the CLI. tinymist and typst-preview do it through the compiler API and a span interner. | tinymist docs |

## 3. Work, by layer

### 3.1 The engine abstraction does not exist yet

`engine.rs` defines a concrete `Tectonic` struct, and `runner.rs` holds `RwLock<Option<Tectonic>>`
and calls it directly. The spec claims the engine interface is generic. It is not. This is the first
piece of work: a trait covering locate, install, the compile spec, cache location and the
cold-cache test, with Tectonic as the first implementor. Everything else depends on it.

### 3.2 Diagnostics: easier than LaTeX, but new code

`log.rs` is 333 lines of TeX log parsing, and `hints/latex.yaml` holds 39 hint entries. None of it
transfers. Typst's `short` format is one diagnostic per line with file, line, column, severity and
message, which is far simpler to parse than a TeX log. A `hints/typst.yaml` starts small, since
Typst's own messages are already written for humans. The error card UI, the fix mechanism and the
lint severity model are unchanged.

### 3.3 Click-to-source has no CLI answer

SyncTeX is 252 lines and is specific to TeX. Typst exposes spans only through its Rust API, which
tinymist uses for two-way jumps. Three options:

1. Ship Typst as a downloaded binary and leave click-to-source out for Typst in the first release.
2. Depend on the `typst` crate and compile in process, which gives spans directly. It removes the
   child process, so it also removes the sandbox for this engine, and it ties Galley's build to the
   compiler's version. Typst has no shell escape and no arbitrary code execution, so the risk is
   smaller than with TeX, but it is a real architectural change and contradicts the rule that every
   engine runs as a sandboxed child.
3. Run tinymist as the helper for previews later, once the basics work.

Recommendation: option 1 for the first release, and revisit option 2 only with a written decision.

### 3.4 The paper index needs a Typst parser

`parse.rs` is 759 lines of line-oriented LaTeX scanning. Typst's structure is explicit and at least
as easy to scan: `=` headings, `<label>` labels, `@ref` references, `#figure(caption: ...)`,
`#include "file.typ"`, `#bibliography("refs.bib")`. The `Paper` model, the ranking, the BM25 search,
the citation graph and the bibliography audit all sit above the parser and should not change.

`math.rs` is 601 lines that parse TeX math into operator trees. Typst math is a different syntax, so
formula search does not transfer without a second front end. It can wait.

### 3.5 Things that stay LaTeX-only at first

`figures.rs`, the figure cache, is built around TikZ compiled through a generated wrapper. Typst
draws natively and compiles fast, so the cache matters less. `pack.rs`, the submission packer, is
tied to what venues accept, and most venues still require LaTeX. Both stay as they are.

### 3.6 Web app

- `paths.rs` needs `typ` in `TEXT_EXTENSIONS`.
- There is no CodeMirror 6 Typst language package from the CodeMirror project, and none found on
  npm. Either write a small Lezer grammar, or start with a stream-based highlighter good enough for
  headings, functions, strings and math. `web/src/editor/latex.ts` is only 41 lines, so the outline
  and word-count helpers are cheap to mirror.
- Snippets, auto-labels and the rename-label command are LaTeX-shaped and need Typst equivalents.

## 4. Order of work

1. Introduce the engine trait and move Tectonic behind it. No behaviour change.
2. Install and locate the Typst binary, mirroring `install.rs`, with the cache under the data dir.
3. Compile a Typst project in the sandbox, offline first, then the fetch pass on a missing package.
4. Parse `short` diagnostics into the existing error cards, plus a small `hints/typst.yaml`.
5. Add `typ` to the text extensions, highlight it in the editor, and add the outline helper.
6. Add a Typst structure parser to `galley-index` behind the existing `Paper` model.
7. Templates: a Typst starter for each existing template that makes sense.
8. Leave out for now: click-to-source, formula search, the figure cache and the packer.

## 5. Open questions

- Does the demo audience want Typst, or is this for the 1.0 story? Ask before step 1.
- Do we pin a Typst version per project, the way a TeX distribution is pinned? Typst is young and
  its syntax still changes, so a project probably records the version it compiles with.
- Does `galley doctor` need to check both engines, and what does it say when only one is present?
