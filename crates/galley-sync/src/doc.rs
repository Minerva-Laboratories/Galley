//! One live document. It holds a yrs [`Awareness`] with the document and the presence, the update
//! log behind it, and a broadcast channel. The channel sends encoded protocol messages to every
//! connected client.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, mpsc, RwLock};
use yrs::encoding::read::Cursor;
use yrs::sync::{Awareness, DefaultProtocol, Message, MessageReader, Protocol, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Assoc, Doc, GetString, IndexedSequence, ReadTxn, StateVector, Subscription, Text, TextRef, Transact, Update};

use crate::persist::UpdateLog;
use crate::project::Housekeeping;
use crate::{Error, Result};

/// Root shared type name. The web app must use the same key (`ydoc.getText("content")`).
pub const TEXT_NAME: &str = "content";

/// After this many frames the log is rewritten as one full update on the next open.
const COMPACT_AFTER_FRAMES: usize = 2000;

/// Outcome of handling one inbound client frame.
pub struct Handled {
    /// Encoded replies for the sending client only (sync step 2, awareness answers).
    pub replies: Vec<Vec<u8>>,
    /// Whether the frame carried a document update, and not an awareness message or a handshake.
    pub edited: bool,
    /// A read-only client tried to send a document update. The connection must close.
    pub rejected: bool,
    /// Awareness client ids announced in this frame; a connection remembers them so it can
    /// clear their presence when it closes.
    pub awareness_clients: Vec<yrs::block::ClientID>,
}

pub struct DocHandle {
    rel_path: String,
    awareness: RwLock<Awareness>,
    /// Resolved once at open. A root type needs a write transaction to resolve. That transaction
    /// would deadlock against a read transaction that the same caller holds.
    text: TextRef,
    tx: broadcast::Sender<Arc<[u8]>>,
    log: Arc<Mutex<UpdateLog>>,
    _subs: Vec<Subscription>,
}

impl std::fmt::Debug for DocHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocHandle").field("path", &self.rel_path).finish()
    }
}

impl DocHandle {
    /// Load the document for `rel_path` from its update log, or seed it from the working tree
    /// when no log exists yet.
    pub(crate) fn open(
        workdir: &Path,
        rel_path: &str,
        housekeeping: mpsc::UnboundedSender<Housekeeping>,
    ) -> Result<Arc<DocHandle>> {
        let log_path = log_path(workdir, rel_path);
        let (log, frames) = UpdateLog::open(&log_path)?;
        let had_log = !frames.is_empty();

        let doc = Doc::new();
        let text = doc.get_or_insert_text(TEXT_NAME);
        {
            let mut txn = doc.transact_mut();
            for frame in &frames {
                let update = Update::decode_v1(frame)
                    .map_err(|e| Error::Corrupt(format!("{}: {e}", log_path.display())))?;
                txn.apply_update(update)
                    .map_err(|e| Error::Corrupt(format!("{}: {e}", log_path.display())))?;
            }
        }

        let log = Arc::new(Mutex::new(log));
        if frames.len() > COMPACT_AFTER_FRAMES {
            let full = doc
                .transact()
                .encode_state_as_update_v1(&StateVector::default());
            log.lock().expect("update log poisoned").compact(&full)?;
        }

        let (tx, _) = broadcast::channel(1024);
        let mut subs = Vec::new();

        // For every committed transaction: persist it, send it out, and tell the housekeeper.
        {
            let log = Arc::clone(&log);
            let tx = tx.clone();
            let hk = housekeeping.clone();
            let rel = rel_path.to_string();
            let sub = doc
                .observe_update_v1(move |_txn, event| {
                    if let Err(e) = log.lock().expect("update log poisoned").append(&event.update) {
                        tracing::error!(path = %rel, error = %e, "failed to append update; text is still in memory and in git after the next flush");
                    }
                    let msg = Message::Sync(SyncMessage::Update(event.update.clone())).encode_v1();
                    let _ = tx.send(Arc::from(msg));
                    let _ = hk.send(Housekeeping::Edited(rel.clone()));
                })
                .map_err(|e| Error::Corrupt(format!("cannot observe document: {e}")))?;
            subs.push(sub);
        }

        let on_disk = std::fs::read_to_string(workdir.join(rel_path)).ok();
        if !had_log {
            if let Some(existing) = on_disk.as_deref().filter(|s| !s.is_empty()) {
                text.insert(&mut doc.transact_mut(), 0, existing);
            }
        } else if let Some(on_disk) = on_disk {
            // The CRDT holds the live content. Galley reconciles a working tree that changed
            // outside Galley only through an explicit pull (SPEC.md §5.2). Log this instead of
            // overwriting the file without notice on the next flush.
            if on_disk != text.get_string(&doc.transact()) {
                tracing::warn!(path = %rel_path, "working tree differs from the live document; the live document wins on the next flush");
            }
        }

        let mut awareness = Awareness::new(doc);
        {
            let tx = tx.clone();
            let sub = awareness.on_update(move |aw, event, _origin| {
                match aw.update_with_clients(event.all_changes()) {
                    Ok(update) => {
                        let _ = tx.send(Arc::from(Message::Awareness(update).encode_v1()));
                    }
                    Err(e) => tracing::warn!(error = %e, "could not encode awareness update"),
                }
            });
            subs.push(sub);
        }

        Ok(Arc::new(DocHandle {
            rel_path: rel_path.to_string(),
            awareness: RwLock::new(awareness),
            text,
            tx,
            log,
            _subs: subs,
        }))
    }

    pub fn rel_path(&self) -> &str {
        &self.rel_path
    }

    /// Messages every client of this document should receive.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<[u8]>> {
        self.tx.subscribe()
    }

    /// The first handshake for a new client. It holds the state vector and the current awareness.
    pub async fn start_message(&self) -> Result<Vec<u8>> {
        let aw = self.awareness.read().await;
        let mut enc = EncoderV1::new();
        DefaultProtocol.start(&aw, &mut enc)?;
        Ok(enc.to_vec())
    }

    /// Apply one inbound frame. A frame can hold several protocol messages. A client with
    /// `apply_updates` set to false is read-only, a viewer or a commenter. Galley drops the
    /// document updates of that client. The client still receives the server state through sync
    /// step 1 and step 2, and Galley accepts its presence through awareness.
    pub async fn handle(&self, data: &[u8], apply_updates: bool) -> Result<Handled> {
        let mut aw = self.awareness.write().await;
        let mut decoder = DecoderV1::new(Cursor::new(data));
        let reader = MessageReader::new(&mut decoder);
        let mut replies = Vec::new();
        let mut edited = false;
        let mut rejected = false;
        let mut awareness_clients = Vec::new();
        for msg in reader {
            let msg = msg.map_err(yrs::sync::Error::from)?;
            match &msg {
                Message::Sync(SyncMessage::Update(_)) | Message::Sync(SyncMessage::SyncStep2(_)) => {
                    if !apply_updates {
                        rejected = true;
                        continue;
                    }
                    edited = true;
                }
                Message::Awareness(update) => {
                    awareness_clients.extend(update.clients.keys().copied());
                }
                _ => {}
            }
            if let Some(reply) = DefaultProtocol.handle_message(&mut aw, msg)? {
                replies.push(reply.encode_v1());
            }
        }
        Ok(Handled {
            replies,
            edited,
            rejected,
            awareness_clients,
        })
    }

    /// Remove the presence of a client that left, so others do not wait for the awareness timeout.
    pub async fn forget_client(&self, client_id: yrs::block::ClientID) {
        let mut aw = self.awareness.write().await;
        aw.remove_state(client_id);
    }

    /// The client ids that awareness knows. The id of the server is not included.
    pub async fn awareness_clients(&self) -> Vec<yrs::block::ClientID> {
        let aw = self.awareness.read().await;
        let own = aw.client_id();
        aw.iter().map(|(id, _)| id).filter(|id| *id != own).collect()
    }

    pub async fn text(&self) -> String {
        let aw = self.awareness.read().await;
        let txn = aw.doc().transact();
        self.text.get_string(&txn)
    }

    /// Replace the content of the document with `new_text` as one CRDT edit. The change is a
    /// minimal splice that keeps the shared prefix and the shared suffix. Connected clients
    /// converge on it like on any other edit. Restore and git pull use this to re-seed the live
    /// document without a rewrite of history. The method does nothing when the text already
    /// matches.
    pub async fn reseed(&self, new_text: &str) {
        let aw = self.awareness.write().await;
        let doc = aw.doc();
        let current = {
            let txn = doc.transact();
            self.text.get_string(&txn)
        };
        if current == new_text {
            return;
        }
        // yrs indexes text by UTF-8 byte offset with OffsetKind::Bytes. Compute the splice in
        // chars for correctness, then convert it to byte offsets. Trim a shared prefix and
        // suffix, so only the changed middle moves.
        let cur: Vec<char> = current.chars().collect();
        let new: Vec<char> = new_text.chars().collect();
        let mut prefix = 0;
        while prefix < cur.len() && prefix < new.len() && cur[prefix] == new[prefix] {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < cur.len() - prefix
            && suffix < new.len() - prefix
            && cur[cur.len() - 1 - suffix] == new[new.len() - 1 - suffix]
        {
            suffix += 1;
        }
        let bytes = |cs: &[char]| cs.iter().map(|c| c.len_utf8()).sum::<usize>() as u32;
        let start = bytes(&cur[..prefix]);
        let remove = bytes(&cur[prefix..cur.len() - suffix]);
        let inserted: String = new[prefix..new.len() - suffix].iter().collect();
        let mut txn = doc.transact_mut();
        if remove > 0 {
            self.text.remove_range(&mut txn, start, remove);
        }
        if !inserted.is_empty() {
            self.text.insert(&mut txn, start, &inserted);
        }
    }

    /// A relative position at `byte_index` that Yjs accepts. It is base64-encoded exactly as the
    /// `encodeAnchor` function of the web app encodes it, so every client can resolve an anchor
    /// that the server made. Comments and suggestions from agents use this. Returns `None` if the
    /// index is past the end of the text.
    pub async fn anchor_at(&self, byte_index: u32) -> Option<String> {
        let aw = self.awareness.read().await;
        let txn = aw.doc().transact();
        let sticky = self.text.sticky_index(&txn, byte_index, Assoc::After)?;
        Some(base64_std(&sticky.encode_v1()))
    }

    pub fn fsync(&self) -> std::io::Result<()> {
        self.log.lock().expect("update log poisoned").fsync()
    }
}

fn log_path(workdir: &Path, rel_path: &str) -> PathBuf {
    let mut p = workdir.join(".galley").join("docs");
    p.push(format!("{rel_path}.ybin"));
    p
}

/// Standard base64 with padding, the same as the `btoa` function of the browser. An anchor must go
/// through the decoder of the web app and come back with the same bytes.
fn base64_std(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod anchor_tests {
    use super::base64_std;

    #[test]
    fn base64_matches_btoa() {
        assert_eq!(base64_std(b""), "");
        assert_eq!(base64_std(b"f"), "Zg==");
        assert_eq!(base64_std(b"fo"), "Zm8=");
        assert_eq!(base64_std(b"foo"), "Zm9v");
        assert_eq!(base64_std(b"foobar"), "Zm9vYmFy");
    }
}
