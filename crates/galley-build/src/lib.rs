#![forbid(unsafe_code)]
//! The build pipeline from SPEC.md §6. It runs the sandbox, then the engine, then the log parser,
//! then the artifacts. Every child process is spawned through [`sandbox::Sandbox`], never with a
//! bare `Command::new`.

pub mod engine;
pub mod figures;
pub mod hints;
pub mod install;
pub mod lint;
pub mod log;
pub mod pack;
pub mod runner;
pub mod sandbox;
pub mod svg;
pub mod synctex;

pub use hints::Hints;
pub use log::{Diagnostic, Fix, Level};
pub use runner::{BuildEvent, BuildRequest, BuildResult, BuildStatus, Builder, ProjectBuilds};
pub use sandbox::{Sandbox, SandboxKind};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Engine(String),
    #[error("latexdiff is not installed on the server, so PDF comparison is unavailable")]
    LatexdiffUnavailable,
    #[error("download failed: {0}")]
    Download(String),
    #[error("hints database is invalid: {0}")]
    Hints(String),
    #[error("synctex data is invalid: {0}")]
    SyncTex(String),
}

pub type Result<T> = std::result::Result<T, Error>;
