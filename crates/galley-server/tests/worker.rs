//! A build on a worker: the server sends the sources, the worker compiles, and the server publishes
//! the PDF under its usual rules. Runs only where pdflatex and latexmk are installed.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use galley_build::{BuildEvent, BuildRequest, BuildResult, BuildStatus, Builder, EngineKind, Hints, ProjectBuilds, Sandbox, SandboxKind};

const TOKEN: &str = "worker-test-token";

fn texlive() -> Option<String> {
    let dir = Path::new("/usr/bin");
    (dir.join("pdflatex").is_file() && dir.join("latexmk").is_file()).then(|| dir.to_string_lossy().into_owned())
}

fn sandbox_kind() -> SandboxKind {
    match std::env::var("GALLEY_TEST_SANDBOX").as_deref() {
        Ok("bwrap") => SandboxKind::Bwrap,
        _ => SandboxKind::None,
    }
}

fn builder(data_dir: &Path, texlive: &str) -> Builder {
    let sandbox = Sandbox { kind: sandbox_kind(), docker_image: String::new(), memory_mb: 1024, cpus: 1.0, systemd_scope: false };
    Builder::new(sandbox, Hints::bundled().unwrap(), Duration::from_secs(120), data_dir, None, 2)
        .with_texlive_path(Some(texlive.to_string()))
}

async fn start_worker(data_dir: &Path, texlive: &str) -> String {
    let app = galley_server::worker::worker_router(Arc::new(builder(data_dir, texlive)), TOKEN, data_dir.join("work"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn request() -> BuildRequest {
    BuildRequest { engine: EngineKind::PdfLatex, draft: false, file: None, lint_disabled: Vec::new(), figure_cache: false }
}

async fn build(builds: &Arc<ProjectBuilds>) -> BuildResult {
    let mut events = builds.subscribe();
    builds.request(request()).await;
    loop {
        if let BuildEvent::BuildFinished(r) = events.recv().await.unwrap() {
            return *r;
        }
    }
}

#[tokio::test]
async fn a_worker_compiles_and_the_server_publishes() {
    let Some(texlive) = texlive() else {
        eprintln!("skipping: needs pdflatex and latexmk in /usr/bin");
        return;
    };
    let worker_data = tempfile::tempdir().unwrap();
    let url = start_worker(worker_data.path(), &texlive).await;

    let server_data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nLorem ipsum dolor sit amet.\n\\end{document}\n",
    )
    .unwrap();
    let core = Arc::new(builder(server_data.path(), &texlive).with_remote(&url, TOKEN));
    let builds = ProjectBuilds::new(core, project.path(), "main.tex", None);

    let ok = build(&builds).await;
    assert_eq!(ok.status, BuildStatus::Ok, "{ok:#?}");
    assert!(ok.pdf_fresh);
    let pdf = project.path().join(".galley/build/last-good.pdf");
    let first = std::fs::read(&pdf).unwrap();
    assert!(first.starts_with(b"%PDF"));
    assert!(project.path().join(".galley/build/last-good.synctex.gz").exists());
    // Inside a sandbox both machines see the project at /work, so SyncTeX lines up. Without one the
    // worker's copy has a different absolute path, which only a development setup ever has.
    if sandbox_kind() == SandboxKind::Bwrap {
        assert!(builds.synctex().unwrap().forward("main.tex", 3).is_some(), "SyncTeX works on the server");
    }

    std::fs::write(project.path().join("main.tex"), "\\documentclass{article}\n\\begin{document}\n\\notacommand\n\\end{document}\n").unwrap();
    let failed = build(&builds).await;
    assert_eq!(failed.status, BuildStatus::Failed, "{failed:#?}");
    assert!(!failed.pdf_fresh);
    assert!(failed.pdf_available);
    assert!(failed.errors.iter().any(|e| e.line == Some(3)), "{:#?}", failed.errors);
    assert_eq!(std::fs::read(&pdf).unwrap(), first, "a failed build keeps the PDF the author had");
    assert!(
        std::fs::read_to_string(project.path().join(".galley/build/last.log")).unwrap().contains("notacommand"),
        "the worker's log arrives for the Problems panel"
    );
}

#[tokio::test]
async fn a_worker_refuses_the_wrong_token_and_the_card_says_what_to_do() {
    let Some(texlive) = texlive() else {
        eprintln!("skipping: needs pdflatex and latexmk in /usr/bin");
        return;
    };
    let worker_data = tempfile::tempdir().unwrap();
    let url = start_worker(worker_data.path(), &texlive).await;
    let server_data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.tex"), "\\documentclass{article}\\begin{document}x\\end{document}").unwrap();
    let core = Arc::new(builder(server_data.path(), &texlive).with_remote(&url, "not-the-token"));
    let builds = ProjectBuilds::new(core, project.path(), "main.tex", None);
    let refused = build(&builds).await;
    assert_eq!(refused.status, BuildStatus::Error);
    let message = refused.message.unwrap();
    assert!(message.contains("refused"), "{message}");
    assert!(message.contains("Your last PDF is unchanged"), "{message}");
    assert!(!worker_data.path().join("work").read_dir().map(|mut d| d.next().is_some()).unwrap_or(false), "nothing unpacked");
}

#[tokio::test]
async fn an_unreachable_worker_is_explained() {
    let Some(texlive) = texlive() else { return };
    let server_data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.tex"), "\\documentclass{article}\\begin{document}x\\end{document}").unwrap();
    let core = Arc::new(builder(server_data.path(), &texlive).with_remote("http://127.0.0.1:9", TOKEN));
    let builds = ProjectBuilds::new(core, project.path(), "main.tex", None);
    let r = build(&builds).await;
    assert_eq!(r.status, BuildStatus::Error);
    assert!(r.message.unwrap().contains("could not be reached"));
}
