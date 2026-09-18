//! Two simulated clients edit through the protocol layer. The text lands in git.

use std::time::Duration;

use galley_sync::{ProjectSync, SyncConfig};
use yrs::encoding::read::Cursor;
use yrs::sync::{Message, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

fn fast() -> SyncConfig {
    SyncConfig {
        flush_quiet: Duration::from_millis(120),
        flush_max: Duration::from_millis(2000),
        fsync_after: Duration::from_millis(10),
    }
}

/// A small y-protocol client. It holds a local Doc and the frames that it sends.
struct Client {
    doc: Doc,
}

impl Client {
    fn new() -> Self {
        Client { doc: Doc::new() }
    }

    fn step1(&self) -> Vec<u8> {
        let sv = self.doc.transact().state_vector();
        Message::Sync(SyncMessage::SyncStep1(sv)).encode_v1()
    }

    fn apply_frame(&self, frame: &[u8]) {
        let mut dec = DecoderV1::new(Cursor::new(frame));
        for msg in MessageReader::new(&mut dec) {
            match msg.unwrap() {
                Message::Sync(SyncMessage::SyncStep2(u)) | Message::Sync(SyncMessage::Update(u)) => {
                    let mut txn = self.doc.transact_mut();
                    txn.apply_update(Update::decode_v1(&u).unwrap()).unwrap();
                }
                _ => {}
            }
        }
    }

    fn insert(&self, at: u32, s: &str) -> Vec<u8> {
        let before = self.doc.transact().state_vector();
        let text = self.doc.get_or_insert_text("content");
        text.insert(&mut self.doc.transact_mut(), at, s);
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        Message::Sync(SyncMessage::Update(update)).encode_v1()
    }

    fn text(&self) -> String {
        let text = self.doc.get_or_insert_text("content");
        text.get_string(&self.doc.transact())
    }
}

#[tokio::test]
async fn edits_sync_between_clients_and_land_in_git() {
    let dir = tempfile::tempdir().unwrap();
    let project = ProjectSync::open("p1", dir.path(), fast()).unwrap();
    std::fs::write(dir.path().join("main.tex"), "\\section{Intro}\n").unwrap();

    let doc = project.doc("main.tex").await.unwrap();
    let a = Client::new();
    let b = Client::new();
    let mut b_inbox = doc.subscribe();

    // After the handshake, both clients receive the seeded text.
    for c in [&a, &b] {
        c.apply_frame(&doc.start_message().await.unwrap());
        let handled = doc.handle(&c.step1(), true).await.unwrap();
        for r in handled.replies {
            c.apply_frame(&r);
        }
    }
    assert_eq!(a.text(), "\\section{Intro}\n");
    assert_eq!(b.text(), "\\section{Intro}\n");

    // A types. B sees the text through the broadcast.
    project.set_last_editor("Ana Novak");
    let handled = doc.handle(&a.insert(14, "duction"), true).await.unwrap();
    assert!(handled.edited);
    let frame = tokio::time::timeout(Duration::from_secs(1), b_inbox.recv())
        .await
        .expect("broadcast")
        .unwrap();
    b.apply_frame(&frame);
    assert_eq!(b.text(), "\\section{Introduction}\n");
    assert_eq!(doc.text().await, "\\section{Introduction}\n");

    // After the quiet period, the auto-commit uses the last editor as the author.
    let mut events = project.subscribe_events();
    let ev = tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .expect("commit event")
        .unwrap();
    let galley_sync::ProjectEvent::Commit(commit) = ev else {
        panic!("expected commit event, got {ev:?}")
    };
    assert_eq!(commit.message, "edit: main.tex");
    assert_eq!(commit.author, "Ana Novak");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("main.tex")).unwrap(),
        "\\section{Introduction}\n"
    );
    let history = project.history(10).await.unwrap();
    assert_eq!(history.len(), 1);

    // Nothing changed, so flush_now does nothing.
    assert!(project.flush_now().await.unwrap().is_none());

    // The update log survives a restart.
    drop(doc);
    drop(project);
    let project = ProjectSync::open("p1", dir.path(), fast()).unwrap();
    let doc = project.doc("main.tex").await.unwrap();
    assert_eq!(doc.text().await, "\\section{Introduction}\n");
    assert!(dir.path().join(".galley/docs/main.tex.ybin").exists());
}

#[tokio::test]
async fn flush_now_commits_pending_text() {
    let dir = tempfile::tempdir().unwrap();
    let project = ProjectSync::open(
        "p2",
        dir.path(),
        SyncConfig {
            flush_quiet: Duration::from_secs(3600),
            ..fast()
        },
    )
    .unwrap();
    project.create_file("notes/a.tex").await.unwrap();
    let doc = project.doc("notes/a.tex").await.unwrap();
    let a = Client::new();
    a.apply_frame(&doc.start_message().await.unwrap());
    doc.handle(&a.insert(0, "hello"), true).await.unwrap();

    let commit = project.flush_now().await.unwrap().expect("a commit");
    assert_eq!(commit.message, "edit: notes/a.tex");
    assert_eq!(commit.author, "Galley");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes/a.tex")).unwrap(),
        "hello"
    );

    let files = project.list_files().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "notes/a.tex");
}

#[tokio::test]
async fn checkpoint_then_restore_reseeds_the_live_document() {
    use galley_history::Author;
    use galley_sync::DiffSide;

    let dir = tempfile::tempdir().unwrap();
    let project = ProjectSync::open(
        "p4",
        dir.path(),
        SyncConfig {
            flush_quiet: Duration::from_secs(3600),
            ..fast()
        },
    )
    .unwrap();
    project.create_file("main.tex").await.unwrap();
    let doc = project.doc("main.tex").await.unwrap();

    // A connected client types the first version and it is checkpointed.
    let a = Client::new();
    a.apply_frame(&doc.start_message().await.unwrap());
    let handled = doc.handle(&a.step1(), true).await.unwrap();
    for r in handled.replies {
        a.apply_frame(&r);
    }
    doc.handle(&a.insert(0, "first version"), true).await.unwrap();
    let mut a_inbox = doc.subscribe();
    let cp_sha = project
        .create_checkpoint("cp1", "v1", Author::galley())
        .await
        .unwrap();
    let checkpoints = project.checkpoints().await.unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].label, "v1");
    assert_eq!(checkpoints[0].sha, cp_sha);

    // The client rewrites the document; that lands as a new commit.
    let cur = doc.text().await;
    doc.handle(&a.insert(cur.len() as u32, " — rewritten"), true).await.unwrap();
    project.flush_now().await.unwrap();
    assert_eq!(doc.text().await, "first version — rewritten");

    // A diff between the checkpoint and the working tree shows the rewrite.
    let diff = project
        .diff(DiffSide::Rev(cp_sha.clone()), DiffSide::Workdir, None)
        .await
        .unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].path, "main.tex");

    // Restore the checkpoint. History grows and is not rewritten. The live document of the client
    // goes back to the old text.
    let restored = project
        .restore(&cp_sha, Author::galley(), "v1")
        .await
        .unwrap()
        .expect("restore commit");
    assert_eq!(restored.message, "restore: v1");
    assert_eq!(doc.text().await, "first version");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("main.tex")).unwrap(),
        "first version"
    );

    // The client receives the re-seed as an ordinary update and converges.
    let frame = tokio::time::timeout(Duration::from_secs(1), a_inbox.recv())
        .await
        .expect("reseed broadcast")
        .unwrap();
    a.apply_frame(&frame);
    // Read the remaining frames, such as the checkpoint flush, so the client is up to date.
    while let Ok(Ok(f)) = tokio::time::timeout(Duration::from_millis(150), a_inbox.recv()).await {
        a.apply_frame(&f);
    }
    assert_eq!(a.text(), "first version");

    // The restore added to the history. It did not rewrite the history.
    let history = project.history(20).await.unwrap();
    assert!(history.len() >= 3);
}

#[tokio::test]
async fn rejects_bad_paths() {
    let dir = tempfile::tempdir().unwrap();
    let project = ProjectSync::open("p3", dir.path(), fast()).unwrap();
    assert!(project.doc("../x.tex").await.is_err());
    assert!(project.doc(".git/config").await.is_err());
    assert!(project.doc("figures/fig1.pdf").await.is_err());
    assert!(project.create_file("main.tex").await.is_ok());
    assert!(project.create_file("main.tex").await.is_err());
    let _ = StateVector::default();
}
