//! An append-only log of yrs updates for one document. Each frame is `[u32 LE length][update v1]`
//! after an 8-byte magic header. A crash during a write can leave a torn last frame. The load drops
//! that frame. This is safe, because every frame is a complete update, and the working tree and git
//! hold the last flushed text.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"GLYDOC01";
const MAX_FRAME: u32 = 256 * 1024 * 1024;

pub struct UpdateLog {
    path: PathBuf,
    file: File,
    frames: usize,
}

impl std::fmt::Debug for UpdateLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateLog")
            .field("path", &self.path)
            .field("frames", &self.frames)
            .finish()
    }
}

impl UpdateLog {
    /// Open the log, or create it. Returns the log and every stored update, in order.
    pub fn open(path: &Path) -> io::Result<(UpdateLog, Vec<Vec<u8>>)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut bytes)?;

        let mut frames = Vec::new();
        let mut valid_len = 0usize;
        if bytes.is_empty() {
            file.write_all(MAGIC)?;
        } else if bytes.len() >= MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC {
            let mut pos = MAGIC.len();
            valid_len = pos;
            while pos + 4 <= bytes.len() {
                let len = u32::from_le_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                if len == 0 || len as u32 > MAX_FRAME || pos + 4 + len > bytes.len() {
                    break;
                }
                frames.push(bytes[pos + 4..pos + 4 + len].to_vec());
                pos += 4 + len;
                valid_len = pos;
            }
            if valid_len < bytes.len() {
                tracing::warn!(
                    path = %path.display(),
                    dropped = bytes.len() - valid_len,
                    "dropping torn trailing bytes from update log"
                );
                file.set_len(valid_len as u64)?;
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a Galley update log", path.display()),
            ));
        }
        let _ = valid_len;
        let n = frames.len();
        Ok((
            UpdateLog {
                path: path.to_path_buf(),
                file,
                frames: n,
            },
            frames,
        ))
    }

    pub fn append(&mut self, update: &[u8]) -> io::Result<()> {
        if update.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(4 + update.len());
        buf.extend_from_slice(&(update.len() as u32).to_le_bytes());
        buf.extend_from_slice(update);
        self.file.write_all(&buf)?;
        self.frames += 1;
        Ok(())
    }

    pub fn fsync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Replace the whole log with one full-state update. The method writes a temp file next to the
    /// log and renames it, so a crash leaves either the old log or the new log.
    pub fn compact(&mut self, full_update: &[u8]) -> io::Result<()> {
        let tmp = self.path.with_extension("ybin.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(MAGIC)?;
            f.write_all(&(full_update.len() as u32).to_le_bytes())?;
            f.write_all(full_update)?;
            f.sync_data()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        self.file = OpenOptions::new().read(true).append(true).open(&self.path)?;
        self.frames = 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".galley/docs/main.tex.ybin");
        {
            let (mut log, frames) = UpdateLog::open(&path).unwrap();
            assert!(frames.is_empty());
            log.append(b"one").unwrap();
            log.append(b"two-two").unwrap();
            log.fsync().unwrap();
        }
        // Simulate a crash mid-frame.
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[9, 0, 0, 0, b'x', b'y']).unwrap();
        }
        let (mut log, frames) = UpdateLog::open(&path).unwrap();
        assert_eq!(frames, vec![b"one".to_vec(), b"two-two".to_vec()]);
        assert_eq!(log.frames(), 2);
        log.append(b"three").unwrap();
        let (log, frames) = UpdateLog::open(&path).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(log.frames(), 3);
    }

    #[test]
    fn compact_keeps_single_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.ybin");
        let (mut log, _) = UpdateLog::open(&path).unwrap();
        log.append(b"a").unwrap();
        log.append(b"b").unwrap();
        log.compact(b"full").unwrap();
        log.append(b"c").unwrap();
        let (_, frames) = UpdateLog::open(&path).unwrap();
        assert_eq!(frames, vec![b"full".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn rejects_foreign_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.ybin");
        std::fs::write(&path, b"not a log").unwrap();
        assert!(UpdateLog::open(&path).is_err());
    }
}
