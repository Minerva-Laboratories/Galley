//! Continuous backup to S3-compatible object storage: Cloudflare R2, Backblaze B2, AWS S3, Tigris
//! or MinIO. A volume on one host is a single point of failure, and a daily snapshot loses up to a
//! day of everyone's work. Every few minutes, each project that changed is packed and uploaded, and
//! so is a consistent copy of the database, which holds the accounts, members, comments and tokens.
//!
//! A backup runs beside the server and never in front of a save. A failed upload is logged, shown
//! in `/api/health`, and retried on the next pass.
//!
//! Layout in the bucket, under the configured prefix:
//! - `galley.db.gz`, the latest database
//! - `daily/<YYYY-MM-DD>/galley.db.gz`, the first database of each day, so a bad change to the
//!   database can be undone
//! - `projects/<id>.tar.gz`, the latest copy of each project: its git repository, its working
//!   files, the live editing state and its settings. Older versions live in the git history inside.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use flate2::Compression;
use rusty_s3::actions::{ListObjectsV2, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use serde::{Deserialize, Serialize};

use crate::config::BackupConfig;
use crate::db::Db;

/// Directories under a project's `.galley` that a build recreates. Backing them up would cost
/// storage and upload time for nothing.
const DERIVED: [&str; 3] = ["build", "pack", "svg"];
/// How long a signed request stays valid. Long enough for a slow upload of a large project.
const SIGNED_FOR: Duration = Duration::from_secs(3600);
const STATE_FILE: &str = "backup-state.json";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup configuration: {0}")]
    Config(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("object storage: {0}")]
    Http(String),
    #[error("database copy: {0}")]
    Db(String),
}

/// What the last pass did, for `/api/health` and `galley backup run`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupStatus {
    pub last_ok: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    pub projects_uploaded: usize,
    pub database_uploaded: bool,
}

/// The fingerprint of each item at its last successful upload. It survives a restart, so a
/// restart does not upload every project again.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Uploaded {
    #[serde(default)]
    items: BTreeMap<String, String>,
    #[serde(default)]
    daily: Option<String>,
}

pub struct Backup {
    bucket: Bucket,
    credentials: Credentials,
    prefix: String,
    http: reqwest::Client,
    data_dir: PathBuf,
    interval: Duration,
    uploaded: Mutex<Uploaded>,
    status: Mutex<BackupStatus>,
}

impl Backup {
    /// `None` when no bucket is configured. Backup is opt-in.
    pub fn from_config(config: &BackupConfig, data_dir: &Path) -> Result<Option<Backup>, BackupError> {
        let Some(resolved) = config.resolve() else {
            return Ok(None);
        };
        let endpoint = url::Url::parse(&resolved.endpoint)
            .map_err(|e| BackupError::Config(format!("endpoint {:?}: {e}", resolved.endpoint)))?;
        let style = if config.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let bucket = Bucket::new(endpoint, style, resolved.bucket, resolved.region)
            .map_err(|e| BackupError::Config(format!("bucket: {e:?}")))?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| BackupError::Http(e.to_string()))?;
        let uploaded = std::fs::read(data_dir.join(STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut prefix = config.prefix.trim_matches('/').to_string();
        if !prefix.is_empty() {
            prefix.push('/');
        }
        Ok(Some(Backup {
            bucket,
            credentials: Credentials::new(resolved.access_key_id, resolved.secret_access_key),
            prefix,
            http,
            data_dir: data_dir.to_path_buf(),
            interval: Duration::from_secs(config.interval_s.max(30)),
            uploaded: Mutex::new(uploaded),
            status: Mutex::new(BackupStatus::default()),
        }))
    }

    pub fn status(&self) -> BackupStatus {
        lock(&self.status).clone()
    }

    /// Run a pass now, then one every interval, for as long as the server runs.
    pub fn spawn(self: Arc<Self>, db: Db) {
        tokio::spawn(async move {
            loop {
                if let Err(e) = self.run_once(&db).await {
                    tracing::warn!(error = %e, "backup pass failed; retrying on the next pass");
                }
                tokio::time::sleep(self.interval).await;
            }
        });
    }

    /// Upload every project that changed since its last upload, and the database if it changed.
    /// An error on one project does not stop the others.
    pub async fn run_once(&self, db: &Db) -> Result<BackupStatus, BackupError> {
        let mut errors = Vec::new();
        let mut projects_uploaded = 0;

        let root = self.data_dir.join("data");
        let ids = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || project_ids(&root)).await.map_err(join_error)??
        };
        for id in ids {
            let dir = root.join(&id);
            let key = format!("projects/{id}");
            let print = {
                let dir = dir.clone();
                tokio::task::spawn_blocking(move || project_fingerprint(&dir)).await.map_err(join_error)??
            };
            if lock(&self.uploaded).items.get(&key) == Some(&print) {
                continue;
            }
            match self.upload_project(&id, &dir).await {
                Ok(()) => {
                    lock(&self.uploaded).items.insert(key, print);
                    projects_uploaded += 1;
                }
                Err(e) => errors.push(format!("project {id}: {e}")),
            }
        }

        let database_uploaded = match self.upload_database(db).await {
            Ok(done) => done,
            Err(e) => {
                errors.push(format!("database: {e}"));
                false
            }
        };
        self.save_state();

        let mut status = lock(&self.status);
        status.projects_uploaded = projects_uploaded;
        status.database_uploaded = database_uploaded;
        if errors.is_empty() {
            status.last_ok = Some(chrono::Utc::now());
            status.last_error = None;
            if projects_uploaded > 0 || database_uploaded {
                tracing::info!(projects = projects_uploaded, database = database_uploaded, "backup uploaded");
            }
            Ok(status.clone())
        } else {
            let message = errors.join("; ");
            status.last_error = Some(message.clone());
            Err(BackupError::Http(message))
        }
    }

    async fn upload_project(&self, id: &str, dir: &Path) -> Result<(), BackupError> {
        let tmp = self.scratch()?;
        let archive = tmp.path().join("project.tar.gz");
        {
            let (dir, archive) = (dir.to_path_buf(), archive.clone());
            tokio::task::spawn_blocking(move || pack_project(&dir, &archive)).await.map_err(join_error)??;
        }
        self.put_file(&format!("projects/{id}.tar.gz"), &archive).await
    }

    /// Returns whether the database changed and was uploaded.
    async fn upload_database(&self, db: &Db) -> Result<bool, BackupError> {
        let print = file_fingerprint(&[self.data_dir.join("galley.db"), self.data_dir.join("galley.db-wal")]);
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let new_day = lock(&self.uploaded).daily.as_deref() != Some(today.as_str());
        if !new_day && lock(&self.uploaded).items.get("galley.db") == Some(&print) {
            return Ok(false);
        }
        let tmp = self.scratch()?;
        let copy = tmp.path().join("galley.db");
        let packed = tmp.path().join("galley.db.gz");
        {
            let (db, copy, packed) = (db.clone(), copy.clone(), packed.clone());
            tokio::task::spawn_blocking(move || -> Result<(), BackupError> {
                // VACUUM INTO writes a consistent copy while the server keeps writing. Copying the
                // file and its WAL by hand can capture a half-applied transaction.
                db.snapshot(&copy).map_err(|e| BackupError::Db(e.to_string()))?;
                gzip_file(&copy, &packed)?;
                Ok(())
            })
            .await
            .map_err(join_error)??;
        }
        self.put_file("galley.db.gz", &packed).await?;
        if new_day {
            self.put_file(&format!("daily/{today}/galley.db.gz"), &packed).await?;
            lock(&self.uploaded).daily = Some(today);
        }
        lock(&self.uploaded).items.insert("galley.db".into(), print);
        Ok(true)
    }

    async fn put_file(&self, key: &str, path: &Path) -> Result<(), BackupError> {
        let url = self.bucket.put_object(Some(&self.credentials), &format!("{}{key}", self.prefix)).sign(SIGNED_FOR);
        let len = tokio::fs::metadata(path).await?.len();
        let file = tokio::fs::File::open(path).await?;
        let response = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .header(reqwest::header::CONTENT_TYPE, "application/gzip")
            .body(reqwest::Body::from(file))
            .send()
            .await
            .map_err(|e| BackupError::Http(e.without_url().to_string()))?;
        check(response, key).await.map(drop)
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, BackupError> {
        let url = self.bucket.get_object(Some(&self.credentials), &format!("{}{key}", self.prefix)).sign(SIGNED_FOR);
        let response = self.http.get(url).send().await.map_err(|e| BackupError::Http(e.without_url().to_string()))?;
        let body = check(response, key).await?.bytes().await;
        body.map(|b| b.to_vec()).map_err(|e| BackupError::Http(e.without_url().to_string()))
    }

    async fn list(&self, under: &str) -> Result<Vec<String>, BackupError> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = ListObjectsV2::new(&self.bucket, Some(&self.credentials));
            action.with_prefix(format!("{}{under}", self.prefix));
            if let Some(t) = &token {
                action.with_continuation_token(t.clone());
            }
            let response = self
                .http
                .get(action.sign(SIGNED_FOR))
                .send()
                .await
                .map_err(|e| BackupError::Http(e.without_url().to_string()))?;
            let text = check(response, under)
                .await?
                .text()
                .await
                .map_err(|e| BackupError::Http(e.without_url().to_string()))?;
            let page = ListObjectsV2::parse_response(&text).map_err(|e| BackupError::Http(format!("listing: {e}")))?;
            keys.extend(page.contents.into_iter().filter_map(|c| c.key.strip_prefix(&self.prefix).map(str::to_string)));
            match page.next_continuation_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        Ok(keys)
    }

    /// Rebuild a data directory from the bucket. It refuses to write into a data directory that
    /// already has a database or projects, so a restore can never overwrite live work.
    pub async fn restore(&self) -> Result<RestoreReport, BackupError> {
        let db_path = self.data_dir.join("galley.db");
        let root = self.data_dir.join("data");
        if db_path.exists() || !project_ids(&root)?.is_empty() {
            return Err(BackupError::Config(format!(
                "{} already holds a database or projects. Restore into an empty data directory.",
                self.data_dir.display()
            )));
        }
        std::fs::create_dir_all(&root)?;
        let db = self.get("galley.db.gz").await?;
        let mut out = std::fs::File::create(&db_path)?;
        std::io::copy(&mut flate2::read::GzDecoder::new(db.as_slice()), &mut out)?;

        let mut report = RestoreReport::default();
        for key in self.list("projects/").await? {
            let Some(id) = key.strip_prefix("projects/").and_then(|k| k.strip_suffix(".tar.gz")) else {
                continue;
            };
            if !is_safe_id(id) {
                report.skipped.push(key);
                continue;
            }
            let bytes = self.get(&key).await?;
            let dest = root.join(id);
            std::fs::create_dir_all(&dest)?;
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
            // `unpack` refuses entries that would land outside `dest`.
            archive.unpack(&dest)?;
            report.projects += 1;
        }
        Ok(report)
    }

    fn scratch(&self) -> Result<tempfile::TempDir, BackupError> {
        let dir = self.data_dir.join("cache/backup");
        std::fs::create_dir_all(&dir)?;
        Ok(tempfile::tempdir_in(dir)?)
    }

    fn save_state(&self) {
        let bytes = match serde_json::to_vec_pretty(&*lock(&self.uploaded)) {
            Ok(b) => b,
            Err(_) => return,
        };
        let path = self.data_dir.join(STATE_FILE);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
            // Losing the state only means the next pass uploads everything again.
            tracing::warn!("could not save the backup state");
        }
    }
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub projects: usize,
    pub skipped: Vec<String>,
}

async fn check(response: reqwest::Response, key: &str) -> Result<reqwest::Response, BackupError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let code = body
        .split("<Code>")
        .nth(1)
        .and_then(|rest| rest.split("</Code>").next())
        .unwrap_or("no detail");
    Err(BackupError::Http(format!("{key}: HTTP {status} ({code})")))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn join_error(e: tokio::task::JoinError) -> BackupError {
    BackupError::Io(std::io::Error::other(e))
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn project_ids(root: &Path) -> std::io::Result<Vec<String>> {
    let mut ids = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && is_safe_id(&name) && entry.path().join(".galley/project.toml").exists() {
            ids.push(name);
        }
    }
    ids.sort();
    Ok(ids)
}

fn is_derived(rel: &Path) -> bool {
    let mut parts = rel.components().map(|c| c.as_os_str().to_string_lossy());
    match (parts.next(), parts.next()) {
        (Some(first), Some(second)) if first == ".galley" => DERIVED.contains(&second.as_ref()),
        _ => false,
    }
}

fn is_temporary(name: &str) -> bool {
    name.ends_with(".galley-tmp")
}

/// A project changes when git gains a commit, tag or branch, or when its live state or settings
/// change. Hashing sizes and times of those files is cheap, so a pass over thousands of quiet
/// projects reads metadata only.
fn project_fingerprint(dir: &Path) -> std::io::Result<String> {
    let mut files = Vec::new();
    for rel in [".git/HEAD", ".git/packed-refs"] {
        files.push(dir.join(rel));
    }
    collect_files(&dir.join(".git/refs"), dir, &mut files, false)?;
    collect_files(&dir.join(".galley"), dir, &mut files, true)?;
    Ok(file_fingerprint(&files))
}

fn collect_files(path: &Path, root: &Path, out: &mut Vec<PathBuf>, skip_derived: bool) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let p = entry.path();
        let rel = p.strip_prefix(root).unwrap_or(&p);
        if skip_derived && is_derived(rel) || is_temporary(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&p, root, out, skip_derived)?;
        } else if kind.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

fn file_fingerprint(files: &[PathBuf]) -> String {
    use std::hash::{Hash, Hasher};
    let mut sorted: Vec<&PathBuf> = files.iter().collect();
    sorted.sort();
    // DefaultHasher is stable within one build of the binary, and a new binary uploading
    // everything once is harmless.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in sorted {
        f.hash(&mut h);
        if let Ok(m) = std::fs::metadata(f) {
            m.len().hash(&mut h);
            m.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

/// Pack a project directory, without build output, into a gzipped tar.
fn pack_project(dir: &Path, archive: &Path) -> Result<(), BackupError> {
    let file = std::fs::File::create(archive)?;
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(file, Compression::fast()));
    tar.follow_symlinks(false);
    add_dir(&mut tar, dir, dir)?;
    tar.into_inner()?.finish()?;
    Ok(())
}

fn add_dir<W: std::io::Write>(tar: &mut tar::Builder<W>, root: &Path, dir: &Path) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(std::io::Error::other)?;
        if is_derived(rel) || is_temporary(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            tar.append_dir(rel, &path)?;
            add_dir(tar, root, &path)?;
        } else if kind.is_file() {
            match std::fs::File::open(&path) {
                Ok(mut f) => tar.append_file(rel, &mut f)?,
                // A file removed between the listing and the read was deleted by an edit. The next
                // pass sees the project as changed and packs it again.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

fn gzip_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut input = std::fs::File::open(from)?;
    let mut gz = flate2::write::GzEncoder::new(std::fs::File::create(to)?, Compression::default());
    std::io::copy(&mut input, &mut gz)?;
    gz.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path, id: &str) -> PathBuf {
        let dir = root.join("data").join(id);
        for d in [".git/refs/heads", ".galley/docs", ".galley/build", "sections"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(dir.join(".git/refs/heads/main"), "aaaa\n").unwrap();
        std::fs::write(dir.join(".galley/project.toml"), "name = \"P\"\n").unwrap();
        std::fs::write(dir.join(".galley/docs/main.tex.ybin"), [1, 2, 3]).unwrap();
        std::fs::write(dir.join(".galley/build/main.pdf"), "pdf").unwrap();
        std::fs::write(dir.join("main.tex"), "\\documentclass{article}").unwrap();
        std::fs::write(dir.join("sections/intro.tex"), "Lorem ipsum").unwrap();
        std::fs::write(dir.join("main.tex.galley-tmp"), "half").unwrap();
        dir
    }

    #[test]
    fn the_archive_holds_sources_and_state_but_not_build_output() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(tmp.path(), "p1");
        let archive = tmp.path().join("p1.tar.gz");
        pack_project(&dir, &archive).unwrap();
        let mut names: Vec<String> = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(&archive).unwrap()))
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(names.contains(&"main.tex".to_string()));
        assert!(names.contains(&"sections/intro.tex".to_string()));
        assert!(names.contains(&".git/refs/heads/main".to_string()));
        assert!(names.contains(&".galley/docs/main.tex.ybin".to_string()));
        assert!(names.contains(&".galley/project.toml".to_string()));
        assert!(!names.iter().any(|n| n.starts_with(".galley/build")), "{names:?}");
        assert!(!names.iter().any(|n| n.ends_with(".galley-tmp")), "{names:?}");
    }

    #[test]
    fn the_fingerprint_follows_commits_and_live_state_but_not_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = project(tmp.path(), "p1");
        let first = project_fingerprint(&dir).unwrap();

        std::fs::write(dir.join(".galley/build/main.pdf"), "a different pdf").unwrap();
        assert_eq!(project_fingerprint(&dir).unwrap(), first, "a build is not a change");

        std::fs::write(dir.join(".git/refs/heads/main"), "bbbbbb\n").unwrap();
        let after_commit = project_fingerprint(&dir).unwrap();
        assert_ne!(after_commit, first, "a commit is a change");

        std::fs::write(dir.join(".galley/docs/main.tex.ybin"), [1, 2, 3, 4]).unwrap();
        assert_ne!(project_fingerprint(&dir).unwrap(), after_commit, "live state is a change");
    }

    #[test]
    fn only_project_directories_are_listed() {
        let tmp = tempfile::tempdir().unwrap();
        project(tmp.path(), "p1");
        project(tmp.path(), "p2");
        std::fs::create_dir_all(tmp.path().join("data/not-a-project")).unwrap();
        assert_eq!(project_ids(&tmp.path().join("data")).unwrap(), vec!["p1", "p2"]);
        assert!(project_ids(&tmp.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn ids_from_the_bucket_cannot_climb_out_of_the_data_directory() {
        assert!(is_safe_id("abc-123_x"));
        assert!(!is_safe_id("../etc"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(""));
    }
}
