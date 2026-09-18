#![forbid(unsafe_code)]
//! All git operations in Galley go through this crate. Two rules from the spec apply everywhere.
//! Galley never rewrites history. A restore makes a new commit, and a checkpoint is a tag. Every
//! project stays a plain repository that `git clone` can read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use git2::{DiffOptions, ErrorCode, IndexAddOption, Oid, Repository, Signature};
use serde::Serialize;

/// Paths Galley keeps inside a project but never commits. Galley excludes them through
/// `.git/info/exclude`, so the user's own `.gitignore` stays their own.
const EXCLUDED: &[&str] = &[".galley/"];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Who a commit is attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

impl Author {
    /// Used when no user is signed in, or when the name of an editor is unknown. M1 has no accounts.
    pub fn galley() -> Self {
        Author {
            name: "Galley".into(),
            email: "galley@localhost".into(),
        }
    }

    /// In M1 an editor is identified by a display name. Derive a stable, harmless email from it.
    pub fn from_display_name(name: &str) -> Self {
        let slug: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let slug = if slug.is_empty() { "editor".to_string() } else { slug };
        Author {
            name: name.to_string(),
            email: format!("{slug}@galley.local"),
        }
    }

    fn signature(&self) -> Result<Signature<'static>> {
        Ok(Signature::now(&self.name, &self.email)?)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub short_sha: String,
    pub message: String,
    pub author: String,
    pub time: DateTime<Utc>,
}

/// A named checkpoint. It is a git tag on a commit. Galley keeps the label, the author and the time
/// for display. The tag makes the commit recoverable with plain git. The fields mirror the
/// `checkpoints` DB row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Checkpoint {
    pub id: String,
    pub label: String,
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub time: DateTime<Utc>,
}

/// The tag prefix for checkpoints. Galley does not use `refs/tags/<label>` because labels contain
/// spaces and unicode that are not valid in ref names. The id is a short, ref-safe token.
const CHECKPOINT_PREFIX: &str = "galley/checkpoint/";

/// The changes to one file between two trees, or between a tree and the working directory.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// added | deleted | modified | renamed
    pub status: String,
    /// Set for renames only: where the file used to be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiffLine {
    /// ' ' context, '+' added, '-' removed.
    pub origin: char,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_lineno: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_lineno: Option<u32>,
    pub content: String,
}

/// The source of a diff or a file lookup. It is a specific revision, or the live working tree.
#[derive(Debug, Clone, Copy)]
pub enum Side<'a> {
    Rev(&'a str),
    Workdir,
}

/// A project's git repository.
pub struct Repo {
    inner: Repository,
    workdir: PathBuf,
}

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo").field("workdir", &self.workdir).finish()
    }
}

impl Repo {
    /// Open the repository at `path`. Initialise one if it does not exist. The working tree can
    /// already contain the files of an imported project. The first [`Repo::commit_all`] commits
    /// them.
    pub fn init_or_open(path: &Path) -> Result<Repo> {
        std::fs::create_dir_all(path)?;
        let inner = match Repository::open(path) {
            Ok(r) => r,
            Err(e) if e.code() == ErrorCode::NotFound => {
                let mut opts = git2::RepositoryInitOptions::new();
                opts.initial_head("main");
                Repository::init_opts(path, &opts)?
            }
            Err(e) => return Err(e.into()),
        };
        let repo = Repo {
            inner,
            workdir: path.to_path_buf(),
        };
        repo.ensure_excludes()?;
        Ok(repo)
    }

    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    fn ensure_excludes(&self) -> Result<()> {
        let info = self.inner.path().join("info");
        std::fs::create_dir_all(&info)?;
        let exclude = info.join("exclude");
        let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
        let missing: Vec<&str> = EXCLUDED
            .iter()
            .copied()
            .filter(|p| !existing.lines().any(|l| l.trim() == *p))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        for p in missing {
            out.push_str(p);
            out.push('\n');
        }
        std::fs::write(exclude, out)?;
        Ok(())
    }

    /// Stage everything in the working tree and commit if the tree changed. Returns `None` when
    /// there was nothing to commit.
    pub fn commit_all(&self, author: &Author, message: &str) -> Result<Option<Oid>> {
        let mut index = self.inner.index()?;
        index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
        index.update_all(["*"].iter(), None)?;
        let tree_oid = index.write_tree()?;
        index.write()?;

        let head = match self.inner.head() {
            Ok(h) => Some(h.peel_to_commit()?),
            Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
                None
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(parent) = &head {
            if parent.tree_id() == tree_oid {
                return Ok(None);
            }
        }

        let tree = self.inner.find_tree(tree_oid)?;
        let sig = author.signature()?;
        let parents: Vec<&git2::Commit> = head.iter().collect();
        let oid = self
            .inner
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(Some(oid))
    }

    /// Most recent commits first. `limit` bounds the walk, so a large history stays fast.
    pub fn log(&self, limit: usize) -> Result<Vec<CommitInfo>> {
        if self.head_sha()?.is_none() {
            return Ok(Vec::new());
        }
        let mut walk = self.inner.revwalk()?;
        walk.push_head()?;
        walk.set_sorting(git2::Sort::TIME)?;
        let mut out = Vec::new();
        for oid in walk.take(limit) {
            let commit = self.inner.find_commit(oid?)?;
            let sha = commit.id().to_string();
            out.push(CommitInfo {
                short_sha: sha[..7].to_string(),
                sha,
                message: String::from_utf8_lossy(commit.summary_bytes().unwrap_or_default())
                    .into_owned(),
                author: String::from_utf8_lossy(commit.author().name_bytes()).into_owned(),
                time: Utc
                    .timestamp_opt(commit.time().seconds(), 0)
                    .single()
                    .unwrap_or_else(Utc::now),
            });
        }
        Ok(out)
    }

    pub fn head_sha(&self) -> Result<Option<String>> {
        match self.inner.head() {
            Ok(h) => Ok(h.target().map(|o| o.to_string())),
            Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Resolve a revision string (sha, `HEAD`, a tag) to its commit.
    fn commit_at<'a>(&'a self, rev: &str) -> Result<git2::Commit<'a>> {
        Ok(self.inner.revparse_single(rev)?.peel_to_commit()?)
    }

    /// Tag `rev` as a checkpoint. The tag is annotated, so plain git keeps the label and the
    /// author. The caller supplies `id` as a ref-safe token. Returns the full sha of the tagged
    /// commit.
    pub fn create_checkpoint(
        &self,
        id: &str,
        rev: &str,
        label: &str,
        author: &Author,
    ) -> Result<String> {
        let commit = self.commit_at(rev)?;
        let sig = author.signature()?;
        let name = format!("{CHECKPOINT_PREFIX}{id}");
        self.inner
            .tag(&name, commit.as_object(), &sig, label, false)?;
        Ok(commit.id().to_string())
    }

    /// Every checkpoint tag, newest commit first.
    pub fn checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut out = Vec::new();
        let names = self.inner.tag_names(Some(&format!("{CHECKPOINT_PREFIX}*")))?;
        for name in names.iter().filter_map(|r| r.ok().flatten()) {
            let id = name.strip_prefix(CHECKPOINT_PREFIX).unwrap_or(name).to_string();
            let obj = self.inner.revparse_single(name)?;
            let commit = obj.peel_to_commit()?;
            let sha = commit.id().to_string();
            // An annotated tag holds the label as its message and holds the tagger. For a
            // lightweight tag, use the data of the commit.
            let (label, author, time) = match obj.as_tag() {
                Some(tag) => {
                    let tagger = tag.tagger();
                    let author = tagger
                        .as_ref()
                        .map(|t| String::from_utf8_lossy(t.name_bytes()).into_owned())
                        .unwrap_or_default();
                    let time = tagger
                        .and_then(|t| Utc.timestamp_opt(t.when().seconds(), 0).single())
                        .unwrap_or_else(Utc::now);
                    (
                        tag.message().ok().flatten().unwrap_or("").trim().to_string(),
                        author,
                        time,
                    )
                }
                None => (
                    String::new(),
                    String::from_utf8_lossy(commit.author().name_bytes()).into_owned(),
                    Utc.timestamp_opt(commit.time().seconds(), 0).single().unwrap_or_else(Utc::now),
                ),
            };
            out.push(Checkpoint {
                id,
                label,
                short_sha: sha[..7].to_string(),
                sha,
                author,
                time,
            });
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.time));
        Ok(out)
    }

    /// The text content of `path` at `rev`, or `None` when the file is absent or binary.
    pub fn file_at(&self, rev: &str, path: &str) -> Result<Option<String>> {
        let tree = self.commit_at(rev)?.tree()?;
        let entry = match tree.get_path(Path::new(path)) {
            Ok(e) => e,
            Err(e) if e.code() == ErrorCode::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let blob = self.inner.find_blob(entry.id())?;
        if blob.is_binary() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(blob.content()).into_owned()))
    }

    /// The changes per file from `from` to `to`. A side of [`Side::Workdir`] uses the live working
    /// tree. Staged and unstaged changes both count. `path` limits the diff to one file when the
    /// caller gives it.
    pub fn diff(&self, from: Side, to: Side, path: Option<&str>) -> Result<Vec<FileDiff>> {
        let mut opts = DiffOptions::new();
        opts.context_lines(3).include_typechange(true);
        if let Some(p) = path {
            opts.pathspec(p);
        }
        let from_tree = match from {
            Side::Rev(r) => Some(self.commit_at(r)?.tree()?),
            Side::Workdir => None,
        };
        let diff = match (from, to) {
            (Side::Rev(_), Side::Rev(r)) => {
                let to_tree = self.commit_at(r)?.tree()?;
                self.inner
                    .diff_tree_to_tree(from_tree.as_ref(), Some(&to_tree), Some(&mut opts))?
            }
            (Side::Rev(_), Side::Workdir) => self.inner.diff_tree_to_workdir_with_index(
                from_tree.as_ref(),
                Some(&mut opts),
            )?,
            (Side::Workdir, Side::Rev(r)) => {
                // Turn a request from the workdir to a rev into a request from the rev to the
                // workdir, with swapped origins.
                let to_tree = self.commit_at(r)?.tree()?;
                opts.reverse(true);
                self.inner
                    .diff_tree_to_workdir_with_index(Some(&to_tree), Some(&mut opts))?
            }
            (Side::Workdir, Side::Workdir) => return Ok(Vec::new()),
        };
        collect_diff(&diff)
    }

    /// Make the working tree match `rev` and record it as a new commit. Galley never rewrites
    /// history. Returns the new commit and the text files with changed content, so that the caller
    /// can re-seed the live documents. `.galley/` and other untracked files do not change.
    pub fn restore(
        &self,
        rev: &str,
        author: &Author,
        message: &str,
    ) -> Result<(Option<CommitInfo>, Vec<String>)> {
        let target = self.commit_at(rev)?.tree()?;
        let target_files = tree_files(&target)?;
        let head_files = match self.inner.head() {
            Ok(h) => tree_files(&h.peel_to_tree()?)?,
            Err(e) if e.code() == ErrorCode::UnbornBranch || e.code() == ErrorCode::NotFound => {
                BTreeMap::new()
            }
            Err(e) => return Err(e.into()),
        };

        let mut changed = Vec::new();
        for (path, oid) in &target_files {
            if head_files.get(path) == Some(oid) {
                continue;
            }
            let blob = self.inner.find_blob(*oid)?;
            write_file(&self.workdir.join(path), blob.content())?;
            changed.push(path.clone());
        }
        for path in head_files.keys() {
            if target_files.contains_key(path) {
                continue;
            }
            let full = self.workdir.join(path);
            if full.exists() {
                std::fs::remove_file(&full)?;
            }
            changed.push(path.clone());
        }

        let oid = self.commit_all(author, message)?;
        let info = match oid {
            Some(_) => self.log(1)?.into_iter().next(),
            None => None,
        };
        Ok((info, changed))
    }
}

/// Flatten a tree into `path -> blob oid` for every file. This includes the files in subtrees.
fn tree_files(tree: &git2::Tree) -> Result<BTreeMap<String, Oid>> {
    let mut out = BTreeMap::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            if let Ok(name) = entry.name() {
                let path = if dir.is_empty() {
                    name.to_string()
                } else {
                    format!("{dir}{name}")
                };
                out.insert(path, entry.id());
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(out)
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// Turn a `git2::Diff` into owned, serialisable per-file structures.
fn collect_diff(diff: &git2::Diff) -> Result<Vec<FileDiff>> {
    use std::cell::RefCell;
    let files: RefCell<Vec<FileDiff>> = RefCell::new(Vec::new());
    diff.foreach(
        &mut |delta, _| {
            let new_path = delta
                .new_file()
                .path()
                .map(|p| p.to_string_lossy().replace('\\', "/"));
            let old_path = delta
                .old_file()
                .path()
                .map(|p| p.to_string_lossy().replace('\\', "/"));
            let status = match delta.status() {
                git2::Delta::Added => "added",
                git2::Delta::Deleted => "deleted",
                git2::Delta::Renamed => "renamed",
                git2::Delta::Copied => "copied",
                _ => "modified",
            };
            let binary = delta.flags().is_binary();
            files.borrow_mut().push(FileDiff {
                path: new_path.or_else(|| old_path.clone()).unwrap_or_default(),
                status: status.to_string(),
                old_path: if delta.status() == git2::Delta::Renamed { old_path } else { None },
                binary,
                hunks: Vec::new(),
            });
            true
        },
        None,
        Some(&mut |_delta, hunk| {
            if let Some(file) = files.borrow_mut().last_mut() {
                file.hunks.push(Hunk {
                    header: String::from_utf8_lossy(hunk.header()).trim_end().to_string(),
                    lines: Vec::new(),
                });
            }
            true
        }),
        Some(&mut |_delta, _hunk, line| {
            let mut files = files.borrow_mut();
            let Some(file) = files.last_mut() else { return true };
            let content = String::from_utf8_lossy(line.content())
                .trim_end_matches('\n')
                .to_string();
            let dl = DiffLine {
                origin: line.origin(),
                old_lineno: line.old_lineno(),
                new_lineno: line.new_lineno(),
                content,
            };
            match file.hunks.last_mut() {
                Some(h) => h.lines.push(dl),
                // A file-level line such as a binary marker has no hunk. Start one.
                None => file.hunks.push(Hunk { header: String::new(), lines: vec![dl] }),
            }
            true
        }),
    )?;
    Ok(files.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_commit_and_log() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        assert!(repo.log(10).unwrap().is_empty());
        assert_eq!(repo.head_sha().unwrap(), None);

        std::fs::write(dir.path().join("main.tex"), "\\documentclass{article}\n").unwrap();
        let author = Author::from_display_name("Ana Novak");
        let first = repo.commit_all(&author, "edit: main.tex").unwrap();
        assert!(first.is_some());
        // Nothing changed, so there is no new commit.
        assert_eq!(repo.commit_all(&author, "edit: main.tex").unwrap(), None);

        std::fs::create_dir_all(dir.path().join("sections")).unwrap();
        std::fs::write(dir.path().join("sections/2.tex"), "\\section{Two}\n").unwrap();
        repo.commit_all(&author, "edit: sections/2.tex").unwrap();

        let log = repo.log(10).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].message, "edit: sections/2.tex");
        assert_eq!(log[1].message, "edit: main.tex");
        assert_eq!(log[0].author, "Ana Novak");
        assert_eq!(log[0].short_sha.len(), 7);
    }

    #[test]
    fn galley_dir_is_never_committed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join(".galley")).unwrap();
        std::fs::write(dir.path().join(".galley/doc-state.ybin"), b"x").unwrap();
        std::fs::write(dir.path().join("main.tex"), "hi").unwrap();
        repo.commit_all(&Author::galley(), "edit: main.tex").unwrap();

        let head = repo.inner.head().unwrap().peel_to_tree().unwrap();
        assert!(head.get_name("main.tex").is_some());
        assert!(head.get_name(".galley").is_none());
    }

    #[test]
    fn checkpoint_tags_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        let author = Author::from_display_name("Ana Novak");
        std::fs::write(dir.path().join("main.tex"), "one\n").unwrap();
        repo.commit_all(&author, "edit: main.tex").unwrap();
        let sha = repo.create_checkpoint("cp1", "HEAD", "Submitted to NeurIPS", &author).unwrap();

        let cps = repo.checkpoints().unwrap();
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0].id, "cp1");
        assert_eq!(cps[0].label, "Submitted to NeurIPS");
        assert_eq!(cps[0].sha, sha);
        assert_eq!(cps[0].author, "Ana Novak");
    }

    #[test]
    fn file_at_reads_old_content() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        let a = Author::galley();
        std::fs::write(dir.path().join("main.tex"), "first\n").unwrap();
        let c1 = repo.commit_all(&a, "one").unwrap().unwrap().to_string();
        std::fs::write(dir.path().join("main.tex"), "second\n").unwrap();
        repo.commit_all(&a, "two").unwrap();

        assert_eq!(repo.file_at(&c1, "main.tex").unwrap().as_deref(), Some("first\n"));
        assert_eq!(repo.file_at("HEAD", "main.tex").unwrap().as_deref(), Some("second\n"));
        assert_eq!(repo.file_at("HEAD", "missing.tex").unwrap(), None);
    }

    #[test]
    fn diff_between_commits() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        let a = Author::galley();
        std::fs::write(dir.path().join("main.tex"), "alpha\nbeta\n").unwrap();
        let c1 = repo.commit_all(&a, "one").unwrap().unwrap().to_string();
        std::fs::write(dir.path().join("main.tex"), "alpha\ngamma\n").unwrap();
        let c2 = repo.commit_all(&a, "two").unwrap().unwrap().to_string();

        let d = repo.diff(Side::Rev(&c1), Side::Rev(&c2), None).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, "main.tex");
        assert_eq!(d[0].status, "modified");
        let added: Vec<_> = d[0].hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '+').collect();
        assert!(added.iter().any(|l| l.content == "gamma"));
        let removed: Vec<_> = d[0].hunks.iter().flat_map(|h| &h.lines).filter(|l| l.origin == '-').collect();
        assert!(removed.iter().any(|l| l.content == "beta"));
    }

    #[test]
    fn diff_against_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        let a = Author::galley();
        std::fs::write(dir.path().join("main.tex"), "kept\n").unwrap();
        let c1 = repo.commit_all(&a, "one").unwrap().unwrap().to_string();
        std::fs::write(dir.path().join("main.tex"), "kept\nnew line\n").unwrap();

        let d = repo.diff(Side::Rev(&c1), Side::Workdir, None).unwrap();
        assert_eq!(d.len(), 1);
        assert!(d[0].hunks.iter().flat_map(|h| &h.lines).any(|l| l.origin == '+' && l.content == "new line"));
    }

    #[test]
    fn restore_makes_new_commit_and_reports_changes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::init_or_open(dir.path()).unwrap();
        let a = Author::galley();
        std::fs::write(dir.path().join("main.tex"), "original\n").unwrap();
        let c1 = repo.commit_all(&a, "one").unwrap().unwrap().to_string();
        std::fs::write(dir.path().join("main.tex"), "rewritten\n").unwrap();
        std::fs::write(dir.path().join("extra.tex"), "added later\n").unwrap();
        repo.commit_all(&a, "two").unwrap();
        // Something Galley keeps but never commits.
        std::fs::create_dir_all(dir.path().join(".galley")).unwrap();
        std::fs::write(dir.path().join(".galley/state"), b"keep me").unwrap();

        let (info, changed) = repo.restore(&c1, &a, "restore: one").unwrap();
        assert!(info.is_some());
        assert_eq!(info.unwrap().message, "restore: one");
        // main.tex is back to the old content and extra.tex is removed.
        assert_eq!(std::fs::read_to_string(dir.path().join("main.tex")).unwrap(), "original\n");
        assert!(!dir.path().join("extra.tex").exists());
        assert!(changed.contains(&"main.tex".to_string()));
        assert!(changed.contains(&"extra.tex".to_string()));
        // The Galley internals do not change. History was not rewritten, so there are 3 commits.
        assert_eq!(std::fs::read_to_string(dir.path().join(".galley/state")).unwrap(), "keep me");
        assert_eq!(repo.log(10).unwrap().len(), 3);
    }

    #[test]
    fn reopen_keeps_history_and_excludes() {
        let dir = tempfile::tempdir().unwrap();
        {
            let repo = Repo::init_or_open(dir.path()).unwrap();
            std::fs::write(dir.path().join("a.tex"), "a").unwrap();
            repo.commit_all(&Author::galley(), "edit: a.tex").unwrap();
        }
        let repo = Repo::init_or_open(dir.path()).unwrap();
        assert_eq!(repo.log(10).unwrap().len(), 1);
        let exclude = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches(".galley/").count(), 1);
    }
}
