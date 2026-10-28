//! Real compiles. They run only with an installed Tectonic engine (`galley engine install`):
//!   GALLEY_TEST_TECTONIC_ENGINES=/path/to/folder/with/engine-KEY cargo test -p galley-build

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use galley_build::{BuildRequest, BuildStatus, Builder, Hints, ProjectBuilds, Sandbox, SandboxKind};

fn env_setup() -> Option<PathBuf> {
    std::env::var_os("GALLEY_TEST_TECTONIC_ENGINES").map(PathBuf::from)
}

fn builder(data_dir: &std::path::Path, engines: &std::path::Path, kind: SandboxKind) -> Arc<Builder> {
    // The data folder's engine is the installed fixture, so no network is needed.
    let folder = galley_build::install::pins().unwrap().folder();
    std::fs::create_dir_all(data_dir.join("tectonic")).unwrap();
    std::os::unix::fs::symlink(std::path::Path::new(&engines).join(&folder), data_dir.join("tectonic").join(&folder)).unwrap();
    let sandbox = Sandbox {
        kind,
        docker_image: "debian:bookworm-slim".into(),
        memory_mb: 1024,
        cpus: 2.0,
        systemd_scope: false,
    };
    Arc::new(Builder::new(sandbox, Hints::bundled().unwrap(), Duration::from_secs(120), data_dir, 2))
}

#[tokio::test]
async fn compiles_template_then_reports_errors() {
    let Some(engines) = env_setup() else {
        eprintln!("skipping: set GALLEY_TEST_TECTONIC_ENGINES");
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
    let b = builder(data.path(), &engines, kind);
    let builds = ProjectBuilds::new(b, project.path(), "main.tex", None);

    let mut events = builds.subscribe();
    builds.request(BuildRequest { draft: false, file: None, lint_disabled: Vec::new(), figure_cache: true, engine: Default::default() }).await;
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
    builds.request(BuildRequest { draft: false, file: None, lint_disabled: Vec::new(), figure_cache: true, engine: Default::default() }).await;
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
