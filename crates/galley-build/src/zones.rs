//! Where a build keeps its files. A compile runs untrusted TeX that can create links, pipes and
//! folders wherever it may write, while the server (which may run as root) later reads what the
//! compile made. So the folders are split by who may write them:
//!
//! - `.galley/build` is the server's own: published PDFs, the log, the last result. No compile
//!   ever mounts it.
//! - `.galley/in` holds what Galley writes for a compile (wrappers, latexdiff inputs). Compiles
//!   see it read-only.
//! - `.galley/scratch` is never mounted. Galley creates the folders compiles may write inside
//!   it, and each is mounted on its own. A compile cannot rename or replace a mount point, so the
//!   server only ever opens a top-level name in one of them, through [`crate::buildfs`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The server's own folder inside `.galley`.
const BUILD: &str = "build";
const INPUTS: &str = "in";
const SCRATCH: &str = "scratch";
/// Written into `.galley` once a build folder from before this layout has been removed. That
/// folder was writable by compiles, so nothing in it can be trusted, not even a marker: this one
/// sits outside it, where no compile could ever write.
const MARKER: &str = ".layout-2";

/// Where a compile finds Galley's inputs.
pub const GUEST_IN: &str = "/galley/in";
/// Where a compile writes. Under /tmp, because Ubuntu's Ghostscript AppArmor profile reads and
/// writes PDF and PostScript there and not below /work.
pub const GUEST_OUT: &str = "/tmp/galley-out";
/// Where figure jobs write the figure cache.
pub const GUEST_FIG: &str = "/tmp/galley-fig";
/// Where Tectonic reads its formats, read-only.
pub const GUEST_CACHE: &str = "/galley/cache";
/// Where Tectonic reads its package bundle, read-only.
pub const GUEST_BUNDLE: &str = "/galley/bundle";

pub fn build_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".galley").join(BUILD)
}

pub fn inputs_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".galley").join(INPUTS)
}

fn scratch_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".galley").join(SCRATCH)
}

pub fn fig_dir(project_dir: &Path) -> PathBuf {
    scratch_dir(project_dir).join("fig")
}

/// The per-project package cache of the old on-demand design. Removed at the next build.
fn old_cache_dir(project_dir: &Path) -> PathBuf {
    scratch_dir(project_dir).join("tectonic-cache")
}

/// A name no other run of this process uses.
pub(crate) fn unique(prefix: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!(
        "{prefix}-{}-{}-{nanos}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Make `path` a real folder. Anything else at that name (a link, a file) is removed first.
fn real_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => return Ok(()),
        Ok(_) => std::fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Another run made it at the same moment. Check it is a folder.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(path)?.file_type().is_dir() {
                Ok(())
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

/// Set up the folders of one project before anything compiles there. A build folder from before
/// this layout is deleted, and with it the old figure and package caches.
pub fn prepare(project_dir: &Path) -> std::io::Result<()> {
    // Two first runs at once would otherwise both delete, one under the other's feet.
    static CLEANING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let galley = project_dir.join(".galley");
    real_dir(&galley)?;
    let build = build_dir(project_dir);
    let marker = galley.join(MARKER);
    let _cleaning = CLEANING.lock().unwrap_or_else(|e| e.into_inner());
    if !crate::buildfs::is_file(&marker) {
        // `remove_dir_all` deletes links without following them.
        match std::fs::symlink_metadata(&build) {
            Ok(m) if m.file_type().is_dir() => std::fs::remove_dir_all(&build)?,
            Ok(_) => std::fs::remove_file(&build)?,
            Err(_) => {}
        }
        let _ = std::fs::remove_dir_all(galley.join("fetch"));
        real_dir(&build)?;
        crate::buildfs::write(&marker, "")?;
    } else {
        real_dir(&build)?;
    }
    real_dir(&inputs_dir(project_dir))?;
    real_dir(&scratch_dir(project_dir))?;
    // Packages now come from the engine's bundle. `remove_tree` never follows a link.
    if std::fs::symlink_metadata(old_cache_dir(project_dir)).is_ok() {
        crate::buildfs::remove_tree(&old_cache_dir(project_dir))?;
    }
    Ok(())
}

/// Remove run folders and thrown-away folders older than `age`. A run folder is deleted when its
/// run ends, so an old one was left by a server that stopped mid-build.
pub fn sweep_stale(project_dir: &Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(scratch_dir(project_dir)) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("run-") || name.starts_with("trash-") || name.starts_with(".tectonic-cache-")) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d > age);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Write one of Galley's inputs for a compile. The folder is never writable by a compile.
pub fn write_input(project_dir: &Path, name: &str, text: impl AsRef<[u8]>) -> std::io::Result<PathBuf> {
    prepare(project_dir)?;
    let path = inputs_dir(project_dir).join(name);
    crate::buildfs::write(&path, text)?;
    Ok(path)
}

/// A folder a compile may write. A run folder is deleted when this is dropped. The figure cache
/// and the package cache stay between builds.
#[derive(Debug)]
pub struct Leaf {
    path: PathBuf,
    temporary: bool,
}

impl Leaf {
    /// A fresh, empty folder for one run.
    pub fn run(project_dir: &Path) -> std::io::Result<Leaf> {
        prepare(project_dir)?;
        let path = scratch_dir(project_dir).join(unique("run"));
        std::fs::create_dir(&path)?;
        Ok(Leaf { path, temporary: true })
    }

    /// The figure cache, shared by the figure jobs and the main pass that includes them.
    pub fn fig(project_dir: &Path) -> std::io::Result<Leaf> {
        prepare(project_dir)?;
        let path = fig_dir(project_dir);
        real_dir(&path)?;
        Ok(Leaf { path, temporary: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Throw away a folder that a stopped compile may still be writing, so the next run starts
    /// from an empty one instead of reusing it.
    pub fn discard(&self) {
        if self.temporary {
            return;
        }
        let Some(parent) = self.path.parent() else { return };
        let trash = parent.join(unique("trash"));
        if std::fs::rename(&self.path, &trash).is_ok() {
            let _ = std::fs::remove_dir_all(&trash);
        }
    }
}

impl Drop for Leaf {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// A file or folder of Galley's inputs that is deleted once the run that needed it is over.
#[derive(Debug)]
pub struct Cleanup(PathBuf);

impl Cleanup {
    pub fn new(path: PathBuf) -> Cleanup {
        Cleanup(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        match std::fs::symlink_metadata(&self.0) {
            Ok(m) if m.file_type().is_dir() => {
                let _ = std::fs::remove_dir_all(&self.0);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&self.0);
            }
            Err(_) => {}
        }
    }
}

/// Whether a writable mount for a compile is one of the scratch folders: its parent is
/// `.galley/scratch`. Nothing else may ever be mounted writable.
pub fn is_scratch(path: &Path) -> bool {
    let Some(parent) = path.parent() else { return false };
    parent.file_name().is_some_and(|n| n == SCRATCH)
        && parent.parent().and_then(Path::file_name).is_some_and(|n| n == ".galley")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn an_old_build_folder_is_removed_once_and_links_are_not_followed() {
        let project = tempfile::tempdir().unwrap();
        let victim = tempfile::tempdir().unwrap();
        std::fs::write(victim.path().join("main.tex"), "keep").unwrap();
        let old = project.path().join(".galley/build");
        std::fs::create_dir_all(old.join("published")).unwrap();
        symlink(victim.path(), old.join("current")).unwrap();
        symlink(victim.path().join("main.tex"), old.join("last.log")).unwrap();
        std::fs::write(old.join("last-good.pdf"), "old pdf").unwrap();

        prepare(project.path()).unwrap();
        assert_eq!(std::fs::read_to_string(victim.path().join("main.tex")).unwrap(), "keep");
        assert!(std::fs::symlink_metadata(old.join("current")).is_err());
        assert!(!old.join("last-good.pdf").exists());
        assert!(is_file(&project.path().join(".galley").join(MARKER)));

        // The second time, Galley's own files stay.
        std::fs::write(old.join("last.log"), "new log").unwrap();
        prepare(project.path()).unwrap();
        assert_eq!(std::fs::read_to_string(old.join("last.log")).unwrap(), "new log");
    }

    #[test]
    fn a_marker_planted_by_an_old_compile_does_not_skip_the_cleanup() {
        let project = tempfile::tempdir().unwrap();
        let victim = tempfile::tempdir().unwrap();
        let old = project.path().join(".galley/build");
        std::fs::create_dir_all(&old).unwrap();
        // Before this layout, a compile could write anything into `.galley/build`.
        std::fs::write(old.join(MARKER), "").unwrap();
        symlink(victim.path(), old.join("published")).unwrap();
        prepare(project.path()).unwrap();
        assert!(std::fs::symlink_metadata(old.join("published")).is_err());
    }

    #[test]
    fn the_old_package_cache_is_removed_without_following_a_link() {
        let project = tempfile::tempdir().unwrap();
        let victim = tempfile::tempdir().unwrap();
        std::fs::write(victim.path().join("keep"), "").unwrap();
        prepare(project.path()).unwrap();
        std::fs::create_dir_all(old_cache_dir(project.path()).join("formats")).unwrap();
        prepare(project.path()).unwrap();
        assert!(!old_cache_dir(project.path()).exists());
        symlink(victim.path(), old_cache_dir(project.path())).unwrap();
        prepare(project.path()).unwrap();
        assert!(std::fs::symlink_metadata(old_cache_dir(project.path())).is_err());
        assert!(victim.path().join("keep").exists());
    }

    #[test]
    fn stale_run_folders_are_swept() {
        let project = tempfile::tempdir().unwrap();
        let leaf = Leaf::run(project.path()).unwrap();
        let path = leaf.path().to_path_buf();
        std::mem::forget(leaf);
        sweep_stale(project.path(), std::time::Duration::from_secs(3600));
        assert!(path.exists(), "a recent run folder may still be in use");
        sweep_stale(project.path(), std::time::Duration::ZERO);
        assert!(!path.exists());
        assert!(fig_dir(project.path()).parent().unwrap().exists());
    }

    fn is_file(path: &Path) -> bool {
        crate::buildfs::is_file(path)
    }

    #[test]
    fn galleys_folders_are_never_links() {
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".galley")).unwrap();
        symlink(elsewhere.path(), project.path().join(".galley/scratch")).unwrap();
        symlink(elsewhere.path(), project.path().join(".galley/in")).unwrap();
        prepare(project.path()).unwrap();
        for name in ["scratch", "in", "build"] {
            let meta = std::fs::symlink_metadata(project.path().join(".galley").join(name)).unwrap();
            assert!(meta.file_type().is_dir(), "{name}");
        }
        assert!(std::fs::read_dir(elsewhere.path()).unwrap().next().is_none());
    }

    #[test]
    fn run_folders_are_fresh_and_removed_after_use() {
        let project = tempfile::tempdir().unwrap();
        prepare(project.path()).unwrap();
        let a = Leaf::run(project.path()).unwrap();
        let b = Leaf::run(project.path()).unwrap();
        assert_ne!(a.path(), b.path());
        assert!(is_scratch(a.path()));
        std::fs::write(a.path().join("main.pdf"), "x").unwrap();
        let path = a.path().to_path_buf();
        drop(a);
        assert!(!path.exists());

        let fig = Leaf::fig(project.path()).unwrap();
        std::fs::write(fig.path().join("galley-fig-figure0.pdf"), "x").unwrap();
        drop(fig);
        assert!(fig_dir(project.path()).join("galley-fig-figure0.pdf").exists());
        Leaf::fig(project.path()).unwrap().discard();
        assert!(!fig_dir(project.path()).exists());
        assert!(is_scratch(&fig_dir(project.path())));
        assert!(!is_scratch(&build_dir(project.path())));
        assert!(!is_scratch(&inputs_dir(project.path())));
    }
}
