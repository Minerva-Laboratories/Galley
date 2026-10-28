//! Installs the Tectonic engine: the pinned static binary and Tectonic's whole package bundle,
//! unpacked into one read-only folder. Compiles then never need the network. Everything is pinned
//! by hash in `deploy/tectonic.pins`, which the image build reads too, so every install compiles in
//! the same way. Change the pins only on purpose.
//!
//! The engine lives in `<dir>/engine-<KEY>/` with `tectonic`, `bundle/` and `cache/formats/`. The
//! key names the bundle and the binary, so formats built for one are never used by another. The
//! folder is filled under a temporary name and renamed into place in one step, so a stopped install
//! never leaves a folder that looks complete.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Error, Result};

const PINS: &str = include_str!("../../../deploy/tectonic.pins");

/// The most redirects a download may follow. Each must stay on https.
const MAX_REDIRECTS: usize = 5;

/// The largest release archive accepted for the binary. The pinned ones are about 10 MB.
const MAX_ARCHIVE: u64 = 64 << 20;

/// Building the formats takes about a second on one core. This leaves room for a slow machine.
const FORMAT_LIMIT: Duration = Duration::from_secs(600);

/// The pinned engine for this machine's architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    pub version: String,
    pub archive_sha256: String,
    pub binary_sha256: String,
    pub bundle_url: String,
    pub bundle_size: u64,
    pub bundle_sha256: String,
    pub bundle_digest: String,
    pub bundle_entries: u64,
    pub key: String,
    pub triple: &'static str,
}

impl Pins {
    /// The release archive of the binary on GitHub.
    pub fn archive_url(&self) -> String {
        format!(
            "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%40{v}/tectonic-{v}-{t}.tar.gz",
            v = self.version,
            t = self.triple
        )
    }

    /// The name of the engine folder.
    pub fn folder(&self) -> String {
        format!("engine-{}", self.key)
    }
}

fn parse_pins(text: &str, arch: &str) -> std::result::Result<Pins, String> {
    let mut map = std::collections::HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line.split_once('=').ok_or_else(|| format!("unreadable pin line: {line}"))?;
        map.insert(name.trim(), value.trim());
    }
    let (prefix, triple) = match arch {
        "x86_64" => ("X86_64", "x86_64-unknown-linux-musl"),
        "aarch64" => ("AARCH64", "aarch64-unknown-linux-musl"),
        other => return Err(format!("no pinned Tectonic build for {other}")),
    };
    let get = |name: &str| map.get(name).map(|v| v.to_string()).ok_or_else(|| format!("missing pin {name}"));
    let number = |name: &str| get(name)?.parse::<u64>().map_err(|_| format!("pin {name} is not a number"));
    let pins = Pins {
        version: get("TECTONIC_VERSION")?,
        archive_sha256: get(&format!("TECTONIC_{prefix}_ARCHIVE_SHA256"))?,
        binary_sha256: get(&format!("TECTONIC_{prefix}_BINARY_SHA256"))?,
        bundle_url: get("BUNDLE_URL")?,
        bundle_size: number("BUNDLE_SIZE")?,
        bundle_sha256: get("BUNDLE_SHA256")?,
        bundle_digest: get("BUNDLE_DIGEST")?,
        bundle_entries: number("BUNDLE_ENTRIES")?,
        key: get(&format!("KEY_{prefix}"))?,
        triple,
    };
    let expected = format!("{}-{}", &pins.bundle_digest[..16.min(pins.bundle_digest.len())], &pins.binary_sha256[..16.min(pins.binary_sha256.len())]);
    if pins.key != expected {
        return Err(format!("pin KEY_{prefix} is {}, but the hashes give {expected}", pins.key));
    }
    if !pins.bundle_url.starts_with("https://") {
        return Err("pin BUNDLE_URL must be https".into());
    }
    Ok(pins)
}

/// The pins for this machine, or why there are none (an architecture Tectonic has no build for).
pub fn pins() -> Result<Pins> {
    parse_pins(PINS, std::env::consts::ARCH).map_err(Error::Download)
}

/// Whether `root` is a complete engine for `pins`: the binary, the bundle with the pinned digest,
/// and a finished format file. No hashing: only an install writes this folder.
pub fn is_complete(root: &Path, pins: &Pins) -> bool {
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    regular(&root.join("tectonic"))
        && crate::buildfs::read(&root.join("bundle").join("SHA256SUM"))
            .is_ok_and(|b| String::from_utf8_lossy(&b).trim() == pins.bundle_digest)
        && crate::engine::Tectonic::has_formats(&root.join("cache"))
}

/// Where a bundle comes from: the pinned URL, or a copy of the same tar on disk (CI, the image
/// build, a machine without network).
pub enum BundleSource {
    Download,
    File(PathBuf),
}

/// Install the engine into `dir/engine-<KEY>` and return that folder. An engine that is already
/// complete is kept. `progress` receives short status lines.
pub async fn install_engine(dir: &Path, source: BundleSource, progress: &(dyn Fn(String) + Send + Sync)) -> Result<PathBuf> {
    let pins = pins()?;
    let target = dir.join(pins.folder());
    std::fs::create_dir_all(dir)?;
    if is_complete(&target, &pins) {
        remove_old(dir, &pins);
        return Ok(target);
    }
    sweep_partials(dir, STALE_PARTIAL);
    let partial = dir.join(crate::zones::unique(".engine") + ".partial");
    std::fs::create_dir(&partial)?;
    let filled = fill(&partial, &pins, source, progress).await;
    let placed = filled.and_then(|()| place(&partial, &target, &pins));
    if placed.is_err() {
        let _ = crate::buildfs::remove_tree(&partial);
    }
    placed?;
    remove_old(dir, &pins);
    progress("Tectonic ready".into());
    Ok(target)
}

async fn fill(partial: &Path, pins: &Pins, source: BundleSource, progress: &(dyn Fn(String) + Send + Sync)) -> Result<()> {
    let client = client()?;
    progress(format!("Downloading Tectonic {}…", pins.version));
    let archive = download_small(&client, &pins.archive_url(), MAX_ARCHIVE).await?;
    if sha256_hex(&archive) != pins.archive_sha256 {
        return Err(Error::Download("the Tectonic archive does not match its pinned hash".into()));
    }
    let binary = partial.join("tectonic");
    {
        let (binary, pins) = (binary.clone(), pins.clone());
        tokio::task::spawn_blocking(move || unpack_binary(&archive, &binary, &pins))
            .await
            .map_err(|e| Error::Download(e.to_string()))??;
    }

    let bundle = partial.join("bundle");
    std::fs::create_dir(&bundle)?;
    match source {
        BundleSource::File(path) => {
            progress(format!("Unpacking the package bundle from {}…", path.display()));
            let (bundle, pins) = (bundle.clone(), pins.clone());
            tokio::task::spawn_blocking(move || -> Result<()> {
                let file = std::fs::File::open(&path)?;
                unpack_bundle(std::io::BufReader::with_capacity(1 << 20, file), &bundle, &pins)
            })
            .await
            .map_err(|e| Error::Download(e.to_string()))??;
        }
        BundleSource::Download => {
            progress(format!("Downloading the package bundle ({:.1} GB)…", pins.bundle_size as f64 / 1e9));
            download_bundle(&client, &bundle, pins, progress).await?;
        }
    }

    progress("Preparing the formats…".into());
    build_formats(partial).await?;
    Ok(())
}

fn client() -> Result<reqwest::Client> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if attempt.url().scheme() != "https" {
            attempt.error("a redirect left https")
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .user_agent(concat!("galley/", env!("CARGO_PKG_VERSION")))
        .redirect(policy)
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Download(e.to_string()))
}

async fn download_small(client: &reqwest::Client, url: &str, cap: u64) -> Result<Vec<u8>> {
    let mut res = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Download(format!("{e}. Check the network.")))?;
    if !res.status().is_success() {
        return Err(Error::Download(format!("{url} answered {}", res.status())));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| Error::Download(e.to_string()))? {
        if (bytes.len() + chunk.len()) as u64 > cap {
            return Err(Error::Download(format!("{url} sent more than expected")));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Take the `tectonic` binary out of the release archive and check it against its pin.
fn unpack_binary(archive: &[u8], binary: &Path, pins: &Pins) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let named = entry.path()?.file_name().and_then(|n| n.to_str()) == Some("tectonic");
        if !named || entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let mut hasher = Sha256::new();
        let mut out = create_new(binary, 0o555)?;
        copy_hashing(&mut (&mut entry).take(MAX_ARCHIVE * 4), &mut out, &mut hasher)?;
        out.sync_all()?;
        if hex(&hasher.finalize()) != pins.binary_sha256 {
            return Err(Error::Download("the Tectonic binary does not match its pinned hash".into()));
        }
        return Ok(());
    }
    Err(Error::Download("the archive did not contain a tectonic binary".into()))
}

/// Stream the bundle into the unpacker as it arrives. The tar is read on a blocking thread from a
/// channel, so 2.9 GB never sits in memory.
async fn download_bundle(client: &reqwest::Client, bundle: &Path, pins: &Pins, progress: &(dyn Fn(String) + Send + Sync)) -> Result<()> {
    let mut res = client
        .get(&pins.bundle_url)
        .send()
        .await
        .map_err(|e| Error::Download(format!("{e}. Check the network, or use --bundle-file.")))?;
    if !res.status().is_success() {
        return Err(Error::Download(format!("{} answered {}", pins.bundle_url, res.status())));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(64);
    let unpacker = {
        let (bundle, pins) = (bundle.to_path_buf(), pins.clone());
        tokio::task::spawn_blocking(move || unpack_bundle(ChannelReader { rx, buf: Vec::new(), at: 0 }, &bundle, &pins))
    };
    let (mut received, mut shown) = (0u64, 0u64);
    loop {
        let chunk = match res.chunk().await {
            Ok(Some(chunk)) => Ok(chunk.to_vec()),
            Ok(None) => break,
            Err(e) => Err(std::io::Error::other(format!("the download was cut off: {e}"))),
        };
        let failed = chunk.is_err();
        if let Ok(c) = &chunk {
            received += c.len() as u64;
        }
        // A closed channel means the unpacker stopped; its error is the one reported.
        if tx.send(chunk).await.is_err() || failed {
            break;
        }
        let tenth = received * 10 / pins.bundle_size.max(1);
        if tenth > shown {
            shown = tenth;
            progress(format!("Downloaded {}% of the package bundle", (tenth * 10).min(100)));
        }
    }
    drop(tx);
    unpacker.await.map_err(|e| Error::Download(e.to_string()))?
}

/// A `Read` over the chunks a download sends.
struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    at: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.buf.len() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.buf = chunk;
                    self.at = 0;
                }
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Hashes and counts every byte read through it, and refuses to read past `cap`.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    read: u64,
    cap: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        self.read += n as u64;
        if self.read > self.cap {
            return Err(std::io::Error::other("the package bundle is larger than its pinned size"));
        }
        self.hasher.update(&out[..n]);
        Ok(n)
    }
}

/// Unpack the bundle tar into `bundle`. Only regular files with a plain one-part name are
/// accepted, each created new (a link or a duplicate name fails), and the whole stream, including
/// anything after the tar's end marker, must match the pinned size and hash.
fn unpack_bundle<R: Read>(reader: R, bundle: &Path, pins: &Pins) -> Result<()> {
    let bad = |m: String| Error::Download(format!("the package bundle is not the pinned one: {m}"));
    let mut archive = tar::Archive::new(Hashing { inner: reader, hasher: Sha256::new(), read: 0, cap: pins.bundle_size });
    let mut entries = 0u64;
    let mut digest = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        entries += 1;
        if entries > pins.bundle_entries {
            return Err(bad("more files than pinned".into()));
        }
        if entry.header().entry_type() != tar::EntryType::Regular {
            return Err(bad("an entry that is not a regular file".into()));
        }
        let name = plain_name(&entry.path()?).ok_or_else(|| bad("an entry name that is not one plain part".into()))?;
        let size = entry.header().size()?;
        let path = bundle.join(&name);
        let mut out = create_new(&path, 0o444)?;
        let copied = std::io::copy(&mut (&mut entry).take(size), &mut out)?;
        if copied != size {
            return Err(bad(format!("{name} is cut short")));
        }
        if name == "SHA256SUM" {
            digest = Some(crate::buildfs::read(&path).map(|b| String::from_utf8_lossy(&b).trim().to_string())?);
        }
    }
    let mut rest = archive.into_inner();
    std::io::copy(&mut rest, &mut std::io::sink())?;
    if rest.read != pins.bundle_size {
        return Err(bad(format!("{} bytes instead of {}", rest.read, pins.bundle_size)));
    }
    if hex(&rest.hasher.finalize()) != pins.bundle_sha256 {
        return Err(bad("its hash differs".into()));
    }
    if entries != pins.bundle_entries {
        return Err(bad(format!("{entries} files instead of {}", pins.bundle_entries)));
    }
    if digest.as_deref() != Some(pins.bundle_digest.as_str()) {
        return Err(bad("its SHA256SUM differs".into()));
    }
    Ok(())
}

/// A name that is a single ordinary path part, without control characters.
fn plain_name(path: &Path) -> Option<String> {
    let mut parts = path.components();
    let only = match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(n)), None) => n.to_str()?,
        _ => return None,
    };
    (!only.is_empty() && !only.chars().any(|c| c.is_control() || c == '/' || c == '\\')).then(|| only.to_string())
}

/// Create a file that must not exist yet, without following a link, with `mode`.
fn create_new(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(mode)
        .open(path)
}

fn copy_hashing(from: &mut impl Read, to: &mut impl Write, hasher: &mut Sha256) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n])?;
        total += n as u64;
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the format file once, with the verified binary and bundle, on a constant document of
/// Galley's own. It runs without network when bubblewrap works here. Where it does not (a
/// `docker build`, Ubuntu's AppArmor rule), it runs unsandboxed: no input comes from an author.
/// This is the one child process not started through [`crate::sandbox::Sandbox`], because no
/// project exists yet.
async fn build_formats(partial: &Path) -> Result<()> {
    let work = partial.join("work");
    std::fs::create_dir(&work)?;
    let doc = work.join("galley-format.tex");
    crate::buildfs::write(&doc, "\\documentclass{article}\n\\begin{document}\nGalley\n\\end{document}\n")?;
    let cache = partial.join("cache");
    std::fs::create_dir(&cache)?;
    let tectonic = partial.join("tectonic");
    let args: Vec<std::ffi::OsString> = vec![
        "-X".into(),
        "compile".into(),
        "-b".into(),
        partial.join("bundle").into(),
        "--only-cached".into(),
        "-o".into(),
        work.clone().into(),
        doc.clone().into(),
    ];
    let run = |sandboxed: bool| {
        let mut cmd = if sandboxed {
            let mut c = tokio::process::Command::new("bwrap");
            c.args(["--dev-bind", "/", "/", "--unshare-net", "--die-with-parent", "--"]).arg(&tectonic);
            c
        } else {
            tokio::process::Command::new(&tectonic)
        };
        cmd.args(&args)
            .env("TECTONIC_CACHE_DIR", &cache)
            .env("XDG_CACHE_HOME", &work)
            .env("HOME", &work)
            .env("SOURCE_DATE_EPOCH", "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        async move {
            match tokio::time::timeout(FORMAT_LIMIT, cmd.output()).await {
                Ok(Ok(out)) if out.status.success() => Ok(()),
                Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("failed").to_string()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("timed out".into()),
            }
        }
    };
    let sandboxed = run(true).await;
    let outcome = match sandboxed {
        Ok(()) => Ok(()),
        Err(_) => run(false).await,
    };
    if let Err(e) = outcome {
        return Err(Error::Engine(format!("Tectonic could not prepare its formats: {e}")));
    }
    if !crate::engine::Tectonic::has_formats(&cache) {
        return Err(Error::Engine("Tectonic ran but left no format file".into()));
    }
    crate::buildfs::remove_tree(&work)?;
    Ok(())
}

/// Make the engine read-only and move it into place. Two installs at once: the second finds the
/// first one's complete folder and keeps it.
fn place(partial: &Path, target: &Path, pins: &Pins) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let formats = partial.join("cache").join("formats");
    for entry in std::fs::read_dir(&formats)?.flatten() {
        if entry.file_type()?.is_file() {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o444))?;
        }
    }
    for dir in [formats, partial.join("cache"), partial.join("bundle")] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))?;
    }
    // The engine folder itself only once it is in place: some systems refuse to move a folder
    // that is not writable.
    match std::fs::rename(partial, target) {
        Ok(()) => Ok(std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o555))?),
        Err(_) if is_complete(target, pins) => {
            let _ = crate::buildfs::remove_tree(partial);
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// A partial folder untouched this long belongs to an install that stopped. A younger one may be
/// an install still running in another process.
pub const STALE_PARTIAL: Duration = Duration::from_secs(6 * 3600);

/// Remove what a stopped install left: partial folders older than `age`.
pub fn sweep_partials(dir: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let old = std::fs::symlink_metadata(entry.path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d >= age);
        if name.starts_with(".engine") && name.ends_with(".partial") && old {
            let _ = crate::buildfs::remove_tree(&entry.path());
        }
    }
}

/// Tidy `dir` at startup: leftovers of stopped installs and of the old design go.
pub fn tidy(dir: &Path) {
    let Ok(pins) = pins() else { return };
    sweep_partials(dir, STALE_PARTIAL);
    remove_old(dir, &pins);
}

/// Remove engines with another key (an upgrade would otherwise keep 2.9 GB it no longer uses) and
/// what the old on-demand design kept: the downloaded binary, the shared package cache and its
/// trust marker.
pub fn remove_old(dir: &Path, pins: &Pins) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let current = pins.folder();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let old = (name.starts_with("engine-") && name != current)
            || matches!(name.as_str(), "tectonic" | "cache" | ".cache-trusted-1" | ".tectonic.download")
            || name.starts_with(".cache-discarded-");
        if old {
            match crate::buildfs::remove_tree(&entry.path()) {
                Ok(()) => tracing::info!(path = %entry.path().display(), "removed an old Tectonic file"),
                Err(e) => tracing::warn!(path = %entry.path().display(), error = %e, "an old Tectonic file could not be removed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pins(tar: &[u8], entries: u64, digest: &str) -> Pins {
        Pins {
            version: "0".into(),
            archive_sha256: String::new(),
            binary_sha256: String::new(),
            bundle_url: "https://example.invalid/b.tar".into(),
            bundle_size: tar.len() as u64,
            bundle_sha256: sha256_hex(tar),
            bundle_digest: digest.into(),
            bundle_entries: entries,
            key: "k".into(),
            triple: "t",
        }
    }

    fn header(name: &[u8], size: u64, kind: tar::EntryType) -> tar::Header {
        let mut h = tar::Header::new_old();
        h.as_old_mut().name[..name.len()].copy_from_slice(name);
        h.set_size(size);
        h.set_mode(0);
        h.set_entry_type(kind);
        h.set_cksum();
        h
    }

    fn tar_of(entries: &[(&[u8], &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, body, kind) in entries {
            let h = header(name, body.len() as u64, *kind);
            b.append(&h, *body).unwrap();
        }
        b.into_inner().unwrap()
    }

    const DIGEST: &str = "abc";

    fn good() -> Vec<u8> {
        tar_of(&[(b"a.sty", b"\\relax", tar::EntryType::Regular), (b"SHA256SUM", b"abc\n", tar::EntryType::Regular)])
    }

    fn unpack(tar: &[u8], pins: &Pins) -> (tempfile::TempDir, Result<()>) {
        let dir = tempfile::tempdir().unwrap();
        let out = unpack_bundle(tar, dir.path(), pins);
        (dir, out)
    }

    #[test]
    fn the_pins_file_parses_and_its_keys_match_the_hashes() {
        for arch in ["x86_64", "aarch64"] {
            let p = parse_pins(PINS, arch).unwrap();
            assert_eq!(p.key, format!("{}-{}", &p.bundle_digest[..16], &p.binary_sha256[..16]));
            assert!(p.archive_url().starts_with("https://github.com/"));
        }
        assert!(parse_pins(PINS, "riscv64").is_err());
        let wrong = PINS.replace("KEY_X86_64=6ffe", "KEY_X86_64=0ffe");
        assert!(parse_pins(&wrong, "x86_64").is_err());
    }

    #[test]
    fn a_pinned_bundle_unpacks_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let tar = good();
        let (dir, out) = unpack(&tar, &test_pins(&tar, 2, DIGEST));
        out.unwrap();
        let mode = std::fs::metadata(dir.path().join("a.sty")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o444);
    }

    #[test]
    fn a_bundle_that_differs_from_its_pins_is_refused() {
        let tar = good();
        let mut pins = test_pins(&tar, 2, DIGEST);
        pins.bundle_sha256 = "0".repeat(64);
        assert!(unpack(&tar, &pins).1.is_err(), "whole-file hash");
        assert!(unpack(&tar, &test_pins(&tar, 2, "other")).1.is_err(), "SHA256SUM");
        assert!(unpack(&tar, &test_pins(&tar, 1, DIGEST)).1.is_err(), "more entries than pinned");
        assert!(unpack(&tar, &test_pins(&tar, 3, DIGEST)).1.is_err(), "fewer entries than pinned");
        let mut small = test_pins(&tar, 2, DIGEST);
        small.bundle_size -= 1;
        assert!(unpack(&tar, &small).1.is_err(), "more bytes than pinned");
    }

    #[test]
    fn the_hash_covers_bytes_after_the_end_marker() {
        let mut tar = good();
        let pins = test_pins(&tar, 2, DIGEST);
        tar.extend_from_slice(&[0u8; 512]);
        let mut grown = test_pins(&tar, 2, DIGEST);
        grown.bundle_sha256 = pins.bundle_sha256.clone();
        assert!(unpack(&tar, &grown).1.is_err());
        let (_d, ok) = unpack(&tar, &test_pins(&tar, 2, DIGEST));
        ok.unwrap();
    }

    #[test]
    fn links_folders_devices_odd_names_and_duplicates_are_refused() {
        type Entry<'a> = (&'a [u8], &'a [u8], tar::EntryType);
        let sum: Entry = (b"SHA256SUM", b"abc", tar::EntryType::Regular);
        let cases: Vec<Vec<Entry>> = vec![
            vec![(b"l", b"", tar::EntryType::Symlink), sum],
            vec![(b"d", b"", tar::EntryType::Directory), sum],
            vec![(b"c", b"", tar::EntryType::Char), sum],
            vec![(b"a/b.sty", b"x", tar::EntryType::Regular), sum],
            vec![(b"..", b"x", tar::EntryType::Regular), sum],
            vec![(b"a\x01", b"x", tar::EntryType::Regular), sum],
            vec![(b"a.sty", b"x", tar::EntryType::Regular), (b"a.sty", b"y", tar::EntryType::Regular), sum],
        ];
        for case in cases {
            let tar = tar_of(&case);
            let (_d, out) = unpack(&tar, &test_pins(&tar, case.len() as u64, DIGEST));
            assert!(out.is_err(), "{:?}", case.iter().map(|c| String::from_utf8_lossy(c.0).into_owned()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_dot_name_is_plain_but_a_path_is_not() {
        assert_eq!(plain_name(Path::new(".tex")).as_deref(), Some(".tex"));
        assert!(plain_name(Path::new("./a")).is_none());
        assert!(plain_name(Path::new("/a")).is_none());
        assert!(plain_name(Path::new("")).is_none());
    }

    #[test]
    fn completeness_needs_binary_digest_and_formats() {
        let tar = good();
        let pins = test_pins(&tar, 2, DIGEST);
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        assert!(!is_complete(r, &pins));
        std::fs::write(r.join("tectonic"), "bin").unwrap();
        std::fs::create_dir_all(r.join("bundle")).unwrap();
        std::fs::write(r.join("bundle/SHA256SUM"), "abc\n").unwrap();
        assert!(!is_complete(r, &pins), "no formats");
        std::fs::create_dir_all(r.join("cache/formats")).unwrap();
        std::fs::write(r.join("cache/formats/x.fmt"), "fmt").unwrap();
        assert!(is_complete(r, &pins));
        std::fs::write(r.join("bundle/SHA256SUM"), "other").unwrap();
        assert!(!is_complete(r, &pins), "another bundle");
    }

    #[test]
    fn old_engines_and_the_old_cache_are_removed_and_the_current_engine_kept() {
        use std::os::unix::fs::PermissionsExt;
        let tar = good();
        let pins = test_pins(&tar, 2, DIGEST);
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for name in ["engine-k", "engine-old/bundle", "cache/formats", ".engine-1.partial"] {
            std::fs::create_dir_all(d.join(name)).unwrap();
        }
        std::fs::write(d.join("engine-old/bundle/a"), "x").unwrap();
        std::fs::set_permissions(d.join("engine-old/bundle"), std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(d.join("engine-old"), std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::write(d.join("tectonic"), "bin").unwrap();
        std::fs::write(d.join(".cache-trusted-1"), "").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), d.join("engine-linked")).unwrap();
        sweep_partials(d, STALE_PARTIAL);
        assert!(d.join(".engine-1.partial").exists(), "a recent partial may be an install still running");
        sweep_partials(d, Duration::ZERO);
        remove_old(d, &pins);
        let left: Vec<String> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec!["engine-k".to_string()]);
        assert!(outside.path().join("keep").is_file(), "only the link is removed");
    }
}
