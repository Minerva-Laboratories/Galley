#![forbid(unsafe_code)]
//! The paper map: what a LaTeX project contains and how its parts refer to each other.
//!
//! Code editors infer structure. LaTeX states it. Sections, labels, `\ref`, `\cite`, captions and
//! environments are all in the source. A useful map needs no model and no index, only a parse and
//! a ranking. An agent reads the map before it greps (docs/RETRIEVAL.md §3).

pub mod bib;
pub mod graph;
pub mod math;
mod parse;
mod rank;
mod search;
mod render;

pub use parse::{scan, BibEntry, Kind, Object, Paper, Section};
pub use rank::{rank, Focus};
pub use search::{render_hits, search, tokenize, Hit};
pub use render::render;

/// About four characters per token. Budgets use tokens because callers work in tokens.
pub(crate) const CHARS_PER_TOKEN: usize = 4;
