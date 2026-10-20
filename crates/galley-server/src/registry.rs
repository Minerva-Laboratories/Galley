//! Projects on disk at `<data_dir>/data/<id>/`. Each project is a git repo with a
//! `.galley/project.toml`. Galley derives the project list from the directory until accounts
//! arrive with SQLite in M3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use galley_build::EngineKind;
use galley_sync::{ProjectSync, SyncConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: String,
    pub name: String,
    pub main_file: String,
    #[serde(default)]
    pub engine: EngineKind,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub updated_at: DateTime<Utc>,
    /// Lint rules switched off for this project. The names come from `galley_build::lint::RULES`.
    #[serde(default)]
    pub lint_disabled: Vec<String>,
    /// Submission deadline as YYYY-MM-DD (SPEC §13.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    /// A venue preset name or free text. It drives the deadline pill and the compliance checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    /// Word budgets per section title.
    #[serde(default)]
    pub budgets: std::collections::BTreeMap<String, u32>,
    /// Look bibliography entries up in the open catalogues Crossref and OpenAlex. It is off until
    /// the project asks for it, because it sends citation keys, DOIs and titles to third parties
    /// (docs/RETRIEVAL.md §5).
    #[serde(default)]
    pub literature: bool,
    /// The persistent figure cache (SPEC §13.1). It is on unless the project turns it off.
    #[serde(default = "on")]
    pub figure_cache: bool,
    /// The commit of the "Submitted to ..." checkpoint. History diffs the camera-ready against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_checkpoint: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no project named {0}")]
    NotFound(String),
    #[error("project names need at least one letter or digit")]
    BadName,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Sync(#[from] galley_sync::Error),
    #[error("project metadata is unreadable: {0}")]
    Meta(#[from] toml::de::Error),
}

/// Under the data dir, one empty file per project id ever used, so no id is handed out twice.
pub const RESERVED_IDS_DIR: &str = ".project-ids";

pub struct Registry {
    root: PathBuf,
    deleted_root: PathBuf,
    ids_root: PathBuf,
    sync_config: SyncConfig,
    default_engine: EngineKind,
    meta_lock: Mutex<()>,
    open: RwLock<HashMap<String, Arc<ProjectSync>>>,
}

impl Registry {
    pub fn new(data_dir: &Path, sync_config: SyncConfig) -> std::io::Result<Registry> {
        Self::new_with_engine(data_dir, sync_config, EngineKind::Tectonic)
    }

    pub fn new_with_engine(data_dir: &Path, sync_config: SyncConfig, default_engine: EngineKind) -> std::io::Result<Registry> {
        let root = data_dir.join("data");
        std::fs::create_dir_all(&root)?;
        // Keep interrupted cleanup outside the live project tree. Renaming into this directory
        // makes removal atomic for readers even when wiping a protected file fails halfway.
        let deleted_root = data_dir.join(".deleted-projects");
        match std::fs::create_dir(&deleted_root) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e),
        }
        if !std::fs::symlink_metadata(&deleted_root)?.file_type().is_dir() {
            return Err(std::io::Error::other("deleted-projects storage must be a directory, not a symlink"));
        }
        for entry in std::fs::read_dir(&deleted_root)? {
            let entry = entry?;
            let result = if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())
            } else {
                std::fs::remove_file(entry.path())
            };
            if let Err(e) = result { tracing::warn!(path = %entry.path().display(), error = %e, "will retry deleted project cleanup on next startup"); }
        }
        let ids_root = data_dir.join(RESERVED_IDS_DIR);
        match std::fs::create_dir(&ids_root) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e),
        }
        if !std::fs::symlink_metadata(&ids_root)?.file_type().is_dir() {
            return Err(std::io::Error::other("project-id storage must be a directory, not a symlink"));
        }
        Ok(Registry {
            root,
            deleted_root,
            ids_root,
            sync_config,
            default_engine,
            meta_lock: Mutex::new(()),
            open: RwLock::new(HashMap::new()),
        })
    }

    /// Project links are identities: never reuse a retired slug, even during database cleanup
    /// or after a restart. create_new also makes concurrent imports choose distinct ids.
    fn reserve_id(&self, base: &str) -> Result<String, RegistryError> {
        let mut id = base.to_string();
        let mut n = 2;
        loop {
            if !self.root.join(&id).exists() {
                match std::fs::OpenOptions::new().write(true).create_new(true).open(self.ids_root.join(&id)) {
                    Ok(_) => return Ok(id),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
                    Err(e) => return Err(e.into()),
                }
            }
            id = format!("{base}-{n}");
            n += 1;
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A scratch directory on the same disk as the projects, so a staged file moves into a
    /// project by rename. It is removed when the handle drops.
    pub fn staging(&self) -> std::io::Result<tempfile::TempDir> {
        let dir = self.root.parent().unwrap_or(&self.root).join("cache").join("staging");
        std::fs::create_dir_all(&dir)?;
        tempfile::tempdir_in(dir)
    }

    pub fn list(&self) -> Result<Vec<ProjectMeta>, RegistryError> {
        let _guard = self.meta_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            match read_meta(&entry.path()) {
                Ok(meta) => out.push(meta),
                Err(e) => tracing::warn!(dir = %entry.path().display(), error = %e, "skipping directory without project metadata"),
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
        Ok(out)
    }

    pub fn meta(&self, id: &str) -> Result<ProjectMeta, RegistryError> {
        let _guard = self.meta_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = self.dir_for(id)?;
        read_meta(&dir)
    }

    /// Create a blank-article project and commit the template.
    pub async fn create(&self, name: &str) -> Result<ProjectMeta, RegistryError> {
        self.create_from(name, crate::templates::default()).await
    }

    /// Create a project from a bundled template and commit its files as the first revision.
    pub async fn create_from(&self, name: &str, template: &crate::templates::Template) -> Result<ProjectMeta, RegistryError> {
        let name = name.trim();
        let base = slugify(name);
        if base.is_empty() {
            return Err(RegistryError::BadName);
        }
        let id = self.reserve_id(&base)?;
        let dir = self.root.join(&id);
        std::fs::create_dir_all(dir.join(".galley"))?;
        for (path, text) in template.files {
            let file = dir.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(file, text)?;
        }
        let now = Utc::now();
        let meta = ProjectMeta {
            lint_disabled: Vec::new(),
            deadline: None,
            venue: template.venue.map(str::to_string),
            budgets: Default::default(),
            submitted_checkpoint: None,
            literature: false,
            figure_cache: true,
            id: id.clone(),
            name: name.to_string(),
            main_file: template.main_file.into(),
            engine: self.default_engine,
            created_at: now,
            updated_at: now,
        };
        write_meta(&dir, &meta)?;

        let project = self.open(&id).await?;
        // Seed every document so the first flush commits the template as it was written.
        for (path, _) in template.files {
            project.doc(path).await?;
        }
        project.flush_now().await?;
        Ok(meta)
    }

    /// Create a project from imported files, such as an unpacked zip. It mirrors `create_from`: the
    /// files land on disk first and the project commits them once, so history starts with the
    /// project as it arrived rather than one commit per file.
    pub async fn create_imported(
        &self,
        name: &str,
        files: Vec<(String, Vec<u8>)>,
        main_file: &str,
    ) -> Result<ProjectMeta, RegistryError> {
        let files = files.into_iter().map(|(p, b)| (p, crate::import::FileData::Bytes(b))).collect();
        self.create_imported_with_folders(name, files, Vec::new(), main_file).await
    }

    pub async fn create_imported_with_folders(
        &self,
        name: &str,
        files: Vec<(String, crate::import::FileData)>,
        folders: Vec<String>,
        main_file: &str,
    ) -> Result<ProjectMeta, RegistryError> {
        let name = name.trim();
        let base = slugify(name);
        if base.is_empty() {
            return Err(RegistryError::BadName);
        }
        let id = self.reserve_id(&base)?;
        let dir = self.root.join(&id);
        std::fs::create_dir_all(dir.join(".galley"))?;
        let count = files.len();
        let mut text_paths = Vec::new();
        for (path, data) in &files {
            let file = dir.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match data {
                crate::import::FileData::Bytes(bytes) => std::fs::write(file, bytes)?,
                // Staging sits in the data dir, so this is normally a rename and copies nothing.
                crate::import::FileData::Staged(from) => {
                    if std::fs::rename(from, &file).is_err() {
                        std::fs::copy(from, &file)?;
                    }
                }
            }
            if galley_sync::paths::is_text_path(path) {
                text_paths.push(path.clone());
            }
        }
        let now = Utc::now();
        let meta = ProjectMeta {
            lint_disabled: Vec::new(),
            deadline: None,
            venue: None,
            budgets: Default::default(),
            submitted_checkpoint: None,
            literature: false,
            figure_cache: true,
            id: id.clone(),
            name: name.to_string(),
            main_file: main_file.to_string(),
            engine: self.default_engine,
            created_at: now,
            updated_at: now,
        };
        write_meta(&dir, &meta)?;

        let project = self.open(&id).await?;
        for folder in &folders {
            project.create_folder(folder)?;
        }
        for path in &text_paths {
            project.doc(path).await?;
        }
        project.flush_with(Some(format!("import: {count} files from a zip"))).await?;
        Ok(meta)
    }

    pub async fn open(&self, id: &str) -> Result<Arc<ProjectSync>, RegistryError> {
        if let Some(p) = self.open.read().await.get(id) {
            return Ok(Arc::clone(p));
        }
        let mut open = self.open.write().await;
        if let Some(p) = open.get(id) {
            return Ok(Arc::clone(p));
        }
        let dir = self.dir_for(id)?;
        let project = ProjectSync::open(id, &dir, self.sync_config.clone())?;
        open.insert(id.to_string(), Arc::clone(&project));
        Ok(project)
    }

    /// Remove a project only after its live writer has stopped. Never traverse a project symlink.
    pub async fn delete(&self, id: &str) -> Result<(), RegistryError> {
        let mut open = self.open.write().await;
        let dir = self.dir_for(id)?;
        if !std::fs::symlink_metadata(&dir)?.file_type().is_dir() {
            return Err(RegistryError::NotFound(id.to_string()));
        }
        // Older projects predate reservations. Retain their identity before the live path moves.
        match std::fs::OpenOptions::new().write(true).create_new(true).open(self.ids_root.join(id)) {
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(e) => return Err(e.into()),
        }
        if let Some(project) = open.remove(id) { project.shutdown().await?; }
        let retired = self.deleted_root.join(format!("{id}-{}", crate::auth::new_id()));
        {
            let _guard = self.meta_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            std::fs::rename(dir, &retired)?;
        }
        drop(open);
        // The project is gone from the live namespace. A failed disk cleanup must not leave a
        // half-deleted project visible; startup retries retired directories.
        if let Err(e) = std::fs::remove_dir_all(&retired) {
            tracing::warn!(path = %retired.display(), error = %e, "will retry deleted project cleanup on next startup");
        }
        Ok(())
    }

    /// Read-modify-write the project's metadata file.
    pub fn update_meta(&self, id: &str, f: impl FnOnce(&mut ProjectMeta)) -> Result<ProjectMeta, RegistryError> {
        let _guard = self.meta_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = self.dir_for(id)?;
        let mut meta = read_meta(&dir)?;
        f(&mut meta);
        meta.updated_at = Utc::now();
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    pub fn touch(&self, id: &str) {
        let _guard = self.meta_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(dir) = self.dir_for(id) {
            if let Ok(mut meta) = read_meta(&dir) {
                meta.updated_at = Utc::now();
                let _ = write_meta(&dir, &meta);
            }
        }
    }

    fn dir_for(&self, id: &str) -> Result<PathBuf, RegistryError> {
        if !is_valid_id(id) {
            return Err(RegistryError::NotFound(id.to_string()));
        }
        let dir = self.root.join(id);
        if !dir.join(".galley/project.toml").exists() {
            return Err(RegistryError::NotFound(id.to_string()));
        }
        Ok(dir)
    }
}

fn on() -> bool {
    true
}

fn read_meta(dir: &Path) -> Result<ProjectMeta, RegistryError> {
    let text = std::fs::read_to_string(dir.join(".galley/project.toml"))?;
    Ok(toml::from_str(&text)?)
}

fn write_meta(dir: &Path, meta: &ProjectMeta) -> Result<(), RegistryError> {
    let text = toml::to_string(meta).expect("metadata serialises");
    std::fs::create_dir_all(dir.join(".galley"))?;
    std::fs::write(dir.join(".galley/project.toml"), text)?;
    Ok(())
}

pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

/// Fold an accented Latin letter to its base, so "robóticos" becomes "roboticos" in a URL rather
/// than "rob-ticos". Letters outside this set still become a dash.
fn fold_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' | 'ã' | 'å' | 'Á' | 'À' | 'Ä' | 'Â' | 'Ã' | 'Å' => 'a',
        'é' | 'è' | 'ë' | 'ê' | 'É' | 'È' | 'Ë' | 'Ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' | 'Í' | 'Ì' | 'Ï' | 'Î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' | 'Ó' | 'Ò' | 'Ö' | 'Ô' | 'Õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' | 'Ú' | 'Ù' | 'Ü' | 'Û' => 'u',
        'ñ' | 'Ñ' => 'n',
        'ç' | 'Ç' => 'c',
        other => other,
    }
}

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars().map(fold_accent) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.truncate(48);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Sparse attention (thesis)"), "sparse-attention-thesis");
        assert_eq!(slugify("  Hello,   World!  "), "hello-world");
        assert_eq!(slugify("***"), "");
        assert_eq!(slugify("Control predictivo para brazos robóticos"), "control-predictivo-para-brazos-roboticos");
        assert_eq!(slugify("Año de Señales"), "ano-de-senales");
        assert!(is_valid_id("sparse-attention"));
        assert!(!is_valid_id("../x"));
        assert!(!is_valid_id("Caps"));
    }

    #[tokio::test]
    async fn create_list_open() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path(), SyncConfig::default()).unwrap();
        let a = reg.create("My thesis").await.unwrap();
        let b = reg.create("My thesis").await.unwrap();
        assert_eq!(a.id, "my-thesis");
        assert_eq!(b.id, "my-thesis-2");
        assert!(reg.create("!!!").await.is_err());

        let list = reg.list().unwrap();
        assert_eq!(list.len(), 2);
        let project = reg.open("my-thesis").await.unwrap();
        assert_eq!(project.history(10).await.unwrap().len(), 1);
        let files: Vec<String> = project
            .list_files()
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(files, vec!["main.tex", "refs.bib"]);
        assert!(reg.open("nope").await.is_err());
    }

    #[tokio::test]
    async fn failed_retirement_leaves_project_reopenable_and_startup_retries_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path(), SyncConfig::default()).unwrap();
        let meta = reg.create("Protected project").await.unwrap();
        let original = reg.open(&meta.id).await.unwrap();
        // A failure to rename must leave the original project intact and able to reopen.
        std::fs::remove_dir(&reg.deleted_root).unwrap();
        std::fs::write(&reg.deleted_root, "blocked").unwrap();
        assert!(reg.delete(&meta.id).await.is_err());
        assert!(original.is_deleted());
        let reopened = reg.open(&meta.id).await.unwrap();
        assert!(!reopened.is_deleted());
        assert!(reopened.exists("main.tex"));
        assert!(!reopened.doc("main.tex").await.unwrap().text().await.is_empty());
        std::fs::remove_file(&reg.deleted_root).unwrap();
        std::fs::create_dir(&reg.deleted_root).unwrap();
        reg.delete(&meta.id).await.unwrap();
        assert!(reg.list().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&reg.deleted_root).unwrap().count(), 0);
        // Reproduce an interrupted cleanup, including a symlink to unrelated data.
        let retired = reg.deleted_root.join("leftover");
        std::fs::create_dir(&retired).unwrap();
        std::fs::write(retired.join("partial.tex"), "leftover").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.tex"), "keep").unwrap();
        #[cfg(unix)] {
            std::os::unix::fs::symlink(outside.path(), retired.join("linked")).unwrap();
        }
        let restarted = Registry::new(dir.path(), SyncConfig::default()).unwrap();
        let replacement = restarted.create("Protected project").await.unwrap();
        assert_ne!(replacement.id, meta.id, "retired project ids must never be reused");
        assert_eq!(std::fs::read_dir(&restarted.deleted_root).unwrap().count(), 0);
        assert!(outside.path().join("keep.tex").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deletion_rejects_traversal_and_project_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path(), SyncConfig::default()).unwrap();
        let external = Registry::new(outside.path(), SyncConfig::default()).unwrap();
        let meta = external.create("External paper").await.unwrap();
        let target = external.root().join(&meta.id);
        symlink(&target, reg.root().join("linked")).unwrap();
        assert!(reg.delete("linked").await.is_err());
        assert!(reg.delete("../external-paper").await.is_err());
        assert!(target.join("main.tex").exists());
        assert_eq!(external.list().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn old_projects_default_to_tectonic_and_new_projects_use_configured_engine() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new_with_engine(dir.path(), SyncConfig::default(), EngineKind::XeLatex)
            .unwrap();
        let project = reg.create("Engine test").await.unwrap();
        assert_eq!(project.engine, EngineKind::XeLatex);
        let imported = reg
            .create_imported(
                "Imported engine test",
                vec![(
                    "main.tex".into(),
                    b"\\documentclass{article}\\begin{document}Hi\\end{document}".to_vec(),
                )],
                "main.tex",
            )
            .await
            .unwrap();
        assert_eq!(imported.engine, EngineKind::XeLatex);
        let path = reg.root().join(&project.id).join(".galley/project.toml");
        let old = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            old.lines()
                .filter(|line| !line.starts_with("engine ="))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(reg.meta(&project.id).unwrap().engine, EngineKind::Tectonic);
    }
}
