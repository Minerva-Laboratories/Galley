#![forbid(unsafe_code)]
//! The local agent runner (SPEC §7.6): checks a project out through its MCP endpoint, lets a model
//! edit the copy with ordinary file tools, then turns the resulting diff into proposals. The model
//! never calls Galley and Galley never calls the model. `propose_patch` stays the only write path.

mod chat;
mod mcp;
mod patch;
mod runner;

pub use chat::ChatBackend;
pub use mcp::McpClient;
pub use patch::{edits, Edit};
pub use runner::{run, RunError, RunOptions, RunReport};
