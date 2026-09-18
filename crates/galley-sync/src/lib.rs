#![forbid(unsafe_code)]
//! The live document layer. It holds one yrs document per text file, an append-only update log on
//! disk, and one housekeeper per project that writes the text into git. The design follows
//! SPEC.md §5.

pub mod doc;
pub mod paths;
pub mod persist;
pub mod project;

pub use doc::DocHandle;
pub use project::{DiffSide, FileEntry, ProjectEvent, ProjectSync, SyncConfig};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git error: {0}")]
    Git(#[from] galley_history::Error),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("not a text file: {0}")]
    NotText(String),
    #[error("no such file: {0}")]
    NotFound(String),
    #[error("sync protocol error: {0}")]
    Protocol(#[from] yrs::sync::Error),
    #[error("stored update is corrupt: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, Error>;
