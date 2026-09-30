//! Projects on disk at `<data_dir>/data/<id>/`. Each project is a git repo with a
//! `.galley/project.toml`. Galley derives the project list from the directory until accounts
//! arrive with SQLite in M3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use galley_sync::{ProjectSync, SyncConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: String,
    pub name: String,
    pub main_file: String,
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

pub struct Registry {
    root: PathBuf,
    sync_config: SyncConfig,
    open: RwLock<HashMap<String, Arc<ProjectSync>>>,
    metadata_lock: Mutex<()>,
}

impl Registry {
    pub fn new(data_dir: &Path, sync_config: SyncConfig) -> std::io::Result<Registry> {
        let root = data_dir.join("data");
        std::fs::create_dir_all(&root)?;
        Ok(Registry {
            root,
            sync_config,
            open: RwLock::new(HashMap::new()),
            metadata_lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> Result<Vec<ProjectMeta>, RegistryError> {
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
        let mut id = base.clone();
        let mut n = 2;
        while self.root.join(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
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
        let name = name.trim();
        let base = slugify(name);
        if base.is_empty() {
            return Err(RegistryError::BadName);
        }
        let mut id = base.clone();
        let mut n = 2;
        while self.root.join(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        let dir = self.root.join(&id);
        std::fs::create_dir_all(dir.join(".galley"))?;
        let count = files.len();
        let mut text_paths = Vec::new();
        for (path, bytes) in &files {
            let file = dir.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(file, bytes)?;
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
            created_at: now,
            updated_at: now,
        };
        write_meta(&dir, &meta)?;

        let project = self.open(&id).await?;
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
        let dir = self.dir_for(id)?;
        let mut open = self.open.write().await;
        if let Some(p) = open.get(id) {
            return Ok(Arc::clone(p));
        }
        let project = ProjectSync::open(id, &dir, self.sync_config.clone())?;
        open.insert(id.to_string(), Arc::clone(&project));
        Ok(project)
    }

    /// Read-modify-write the project's metadata file.
    pub fn update_meta(&self, id: &str, f: impl FnOnce(&mut ProjectMeta)) -> Result<ProjectMeta, RegistryError> {
        let _guard = self.metadata_lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.dir_for(id)?;
        let mut meta = read_meta(&dir)?;
        f(&mut meta);
        meta.updated_at = Utc::now();
        write_meta(&dir, &meta)?;
        Ok(meta)
    }

    pub fn touch(&self, id: &str) {
        let _guard = self.metadata_lock.lock().unwrap_or_else(|e| e.into_inner());
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
    // Readers always see a complete file, even during a rename or a history touch.
    let temporary = dir.join(".galley/project.toml.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(temporary, dir.join(".galley/project.toml"))?;
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
}
