#![forbid(unsafe_code)]
//! The literature layer: fill in bibliography entries and find work the paper should probably
//! cite, from open catalogues (docs/RETRIEVAL.md §5).
//!
//! Only citation keys, DOIs and titles leave the machine. The document never leaves. Every project
//! is opted out until someone turns it on. Answers are cached on disk. A dead API shows
//! "unavailable" and the editor keeps working.

mod client;
mod coverage;
mod enrich;
mod lookup;

pub use client::{Client, Error};
pub use coverage::{coverage, render as coverage_report, Candidate};
pub use enrich::{enrich, render as enrich_report, Suggestion};
pub use lookup::{lookup, parse_id, Id, Lookup};

/// Contact details go in the User-Agent and the `mailto` parameter. Both catalogues ask for them.
/// Both give better service in return.
pub const USER_AGENT_BASE: &str = concat!("galley/", env!("CARGO_PKG_VERSION"));
