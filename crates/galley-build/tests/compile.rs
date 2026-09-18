//! Real compiles. They run only with a Tectonic binary and a warm cache:
//!   GALLEY_TEST_TECTONIC=/path/to/tectonic GALLEY_TEST_TECTONIC_CACHE=/path/to/cache cargo test -p galley-build

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use galley_build::{BuildRequest, BuildStatus, Builder, Hints, ProjectBuilds, Sandbox, SandboxKind};

fn env_setup() -> Option<(PathBuf, PathBuf)> {
    let bin = std::env::var_os("GALLEY_TEST_TECTONIC")?;
    let cache = std::env::var_os("GALLEY_TEST_TECTONIC_CACHE")?;
    Some((bin.into(), cache.into()))
}

fn builder(data_dir: &std::path::Path, bin: &std::path::Path, cache: &std::path::Path, kind: SandboxKind) -> Arc<Builder> {
    // Point the cache of the data dir at the warm cache, so that no network is needed.
    std::fs::create_dir_all(data_dir.join("tectonic")).unwrap();
    std::os::unix::fs::symlink(cache, data_dir.join("tectonic/cache")).unwrap();
    let sandbox = Sandbox {
        kind,
        docker_image: "debian:bookworm-slim".into(),
        memory_mb: 1024,
        cpus: 2.0,
        systemd_scope: false,
    };
    Arc::new(Builder::new(sandbox, Hints::bundled().unwrap(), Duration::from_secs(120), data_dir, Some(bin.to_string_lossy().into_owned())))
}

#[tokio::test]
async fn compiles_template_then_reports_errors() {
    let Some((bin, cache)) = env_setup() else {
        eprintln!("skipping: set GALLEY_TEST_TECTONIC and GALLEY_TEST_TECTONIC_CACHE");
        return;
    };
    let kind = match std::env::var("GALLEY_TEST_SANDBOX").as_deref() {
        Ok("docker") => SandboxKind::Docker,
        Ok("bwrap") => SandboxKind::Bwrap,
        _ => SandboxKind::None,
    };
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.tex"), include_str!("../../../templates/blank-article/main.tex")).unwrap();
    std::fs::write(project.path().join("refs.bib"), "").unwrap();
    let b = builder(data.path(), &bin, &cache, kind);
    let builds = ProjectBuilds::new(b, project.path(), "main.tex", None);

    let mut events = builds.subscribe();
    builds.request(BuildRequest { draft: false, file: None, lint_disabled: Vec::new(), figure_cache: true }).await;
    let result = loop {
        match events.recv().await.unwrap() {
            galley_build::BuildEvent::BuildFinished(r) => break r,
            other => eprintln!("{other:?}"),
        }
    };
    assert_eq!(result.status, BuildStatus::Ok, "{result:#?}");
    assert!(result.pdf_fresh);
    assert!(project.path().join(".galley/build/last-good.pdf").exists());
    assert!(project.path().join(".galley/build/last-good.synctex.gz").exists());
    let st = builds.synctex().unwrap();
    assert!(st.forward("main.tex", 10).is_some());

    // Break the document. The PDF from the good build stays.
    std::fs::write(project.path().join("main.tex"), "\\documentclass{article}\n\\begin{document}\n\\citep{x}\n\\end{document}\n").unwrap();
    builds.request(BuildRequest { draft: false, file: None, lint_disabled: Vec::new(), figure_cache: true }).await;
    let result = loop {
        if let galley_build::BuildEvent::BuildFinished(r) = events.recv().await.unwrap() {
            break r;
        }
    };
    assert_eq!(result.status, BuildStatus::Failed);
    assert!(!result.pdf_fresh);
    assert!(result.pdf_available);
    assert_eq!(result.errors[0].code, "undefined-control-sequence-natbib");
    assert_eq!(result.errors[0].line, Some(3));
    assert!(result.errors[0].fix.is_some());
}
