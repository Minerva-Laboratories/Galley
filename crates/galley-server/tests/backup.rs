//! Backup against a small in-process stand-in for S3: upload, skip what did not change, restore.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use galley_server::backup::Backup;
use galley_server::config::BackupConfig;
use galley_server::db::Db;

type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

const BUCKET: &str = "backups";

fn key_of(uri: &Uri) -> String {
    uri.path().trim_start_matches(&format!("/{BUCKET}/")).to_string()
}

async fn put(State(objects): State<Objects>, uri: Uri, body: Bytes) -> StatusCode {
    objects.lock().unwrap().insert(key_of(&uri), body.to_vec());
    StatusCode::OK
}

async fn fetch(State(objects): State<Objects>, uri: Uri) -> axum::response::Response {
    match objects.lock().unwrap().get(&key_of(&uri)) {
        Some(bytes) => bytes.clone().into_response(),
        None => (StatusCode::NOT_FOUND, "<Error><Code>NoSuchKey</Code></Error>").into_response(),
    }
}

async fn list(State(objects): State<Objects>, Query(q): Query<BTreeMap<String, String>>) -> String {
    let prefix = q.get("prefix").cloned().unwrap_or_default();
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>backups</Name><IsTruncated>false</IsTruncated>"#);
    for (key, bytes) in objects.lock().unwrap().iter().filter(|(k, _)| k.starts_with(&prefix)) {
        xml.push_str(&format!(
            "<Contents><Key>{key}</Key><LastModified>2026-10-08T00:00:00.000Z</LastModified><ETag>\"x\"</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
            bytes.len()
        ));
    }
    xml.push_str("</ListBucketResult>");
    xml
}

async fn fake_s3() -> (String, Objects) {
    let objects: Objects = Arc::default();
    let app = Router::new()
        .route(&format!("/{BUCKET}"), get(list))
        .route(&format!("/{BUCKET}/"), get(list))
        .route(&format!("/{BUCKET}/{{*key}}"), get(fetch).put(put))
        .with_state(objects.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), objects)
}

fn config(endpoint: &str) -> BackupConfig {
    BackupConfig {
        endpoint: endpoint.into(),
        bucket: BUCKET.into(),
        region: "us-east-1".into(),
        path_style: true,
        access_key_id: "test-key".into(),
        secret_access_key: "test-secret".into(),
        ..BackupConfig::default()
    }
}

fn write_project(data_dir: &Path, id: &str, text: &str) {
    let dir = data_dir.join("data").join(id);
    for d in [".git/refs/heads", ".galley/docs", ".galley/build"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(dir.join(".git/refs/heads/main"), format!("{:040}\n", text.len())).unwrap();
    std::fs::write(dir.join(".galley/project.toml"), format!("name = \"{id}\"\n")).unwrap();
    std::fs::write(dir.join(".galley/build/main.pdf"), "build output").unwrap();
    std::fs::write(dir.join("main.tex"), text).unwrap();
}

#[tokio::test]
async fn backup_uploads_changes_only_and_restores_into_an_empty_directory() {
    let (endpoint, objects) = fake_s3().await;
    let live = tempfile::tempdir().unwrap();
    let db = Db::open(&live.path().join("galley.db")).unwrap();
    write_project(live.path(), "alicia", "Lorem ipsum dolor sit amet.");
    write_project(live.path(), "bob", "Consectetur adipiscing elit.");

    let backup = Backup::from_config(&config(&endpoint), live.path()).unwrap().unwrap();
    let first = backup.run_once(&db).await.unwrap();
    assert_eq!(first.projects_uploaded, 2);
    assert!(first.database_uploaded);
    {
        let keys: Vec<String> = objects.lock().unwrap().keys().cloned().collect();
        assert!(keys.contains(&"galley/projects/alicia.tar.gz".to_string()), "{keys:?}");
        assert!(keys.contains(&"galley/projects/bob.tar.gz".to_string()), "{keys:?}");
        assert!(keys.contains(&"galley/galley.db.gz".to_string()), "{keys:?}");
        assert!(keys.iter().any(|k| k.starts_with("galley/daily/")), "{keys:?}");
    }

    let quiet = backup.run_once(&db).await.unwrap();
    assert_eq!(quiet.projects_uploaded, 0, "nothing changed, nothing uploaded");
    assert!(!quiet.database_uploaded);

    write_project(live.path(), "bob", "Consectetur adipiscing elit, sed do eiusmod.");
    let after_edit = backup.run_once(&db).await.unwrap();
    assert_eq!(after_edit.projects_uploaded, 1, "only the edited project goes up again");

    // A new process remembers what it uploaded, so a restart does not upload everything again.
    let restarted = Backup::from_config(&config(&endpoint), live.path()).unwrap().unwrap();
    assert_eq!(restarted.run_once(&db).await.unwrap().projects_uploaded, 0);

    let fresh = tempfile::tempdir().unwrap();
    let restorer = Backup::from_config(&config(&endpoint), fresh.path()).unwrap().unwrap();
    let report = restorer.restore().await.unwrap();
    assert_eq!(report.projects, 2);
    assert_eq!(
        std::fs::read_to_string(fresh.path().join("data/bob/main.tex")).unwrap(),
        "Consectetur adipiscing elit, sed do eiusmod."
    );
    assert!(fresh.path().join("data/alicia/.git/refs/heads/main").exists());
    assert!(!fresh.path().join("data/alicia/.galley/build").exists(), "build output is not backed up");
    Db::open(&fresh.path().join("galley.db")).expect("the restored database opens");

    let again = restorer.restore().await;
    assert!(again.is_err(), "a restore never writes over a data directory that has work in it");
}

#[tokio::test]
async fn a_failed_upload_is_reported_and_retried() {
    let live = tempfile::tempdir().unwrap();
    let db = Db::open(&live.path().join("galley.db")).unwrap();
    write_project(live.path(), "alicia", "Lorem ipsum.");
    // Nothing listens on port 9 of localhost, so every upload fails.
    let backup = Backup::from_config(&config("http://127.0.0.1:9"), live.path()).unwrap().unwrap();
    assert!(backup.run_once(&db).await.is_err());
    let status = backup.status();
    assert!(status.last_error.is_some());
    assert!(status.last_ok.is_none());

    let (endpoint, objects) = fake_s3().await;
    let backup = Backup::from_config(&config(&endpoint), live.path()).unwrap().unwrap();
    assert_eq!(backup.run_once(&db).await.unwrap().projects_uploaded, 1, "the failed project is retried");
    assert!(objects.lock().unwrap().contains_key("galley/projects/alicia.tar.gz"));
}
