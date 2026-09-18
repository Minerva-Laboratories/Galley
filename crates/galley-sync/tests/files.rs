//! File operations: upload, rename and delete. Each one makes a commit. None of them can bring
//! deleted text back.

use std::time::Duration;

use galley_sync::{ProjectSync, SyncConfig};
use yrs::encoding::read::Cursor;
use yrs::sync::{Message, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

fn fast() -> SyncConfig {
    SyncConfig {
        flush_quiet: Duration::from_millis(80),
        flush_max: Duration::from_millis(1000),
        fsync_after: Duration::from_millis(10),
    }
}


/// A browser that saw the file before it was deleted, and edits it afterwards.
struct Stale {
    doc: Doc,
}

impl Stale {
    fn sync_from(&self, update: &[u8]) {
        let mut dec = DecoderV1::new(Cursor::new(update));
        for msg in MessageReader::new(&mut dec) {
            if let Message::Sync(SyncMessage::SyncStep2(u)) | Message::Sync(SyncMessage::Update(u)) = msg.unwrap() {
                self.doc.transact_mut().apply_update(Update::decode_v1(&u).unwrap()).unwrap();
            }
        }
    }
    fn text(&self) -> String {
        self.doc.get_or_insert_text("content").get_string(&self.doc.transact())
    }
    fn full_update(&self) -> Vec<u8> {
        let u = self.doc.transact().encode_state_as_update_v1(&StateVector::default());
        Message::Sync(SyncMessage::Update(u)).encode_v1()
    }
}

#[tokio::test]
async fn upload_rename_delete_commit_and_deleted_text_stays_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let p = ProjectSync::open("t", dir.path(), fast()).unwrap();

    p.put_file("figs/plot.png", b"\x89PNG....", false, "Ana").await.unwrap();
    p.put_file("notes.tex", b"hello notes", false, "Ana").await.unwrap();
    assert!(p.put_file("notes.tex", b"again", false, "Ana").await.is_err(), "no silent overwrite");
    assert_eq!(p.read_bytes("notes.tex").await.unwrap().1, b"hello notes");

    // A browser loads the document before anything else happens.
    let stale = Stale { doc: Doc::new() };
    let doc = p.doc("notes.tex").await.unwrap();
    for frame in [doc.start_message().await.unwrap()] {
        let reply = doc
            .handle(&Message::Sync(SyncMessage::SyncStep1(stale.doc.transact().state_vector())).encode_v1(), true)
            .await
            .unwrap();
        stale.sync_from(&frame);
        for r in reply.replies {
            stale.sync_from(&r);
        }
    }
    assert_eq!(stale.text(), "hello notes");

    p.rename_file("notes.tex", "sections/notes.tex", "Ana").await.unwrap();
    assert!(!dir.path().join("notes.tex").exists());
    assert_eq!(std::fs::read_to_string(dir.path().join("sections/notes.tex")).unwrap(), "hello notes");
    p.rename_file("figs/plot.png", "plot.png", "Ana").await.unwrap();
    assert!(!dir.path().join("figs").exists(), "empty folders are pruned");

    p.delete_file("sections/notes.tex", "Ana").await.unwrap();
    assert!(!dir.path().join("sections").exists());

    // The stale browser edits its copy of the old path and syncs. The file must not come back.
    stale.doc.get_or_insert_text("content").insert(&mut stale.doc.transact_mut(), 0, "late ");
    doc.handle(&stale.full_update(), true).await.unwrap();
    p.flush_now().await.unwrap();
    assert!(!dir.path().join("notes.tex").exists(), "a late edit must not revive a deleted file");

    // A new file with the same name starts empty, even though the old history exists.
    p.put_file("notes.tex", b"fresh", false, "Ana").await.unwrap();
    assert_eq!(p.read_bytes("notes.tex").await.unwrap().1, b"fresh");

    let log: Vec<String> = p.history(50).await.unwrap().into_iter().map(|c| c.message.trim().to_string()).collect();
    for want in ["upload: figs/plot.png", "upload: notes.tex", "rename: notes.tex → sections/notes.tex", "rename: figs/plot.png → plot.png", "delete: sections/notes.tex"] {
        assert!(log.iter().any(|m| m == want), "missing commit {want:?} in {log:?}");
    }
    let names: Vec<String> = p.list_files().unwrap().into_iter().map(|f| f.path).collect();
    assert_eq!(names, vec!["notes.tex", "plot.png"]);
}

#[tokio::test]
async fn rejects_bad_operations() {
    let dir = tempfile::tempdir().unwrap();
    let p = ProjectSync::open("t", dir.path(), fast()).unwrap();
    p.put_file("a.tex", b"x", false, "Ana").await.unwrap();
    p.put_file("b.tex", b"y", false, "Ana").await.unwrap();
    assert!(p.rename_file("a.tex", "b.tex", "Ana").await.is_err(), "no overwrite on rename");
    assert!(p.rename_file("a.tex", "a.png", "Ana").await.is_err(), "no text-to-binary rename");
    assert!(p.delete_file("missing.tex", "Ana").await.is_err());
    assert!(p.put_file("../escape.tex", b"x", false, "Ana").await.is_err());
    assert!(p.put_file(".git/config", b"x", false, "Ana").await.is_err());
    assert!(p.put_file("bad.tex", &[0xff, 0xfe], false, "Ana").await.is_err(), "text files must be UTF-8");
    assert!(p.read_bytes("missing.png").await.is_err());
}

#[tokio::test]
async fn a_deleted_file_comes_back_from_history_with_its_text() {
    let dir = tempfile::tempdir().unwrap();
    let p = ProjectSync::open("t", dir.path(), fast()).unwrap();
    p.put_file("intro.tex", b"Once upon a time", false, "Ana").await.unwrap();
    let before = p.history(1).await.unwrap()[0].sha.clone();
    p.delete_file("intro.tex", "Ana").await.unwrap();
    assert!(!p.exists("intro.tex"));
    p.restore(&before, galley_history::Author::from_display_name("Ana"), "before the delete").await.unwrap();
    assert!(p.exists("intro.tex"));
    assert_eq!(p.read_bytes("intro.tex").await.unwrap().1, b"Once upon a time");
    p.flush_now().await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("intro.tex")).unwrap(), "Once upon a time");
}
