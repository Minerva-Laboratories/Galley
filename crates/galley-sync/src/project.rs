//! The live state of one project. This is the open documents, the git repository and the
//! housekeeper task. The housekeeper fsyncs the update logs and flushes the text into commits.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use galley_history::{Author, Checkpoint, CommitInfo, FileDiff, Repo, Side};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};

use crate::doc::DocHandle;
use crate::paths::{clean_rel_path, is_text_path};
use crate::{Error, Result};

/// An owned counterpart to [`galley_history::Side`]. It can cross an `await` or `spawn_blocking`
/// boundary. The borrowing `Side` cannot. `Workdir` reads the live working tree.
#[derive(Debug, Clone)]
pub enum DiffSide {
    Rev(String),
    Workdir,
}

impl DiffSide {
    fn is_workdir(&self) -> bool {
        matches!(self, DiffSide::Workdir)
    }
    fn as_side(&self) -> Side<'_> {
        match self {
            DiffSide::Rev(r) => Side::Rev(r),
            DiffSide::Workdir => Side::Workdir,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Commit after this much quiet following an edit.
    pub flush_quiet: Duration,
    /// Commit at least this often while edits keep coming.
    pub flush_max: Duration,
    /// fsync the update logs this long after the last edit.
    pub fsync_after: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            flush_quiet: Duration::from_millis(4000),
            flush_max: Duration::from_millis(60_000),
            fsync_after: Duration::from_millis(250),
        }
    }
}

/// Signals from documents and connections to the housekeeper.
#[derive(Debug)]
pub enum Housekeeping {
    /// A document changed.
    Edited(String),
    /// A client disconnected. Do an fsync now and a flush soon.
    Disconnected,
    /// Flush immediately and report the commit. Builds and tests use this. A message replaces the
    /// default `edit: …`, so History shows a deliberate operation as one.
    FlushNow(Option<String>, oneshot::Sender<Result<Option<CommitInfo>>>),
}

/// Sent to the listeners of the project, such as the History drawer and later the build bar.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProjectEvent {
    Commit(CommitInfo),
    FileCreated { path: String },
    FileDeleted { path: String },
    FileRenamed { from: String, to: String },
    /// Uploads and other bulk changes. Listeners fetch the file list again.
    FilesChanged,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Text,
    Binary,
}

pub struct ProjectSync {
    id: String,
    workdir: PathBuf,
    repo: Arc<Mutex<Repo>>,
    docs: RwLock<HashMap<String, Arc<DocHandle>>>,
    housekeeping: mpsc::UnboundedSender<Housekeeping>,
    events: broadcast::Sender<ProjectEvent>,
    last_editor: Mutex<Option<String>>,
}

impl std::fmt::Debug for ProjectSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectSync").field("id", &self.id).finish()
    }
}

impl ProjectSync {
    /// Open the project at `workdir` and start its housekeeper. Initialise git if there is no
    /// repository. Call this from inside a tokio runtime.
    pub fn open(id: &str, workdir: &Path, config: SyncConfig) -> Result<Arc<ProjectSync>> {
        let repo = Repo::init_or_open(workdir)?;
        let (hk_tx, hk_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(256);
        let project = Arc::new(ProjectSync {
            id: id.to_string(),
            workdir: workdir.to_path_buf(),
            repo: Arc::new(Mutex::new(repo)),
            docs: RwLock::new(HashMap::new()),
            housekeeping: hk_tx,
            events,
            last_editor: Mutex::new(None),
        });
        tokio::spawn(housekeeper(Arc::downgrade(&project), hk_rx, config));
        Ok(project)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<ProjectEvent> {
        self.events.subscribe()
    }

    pub fn housekeeping(&self) -> mpsc::UnboundedSender<Housekeeping> {
        self.housekeeping.clone()
    }

    /// Remember who edited last. The next auto-commit uses that name as the author.
    pub fn set_last_editor(&self, name: &str) {
        let name = name.trim();
        if !name.is_empty() {
            *self.last_editor.lock().expect("last_editor poisoned") = Some(name.to_string());
        }
    }

    /// The live document for a text file, loading it on first use.
    pub async fn doc(&self, rel_path: &str) -> Result<Arc<DocHandle>> {
        let rel = clean_rel_path(rel_path).ok_or_else(|| Error::InvalidPath(rel_path.into()))?;
        if !is_text_path(&rel) {
            return Err(Error::NotText(rel));
        }
        if let Some(doc) = self.docs.read().await.get(&rel) {
            return Ok(Arc::clone(doc));
        }
        let mut docs = self.docs.write().await;
        if let Some(doc) = docs.get(&rel) {
            return Ok(Arc::clone(doc));
        }
        let workdir = self.workdir.clone();
        let hk = self.housekeeping.clone();
        let rel_for_open = rel.clone();
        let doc = tokio::task::spawn_blocking(move || DocHandle::open(&workdir, &rel_for_open, hk))
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e)))??;
        docs.insert(rel, Arc::clone(&doc));
        Ok(doc)
    }

    /// Create an empty text file (and its document). Fails if it already exists.
    pub async fn create_file(&self, rel_path: &str) -> Result<FileEntry> {
        let rel = clean_rel_path(rel_path).ok_or_else(|| Error::InvalidPath(rel_path.into()))?;
        if !is_text_path(&rel) {
            return Err(Error::NotText(rel));
        }
        let full = self.workdir.join(&rel);
        if full.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{rel} already exists"),
            )));
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, "")?;
        let _ = self.events.send(ProjectEvent::FileCreated { path: rel.clone() });
        Ok(FileEntry {
            path: rel,
            kind: FileKind::Text,
            size: 0,
        })
    }

    /// Whether a project file exists on disk.
    pub fn exists(&self, rel_path: &str) -> bool {
        clean_rel_path(rel_path).is_some_and(|rel| self.workdir.join(rel).is_file())
    }

    /// The current bytes of a file. Text comes from the live document. Every other file comes from
    /// the working tree.
    pub async fn read_bytes(&self, rel_path: &str) -> Result<(String, Vec<u8>)> {
        let rel = clean_rel_path(rel_path).ok_or_else(|| Error::InvalidPath(rel_path.into()))?;
        let full = self.workdir.join(&rel);
        if !full.is_file() {
            return Err(Error::NotFound(rel));
        }
        if is_text_path(&rel) {
            let text = self.doc(&rel).await?.text().await;
            return Ok((rel, text.into_bytes()));
        }
        Ok((rel.clone(), std::fs::read(full)?))
    }

    /// Add or replace a file with uploaded bytes and commit. Text files go through their document,
    /// so open editors show the new content as an ordinary edit.
    pub async fn put_file(&self, rel_path: &str, bytes: &[u8], replace: bool, author: &str) -> Result<FileEntry> {
        let rel = clean_rel_path(rel_path).ok_or_else(|| Error::InvalidPath(rel_path.into()))?;
        let full = self.workdir.join(&rel);
        if full.is_dir() {
            return Err(Error::InvalidPath(rel));
        }
        let existed = full.exists();
        if existed && !replace {
            return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{rel} already exists"))));
        }
        let text = is_text_path(&rel);
        if text {
            let content = std::str::from_utf8(bytes).map_err(|_| Error::NotText(rel.clone()))?.to_string();
            write_atomic(&full, bytes)?;
            self.doc(&rel).await?.reseed(&content).await;
        } else {
            write_atomic(&full, bytes)?;
        }
        self.set_last_editor(author);
        let verb = if existed { "replace" } else { "upload" };
        self.flush_with(Some(format!("{verb}: {rel}"))).await?;
        let _ = self.events.send(ProjectEvent::FilesChanged);
        Ok(FileEntry { path: rel, kind: if text { FileKind::Text } else { FileKind::Binary }, size: bytes.len() as u64 })
    }

    /// Delete a file and commit. Galley empties the document of a text file and does not discard
    /// it. A browser that holds the history offline then merges into an empty file. It does not
    /// bring the file back.
    pub async fn delete_file(&self, rel_path: &str, author: &str) -> Result<()> {
        let rel = clean_rel_path(rel_path).ok_or_else(|| Error::InvalidPath(rel_path.into()))?;
        let full = self.workdir.join(&rel);
        if !full.is_file() {
            return Err(Error::NotFound(rel));
        }
        // Land pending edits first so the deleted text is in history.
        self.flush_now().await?;
        std::fs::remove_file(&full)?;
        if is_text_path(&rel) {
            self.doc(&rel).await?.reseed("").await;
        }
        prune_empty_dirs(&self.workdir, full.parent());
        self.set_last_editor(author);
        self.flush_with(Some(format!("delete: {rel}"))).await?;
        let _ = self.events.send(ProjectEvent::FileDeleted { path: rel });
        Ok(())
    }

    /// Rename or move a file and commit. Text moves through the documents. The new document takes
    /// the content. The old document is emptied, as on a delete.
    pub async fn rename_file(&self, from: &str, to: &str, author: &str) -> Result<String> {
        let from = clean_rel_path(from).ok_or_else(|| Error::InvalidPath(from.into()))?;
        let to = clean_rel_path(to).ok_or_else(|| Error::InvalidPath(to.into()))?;
        let (src, dst) = (self.workdir.join(&from), self.workdir.join(&to));
        if !src.is_file() {
            return Err(Error::NotFound(from));
        }
        if from == to {
            return Ok(to);
        }
        if dst.exists() {
            return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{to} already exists"))));
        }
        if is_text_path(&from) != is_text_path(&to) {
            return Err(Error::InvalidPath(format!("{to} (a rename cannot change a text file into a binary one or back)")));
        }
        self.flush_now().await?;
        if is_text_path(&from) {
            let content = self.doc(&from).await?.text().await;
            write_atomic(&dst, content.as_bytes())?;
            self.doc(&to).await?.reseed(&content).await;
            std::fs::remove_file(&src)?;
            self.doc(&from).await?.reseed("").await;
        } else {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&src, &dst)?;
        }
        prune_empty_dirs(&self.workdir, src.parent());
        self.set_last_editor(author);
        self.flush_with(Some(format!("rename: {from} → {to}"))).await?;
        let _ = self.events.send(ProjectEvent::FileRenamed { from, to: to.clone() });
        Ok(to)
    }

    /// Every file in the working tree except git and Galley internals, sorted by path.
    pub fn list_files(&self) -> Result<Vec<FileEntry>> {
        let mut out = BTreeSet::new();
        walk(&self.workdir, &self.workdir, &mut out)?;
        Ok(out.into_iter().collect())
    }

    /// Write every open document to the working tree and commit. Returns the commit, or `None`
    /// when the tree already matched HEAD.
    pub async fn flush_now(&self) -> Result<Option<CommitInfo>> {
        self.flush_with(None).await
    }

    /// `flush_now` with a commit message of the caller's choosing.
    pub async fn flush_with(&self, message: Option<String>) -> Result<Option<CommitInfo>> {
        let (tx, rx) = oneshot::channel();
        self.housekeeping
            .send(Housekeeping::FlushNow(message, tx))
            .map_err(|_| Error::Io(std::io::Error::other("housekeeper stopped")))?;
        rx.await
            .map_err(|_| Error::Io(std::io::Error::other("housekeeper dropped request")))?
    }

    pub async fn history(&self, limit: usize) -> Result<Vec<CommitInfo>> {
        let repo = Arc::clone(&self.repo);
        tokio::task::spawn_blocking(move || repo.lock().expect("repo poisoned").log(limit))
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e)))?
            .map_err(Error::from)
    }

    pub async fn checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let repo = Arc::clone(&self.repo);
        tokio::task::spawn_blocking(move || repo.lock().expect("repo poisoned").checkpoints())
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e)))?
            .map_err(Error::from)
    }

    /// Flush pending edits, then tag the current commit as a named checkpoint. Returns the sha of
    /// the tagged commit. `id` is a ref-safe token that the caller also stores in SQLite.
    pub async fn create_checkpoint(&self, id: &str, label: &str, author: Author) -> Result<String> {
        self.flush_now().await?;
        let repo = Arc::clone(&self.repo);
        let (id, label) = (id.to_string(), label.to_string());
        tokio::task::spawn_blocking(move || {
            repo.lock()
                .expect("repo poisoned")
                .create_checkpoint(&id, "HEAD", &label, &author)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?
        .map_err(Error::from)
    }

    pub async fn file_at(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let repo = Arc::clone(&self.repo);
        let (rev, path) = (rev.to_string(), path.to_string());
        tokio::task::spawn_blocking(move || repo.lock().expect("repo poisoned").file_at(&rev, &path))
            .await
            .map_err(|e| Error::Io(std::io::Error::other(e)))?
            .map_err(Error::from)
    }

    /// Diff between two revisions, or from a revision to the live working tree. If one side is the
    /// working tree, Galley flushes the pending edits first. A compare with the current text then
    /// shows what the user sees.
    pub async fn diff(
        &self,
        from: DiffSide,
        to: DiffSide,
        path: Option<String>,
    ) -> Result<Vec<FileDiff>> {
        if from.is_workdir() || to.is_workdir() {
            self.flush_now().await?;
        }
        let repo = Arc::clone(&self.repo);
        tokio::task::spawn_blocking(move || {
            let repo = repo.lock().expect("repo poisoned");
            repo.diff(from.as_side(), to.as_side(), path.as_deref())
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?
        .map_err(Error::from)
    }

    /// Make the working tree match `rev` and commit it. Galley never rewrites history. Then re-seed
    /// every live document that changed. Collaborators converge on the restored text, and the next
    /// auto-commit does not overwrite it. Returns the new commit.
    pub async fn restore(&self, rev: &str, author: Author, label: &str) -> Result<Option<CommitInfo>> {
        // Land whatever is in flight so nothing is lost, then restore on top of it.
        self.flush_now().await?;
        let repo = Arc::clone(&self.repo);
        let (rev_owned, message) = (rev.to_string(), format!("restore: {label}"));
        let (commit, changed) = tokio::task::spawn_blocking(move || {
            repo.lock()
                .expect("repo poisoned")
                .restore(&rev_owned, &author, &message)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?
        .map_err(Error::Git)?;

        // Re-seed the live documents from the restored files. It is enough to open a document that
        // is not loaded yet. The reseed appends the delta to the update log, so later opens are
        // correct too.
        for rel in &changed {
            if !is_text_path(rel) {
                continue;
            }
            let on_disk = std::fs::read_to_string(self.workdir.join(rel)).unwrap_or_default();
            if let Ok(doc) = self.doc(rel).await {
                doc.reseed(&on_disk).await;
            }
        }

        if let Some(c) = &commit {
            tracing::info!(project = %self.id, sha = %c.short_sha, "restored");
            let _ = self.events.send(ProjectEvent::Commit(c.clone()));
            // A restore can bring back deleted files or remove added ones.
            let _ = self.events.send(ProjectEvent::FilesChanged);
        }
        Ok(commit)
    }

    async fn write_and_commit(&self, dirty: &BTreeSet<String>, message: Option<String>) -> Result<Option<CommitInfo>> {
        let docs = self.docs.read().await;
        let mut written = Vec::new();
        for rel in dirty {
            let Some(doc) = docs.get(rel) else { continue };
            let full = self.workdir.join(rel);
            // A create, an upload, a rename or a restore makes a file on disk first. A document
            // with no file was deleted. A late edit from a stale editor must not bring it back.
            if !full.exists() {
                continue;
            }
            let text = doc.text().await;
            let full2 = full.clone();
            tokio::task::spawn_blocking(move || write_atomic(&full2, text.as_bytes()))
                .await
                .map_err(|e| Error::Io(std::io::Error::other(e)))??;
            let _ = full;
            written.push(rel.clone());
        }
        drop(docs);
        // A named flush also carries file operations such as a delete, a rename or an upload.
        // These have no document text to write. commit_all decides whether the tree changed.
        if written.is_empty() && message.is_none() {
            return Ok(None);
        }
        let author = self
            .last_editor
            .lock()
            .expect("last_editor poisoned")
            .as_deref()
            .map(Author::from_display_name)
            .unwrap_or_else(Author::galley);
        let message = message.unwrap_or_else(|| format!("edit: {}", written.join(", ")));
        let repo = Arc::clone(&self.repo);
        let commit = tokio::task::spawn_blocking(move || {
            let repo = repo.lock().expect("repo poisoned");
            match repo.commit_all(&author, &message)? {
                Some(_) => Ok(repo.log(1)?.into_iter().next()),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        let commit: Option<CommitInfo> = commit.map_err(Error::Git)?;
        if let Some(c) = &commit {
            tracing::info!(project = %self.id, sha = %c.short_sha, "committed");
            let _ = self.events.send(ProjectEvent::Commit(c.clone()));
        }
        Ok(commit)
    }

    async fn fsync_all(&self) {
        let docs = self.docs.read().await;
        for doc in docs.values() {
            if let Err(e) = doc.fsync() {
                tracing::warn!(path = doc.rel_path(), error = %e, "fsync failed");
            }
        }
    }
}

/// Timers only become shorter. This task never blocks on git for longer than one commit.
async fn housekeeper(
    project: Weak<ProjectSync>,
    mut rx: mpsc::UnboundedReceiver<Housekeeping>,
    config: SyncConfig,
) {
    let mut dirty: BTreeSet<String> = BTreeSet::new();
    let mut last_edit: Option<Instant> = None;
    let mut first_unflushed: Option<Instant> = None;
    let mut fsync_due: Option<Instant> = None;

    loop {
        let flush_due = match (last_edit, first_unflushed) {
            (Some(last), Some(first)) => Some((last + config.flush_quiet).min(first + config.flush_max)),
            _ => None,
        };
        let next = match (fsync_due, flush_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };

        let sleep = async {
            match next {
                Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Housekeeping::Edited(path)) => {
                    let now = Instant::now();
                    dirty.insert(path);
                    last_edit = Some(now);
                    first_unflushed.get_or_insert(now);
                    fsync_due = Some(now + config.fsync_after);
                }
                Some(Housekeeping::Disconnected) => {
                    if let Some(p) = project.upgrade() {
                        p.fsync_all().await;
                    }
                    fsync_due = None;
                    if !dirty.is_empty() {
                        // Treat the disconnect as the end of a burst so the text lands soon.
                        last_edit = Some(Instant::now() - config.flush_quiet + config.fsync_after);
                    }
                }
                Some(Housekeeping::FlushNow(message, reply)) => {
                    let result = match project.upgrade() {
                        Some(p) => {
                            p.fsync_all().await;
                            let taken = std::mem::take(&mut dirty);
                            last_edit = None;
                            first_unflushed = None;
                            fsync_due = None;
                            p.write_and_commit(&taken, message).await
                        }
                        None => Ok(None),
                    };
                    let _ = reply.send(result);
                }
                None => break,
            },
            _ = sleep => {
                let now = Instant::now();
                let Some(p) = project.upgrade() else { break };
                if fsync_due.is_some_and(|t| t <= now) {
                    p.fsync_all().await;
                    fsync_due = None;
                }
                if flush_due.is_some_and(|t| t <= now) {
                    let taken = std::mem::take(&mut dirty);
                    last_edit = None;
                    first_unflushed = None;
                    if let Err(e) = p.write_and_commit(&taken, None).await {
                        tracing::error!(project = p.id(), error = %e, "auto-commit failed; will retry on the next edit");
                        dirty.extend(taken);
                        last_edit = Some(now);
                        first_unflushed = Some(now);
                    }
                }
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.galley-tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Remove the directories that a delete or a move left empty. Do not remove the project root.
fn prune_empty_dirs(root: &Path, mut dir: Option<&Path>) {
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<FileEntry>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == ".galley" || name.ends_with(".galley-tmp") {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(std::io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            let kind = if is_text_path(&rel) {
                FileKind::Text
            } else {
                FileKind::Binary
            };
            out.insert(FileEntry {
                path: rel,
                kind,
                size: meta.len(),
            });
        }
    }
    Ok(())
}
