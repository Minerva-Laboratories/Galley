#![forbid(unsafe_code)]
//! The Galley HTTP and WebSocket server. It holds the REST routes for projects and files, the
//! y-protocol sockets for live documents, a project event socket, and the embedded web app.

mod app;
mod assets;
pub mod auth;
mod auth_routes;
mod bib_routes;
mod build_routes;
mod collab_routes;
mod file_routes;
pub mod config;
mod history_routes;
mod mcp_routes;
mod pack_routes;
mod token_routes;
pub mod db;
pub mod registry;
mod routes;
mod share_routes;
pub mod grammar;
pub mod templates;
pub mod venues;
pub mod store;
mod ws;

pub use app::{router, serve, AppState, ServeOptions};
pub use config::Config;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
