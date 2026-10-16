//! Opening, writing and copying the files of a build. A compile writes only into its own scratch
//! folders (see [`crate::zones`]), and anything it wrote may be a symlink, a pipe or a huge file.
//! So every file Galley reads is opened without following a link and without waiting on a pipe,
//! must be a regular file, and is read up to a size limit. Writes go to a fresh file that is then
//! renamed into place, so a write never goes through a link either.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::runner::{LAST_GOOD_PDF, LAST_GOOD_SYNCTEX, PDF_PRODUCER_JSON};

/// Galley's published generations, inside `.galley/build`.
pub const PUBLISHED: &str = "published";
/// Galley's pointer to the newest generation in `published/`.
pub const CURRENT: &str = "current";

/// The most Galley reads of a PDF or a SyncTeX file.
pub const MAX_PDF: u64 = 1 << 30;
/// The most Galley reads of a log.
pub const MAX_LOG: u64 = 64 << 20;
/// The most Galley reads of a small record such as a figure list or a figure checksum.
pub const MAX_SMALL: u64 = 1 << 20;

/// The links Galley itself keeps at the top of the build folder, with the only target each may have.
const ALIASES: &[(&str, &str)] = &[
    (LAST_GOOD_PDF, "current/paper.pdf"),
    (LAST_GOOD_SYNCTEX, "current/paper.synctex.gz"),
    (PDF_PRODUCER_JSON, "current/producer.json"),
];

fn not_regular() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "not a regular file")
}

fn too_large() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "the file is larger than Galley reads")
}

/// Open `path` for reading only if its last part is a regular file. A symlink is refused by the
/// kernel (`O_NOFOLLOW`), and a pipe opens without waiting for a writer (`O_NONBLOCK`) and is then
/// refused, so a compile cannot make the server read another file or hang.
pub fn open(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

/// Whether `path` is a regular file, without following a link.
pub fn is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Read a regular file of at most `cap` bytes.
pub fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let file = open(path)?;
    if file.metadata()?.len() > cap {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(too_large());
    }
    Ok(bytes)
}

pub fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    read_capped(path, MAX_PDF)
}

pub fn read_to_string(path: &Path) -> std::io::Result<String> {
    String::from_utf8(read_capped(path, MAX_LOG)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A name next to `path` that no other writer uses.
fn temp_next_to(path: &Path) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().ok_or_else(not_regular)?.to_string_lossy();
    Ok(path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )))
}

/// Fill a new file next to `path` with `fill`, then rename it over `path`. A rename replaces a
/// link; it never writes through one.
fn replace(
    path: &Path,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let tmp = temp_next_to(path)?;
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| fill(&mut f));
    match written.and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Write `bytes` to `path` without following a link there.
pub fn write(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    replace(path, |f| f.write_all(bytes.as_ref()))
}

/// Copy a regular file of at most `cap` bytes, streaming it, with no link followed on either side.
pub fn copy_capped(from: &Path, to: &Path, cap: u64) -> std::io::Result<()> {
    let source = open(from)?;
    if source.metadata()?.len() > cap {
        return Err(too_large());
    }
    replace(to, |f| {
        let copied = std::io::copy(&mut source.take(cap + 1), f)?;
        if copied > cap { Err(too_large()) } else { Ok(()) }
    })
}

pub fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    copy_capped(from, to, MAX_PDF)
}

/// The real file behind one of Galley's aliases (`last-good.pdf` and the like), or `None` when
/// the alias or `current` is not what Galley writes.
pub fn resolve(out_dir: &Path, alias: &str) -> Option<PathBuf> {
    let path = out_dir.join(alias);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_file() {
        return Some(path);
    }
    let (_, expected) = ALIASES.iter().find(|(name, _)| *name == alias)?;
    if std::fs::read_link(&path).ok()? != Path::new(expected) {
        return None;
    }
    let generation = current_generation(out_dir)?;
    let file = expected.strip_prefix("current/")?;
    Some(out_dir.join(PUBLISHED).join(generation).join(file))
}

/// The generation `current` points at, if it is a plain name inside `published/`.
fn current_generation(out_dir: &Path) -> Option<String> {
    let target = std::fs::read_link(out_dir.join(CURRENT)).ok()?;
    let generation = target.to_str()?.strip_prefix("published/")?;
    is_plain_name(generation).then(|| generation.to_string())
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn reads_refuse_links_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        std::fs::write(secret.path().join("main.tex"), "secret").unwrap();
        std::fs::write(dir.path().join("last.log"), "log").unwrap();
        assert_eq!(read_to_string(&dir.path().join("last.log")).unwrap(), "log");
        symlink(secret.path().join("main.tex"), dir.path().join("errors.json")).unwrap();
        assert!(read(&dir.path().join("errors.json")).is_err());
        assert!(!is_file(&dir.path().join("errors.json")));
        assert!(read(&dir.path().join("missing")).is_err());
        std::fs::create_dir(dir.path().join("main.pdf")).unwrap();
        assert!(read(&dir.path().join("main.pdf")).is_err());
    }

    #[test]
    fn a_pipe_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("main.log");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        let started = std::time::Instant::now();
        assert!(read(&fifo).is_err());
        assert!(copy(&fifo, &dir.path().join("copy")).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!dir.path().join("copy").exists());
    }

    #[test]
    fn reads_and_copies_stop_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.pdf");
        std::fs::write(&big, vec![b'x'; 2048]).unwrap();
        assert!(read_capped(&big, 1024).is_err());
        assert_eq!(read_capped(&big, 4096).unwrap().len(), 2048);
        let to = dir.path().join("out.pdf");
        assert!(copy_capped(&big, &to, 1024).is_err());
        assert!(!to.exists(), "a refused copy leaves nothing behind");
        copy_capped(&big, &to, 4096).unwrap();
        assert_eq!(std::fs::read(&to).unwrap().len(), 2048);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn writes_replace_links_instead_of_following_them() {
        let dir = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        let victim = secret.path().join("main.tex");
        std::fs::write(&victim, "keep me").unwrap();
        symlink(&victim, dir.path().join("last.log")).unwrap();
        write(&dir.path().join("last.log"), "new log").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        assert!(is_file(&dir.path().join("last.log")));
        assert_eq!(std::fs::read_to_string(dir.path().join("last.log")).unwrap(), "new log");
        symlink(&victim, dir.path().join("copy.pdf")).unwrap();
        copy(&dir.path().join("last.log"), &dir.path().join("copy.pdf")).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
    }

    #[test]
    fn resolve_follows_only_galleys_own_layout() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        std::fs::create_dir_all(out.join("published/7-1-0-5")).unwrap();
        std::fs::write(out.join("published/7-1-0-5/paper.pdf"), "%PDF").unwrap();
        symlink("published/7-1-0-5", out.join(CURRENT)).unwrap();
        symlink("current/paper.pdf", out.join(LAST_GOOD_PDF)).unwrap();
        assert_eq!(resolve(out, LAST_GOOD_PDF), Some(out.join("published/7-1-0-5/paper.pdf")));

        std::fs::remove_file(out.join(CURRENT)).unwrap();
        symlink("/etc", out.join(CURRENT)).unwrap();
        assert_eq!(resolve(out, LAST_GOOD_PDF), None);
        std::fs::remove_file(out.join(CURRENT)).unwrap();
        symlink("published/../../x", out.join(CURRENT)).unwrap();
        assert_eq!(resolve(out, LAST_GOOD_PDF), None);

        std::fs::remove_file(out.join(LAST_GOOD_PDF)).unwrap();
        symlink("/etc/hosts", out.join(LAST_GOOD_PDF)).unwrap();
        assert_eq!(resolve(out, LAST_GOOD_PDF), None);
    }
}
