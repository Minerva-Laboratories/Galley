# Retrieval, citations and the literature layer

Status: exploration, 2026-09-17. Nothing here is built. This covers what PaperQA2 can and cannot do for
Galley, what a LaTeX-aware "paper map" would be (aider's repo map, for papers), a citation graph, math
search, and which of it is a Galley feature and which is a separate product.

## 1. The premise

LaTeX carries structure that code editors have to infer and PDF pipelines throw away: sections,
labels, `\ref`, `\cite`, theorem environments, captions, a bibliography with keys. Retrieval built on
that structure should beat generic chunk-and-embed on the same document, and it costs no model at all.
Today Galley's agents get a regex `search` and a whole-file `read_file`. This is the weakest part of M5.

## 2. PaperQA2: useful, but not as a dependency

What it is (verified 2026-09-17): an Apache-2.0 Python library and `pqa` CLI from FutureHouse, v5
(calendar-versioned since `v2025.12.17`), for RAG over PDFs and text with a scientific focus. It parses
documents, embeds chunks, retrieves with citations, and enriches metadata through Semantic Scholar,
Crossref and Unpaywall. It defaults to OpenAI models through LiteLLM, and can run fully local against
Ollama or llamafile.

Why it cannot go inside Galley:

- **Python.** Galley adds no new runtime dependency. The binary ships alone.
- **It needs a model.** V1 buys no inference. Every agent runs on the member's machine.
- **It is a PDF-first pipeline.** Our source of truth is the live LaTeX, not a rendered PDF.

Where it fits instead:

- **As a client.** PaperQA2 driving a Galley project through MCP is the "bring your own AI
  client" story (SPEC §7.7). It reads files, and we take proposals. A worked example in the docs costs
  us one page and nothing else.
- **As a reference implementation.** Its retrieval-with-citations design is worth copying in the parts
  we build natively.
- **In a side product** (§7), where a Python service is allowed because it is not the editor.

## 3. The paper map (what aider's repo map is for code)

Aider builds a repo map by extracting definitions with tree-sitter, modelling files and symbols as a
graph, ranking it with a PageRank-style algorithm, and emitting the highest-ranked parts within a token
budget. The equivalent for a paper is easier than for code, because LaTeX names its own structure.

**Nodes:** files, sections and subsections, labelled objects (figures, tables, equations, theorems),
bibliography entries. **Edges:** `\input` (file to file), `\ref`/`\cref`/`\eqref` (section to label),
`\cite` (section to bib entry), containment (file to section to object).

**Ranking:** personalise the walk on what the agent is working on. That means the open file, the
section at the cursor, and the sections named in the task. Then "what else touches `tab:main`" or
"which sections cite `openvla`" comes out ranked instead of as 40 regex hits.

**Output:** a compact text map, budgeted, e.g.

```
main.tex  (8 sections, 6.1k words)
  §4 Experimental Results [sec:main] 1.9k words
     tab:main (LIBERO success by suite) ← cited by §4, §A.1
     cites: libero, openvla, octo, mail, ctvam, diffusionpolicy
  §5 Ablation [sec:ablation] 0.9k words → refs tab:decomp, fig:effect
appendix: app:arch, app:cvae, app:depth, app:metric …
unreferenced: fig:saturation (commented out), app:latency
```

That single artifact answers most of what an agent currently greps for. It is also the same thing the
Outline drawer and the lint rules already parse. Costs: one parser, no network, no model.

## 4. Retrieval inside a project

1. **Structure-first (no index).** For "where is X defined", "which section discusses Y", the map plus
   the existing label and citation tables usually answers outright.
2. **Lexical, section-level.** BM25 over section-sized chunks (tantivy: Rust, Apache-2.0, in-process),
   with boosts for captions, headings and defined terms. A hit reports `§4.2 / tab:main` instead of a
   line number. This is deterministic and cheap, works offline, and needs no model.
3. **No embeddings, and no model in the retrieval loop.** We considered ranking by a model and dropped
   it. The model at the other end of MCP is the ranker. Retrieval hands it a wide, cheap set of
   candidates and lets it choose.

**Progressive disclosure is the contract.** Every tool answers first with the smallest thing that lets
a model choose: the map before the files, one line per passage or formula before the text, a candidate
list before the detail. It expands only when asked, through `detail`, a `focus`, or a bigger budget.
The context window is the scarce resource. The model makes the final selection, and no scoring function
of ours makes it.

## 5. Citation graph and related work

**Sources.** Crossref, arXiv and the DOI resolver are already on the network allowlist.
Two more are worth adding, with a DECISIONS entry each:

- **OpenAlex.** An open catalogue of works, authors and citations, CC0 data, no key. A `mailto` gets
  the polite pool. Verify current rate limits before shipping. A full snapshot exists for later.
- **Semantic Scholar Academic Graph.** 214M papers, 2.49B citations, SPECTER2 embeddings and a
  recommendations endpoint. It is free, and a key is recommended (introductory limit ~1 request/second).
  **Not used yet**: we must read its API licence before a paid service depends on it. Crossref and
  OpenAlex cover enrichment and coverage without it.

**What it enables, in order of usefulness:**

1. **Bib hygiene.** DOI and arXiv lookup, deduplication, filling missing
   fields, flagging preprints that now have a published version, and "this entry is cited nowhere"
   (the lint rule exists already).
2. **Coverage.** For each cited work, what it cites and what cites it. A paper that everything in your
   bibliography cites, and you do not, is worth a suggestion. This is the most useful answer here, and
   it is a graph query, not a model.
3. **Related work.** Ranked candidates by co-citation and by a section's own terms, each with its DOI,
   for the author to accept. Never inserted automatically, and never invented. Every suggestion carries
   a resolvable identifier, which is the rule the `cite` agent already follows.
4. **A graph view.** Your bibliography as a graph (co-citation edges, clusters, years), drawn on canvas
   in the web app, with no new runtime dependency. It is useful for spotting a missed cluster. It is
   also the most demo-able thing in this document.

**Privacy.** This sends citation keys and DOIs to third parties, never document text. It is
per-project opt-in with a visible setting. Self-hosted installs can point at their own mirror or turn
it off.

## 6. Math-expression search

Approach0 (MIT) is the reference. It builds operator trees over LaTeX with substructure matching. Its
public repository is inactive and the current engine is closed-source, so it is a design to copy, not a
library to link.

Inside a project this is tractable, and no other editor does it. Normalise each display equation into a
canonical operator tree, index the paths, and support "find equations shaped like this", "where else
does this operator appear", and "these two equations define the same symbol differently". It makes the
notation-consistency check possible, which is §8 of the Reviewer agent's job. No LaTeX editor does
this. Across a corpus it becomes a search engine, which is the side-product question below.

## 7. Side product, or Galley features?

| Idea | Fit with Galley | Effort | Verdict |
|---|---|---|---|
| Paper map + structural retrieval | Core; fixes the weakest part of M5 | Small | **Galley feature** |
| Bib hygiene + coverage | Core; M8 already promises a bib manager | Small–medium | **Galley feature** |
| Citation graph view | Natural in the editor; strong demo | Medium | **Galley feature** |
| In-project math search | Only possible where the source lives | Medium | **Galley feature** |
| "Atlas": literature search across the whole corpus — semantic + math + graph | Separate audience, separate infrastructure (corpus, index, ranking) | Large | **Later, standalone; reuses the index crate** |
| Related-work drafting pipeline | Belongs to the agent layer, needs a model | Medium | Post-V1, via the runner |
| Venue/reviewer finder | Adjacent, thin, crowded | Small | Skip |
| A shared library with deduplication for a group | Fits a group's shared bibliography | Medium | M8 |

The pattern is this. Everything that needs *the project's source* belongs in Galley, and it is small
because the parsing already exists. Everything that needs *a corpus* is a different product with
different costs. Build the first group now, and let it produce the crate the second group would need.

We want a side product under the Galley identity. A second feasible line of work is worth having, and
Atlas shares this crate, this brand and this audience. The sequencing argument holds on its own terms.
The in-project pieces take days of work and make the editor better at once. They also make Atlas cheap
to start when its turn comes.

## 8. Prototype order

1. **`galley-index` crate + `map` MCP tool + `galley map` CLI.** The paper map of §3. No network, no
   model. It improves every agent at once and is the foundation for the rest.
2. **Section-level BM25** (tantivy) behind a `context(query)` MCP tool that returns ranked passages
   with their section and label. It replaces regex search for agents.
3. **Bib hygiene**: enrichment, dedupe, published-version detection, and a coverage check, with
   Crossref and OpenAlex. It shows as lint and a bibliography drawer (M8's bib manager, brought
   forward).
4. **Citation graph view** on canvas from the project's bibliography.
5. **Math (done).** Formulas parsed into operator trees. A query matches by shape (same structure,
   different letters) or as a subexpression. Embeddings are off the table (§4).

Steps 1 and 2 take a couple of days and need no decisions. Step 3 needs the OpenAlex/Semantic Scholar
allowlist decision. Steps 4 and 5 are where this stops being cheap.

## 9. Risks

- **A wrong "missing citation" is worse than none.** Rank by graph evidence, show why, and never insert.
- **Third-party terms and rate limits** change. Verify them before depending on them, cache locally,
  and keep the feature optional, so a dead API degrades to "unavailable" and does not break the editor.
- **Index size and CPU** on the hosted service. An index per project is disk we pay for. Measure before
  enabling it by default if the cost is material.
- **Scope drift.** This document is a plan for after M7/M8, except steps 1 and 2. Those are small
  enough to do now, and they make the agents in M5 measurably better.
