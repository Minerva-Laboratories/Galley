//! Opt-in, strict integration matrix for real TeX engines. Set GALLEY_TEST_ENGINE_MATRIX=1;
//! then all required paths and tools must be present and every assertion runs.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use galley_build::{
    BuildRequest, BuildStatus, Builder, EngineKind, Hints, ProjectBuilds, Sandbox, SandboxKind,
};

fn setup(data: &Path, timeout: Duration) -> Builder {
    let tectonic = std::env::var("GALLEY_TEST_TECTONIC").expect("GALLEY_TEST_TECTONIC is required");
    let cache = std::env::var("GALLEY_TEST_TECTONIC_CACHE")
        .expect("GALLEY_TEST_TECTONIC_CACHE is required");
    let texlive =
        std::env::var("GALLEY_TEST_TEXLIVE_PATH").expect("GALLEY_TEST_TEXLIVE_PATH is required");
    assert!(
        Path::new(&tectonic).is_file(),
        "missing Tectonic binary: {tectonic}"
    );
    assert!(
        Path::new(&cache).is_dir(),
        "missing Tectonic cache: {cache}"
    );
    assert!(
        Path::new(&texlive).is_dir(),
        "missing TeX Live executable directory: {texlive}"
    );
    std::fs::create_dir_all(data.join("tectonic")).unwrap();
    std::os::unix::fs::symlink(cache, data.join("tectonic/cache")).unwrap();
    let kind = if std::env::var("GALLEY_TEST_SANDBOX").as_deref() == Ok("bwrap") {
        SandboxKind::Bwrap
    } else {
        SandboxKind::None
    };
    Builder::new(
        Sandbox {
            kind,
            docker_image: String::new(),
            memory_mb: 2048,
            cpus: 2.0,
            systemd_scope: false,
        },
        Hints::bundled().unwrap(),
        timeout,
        data,
        Some(tectonic),
        1,
    )
    .with_texlive_path(Some(texlive))
}

fn request(engine: EngineKind) -> BuildRequest {
    BuildRequest {
        engine,
        draft: false,
        file: None,
        lint_disabled: Vec::new(),
        figure_cache: false,
    }
}

async fn good(builder: &Builder, project: &Path, id: u64, engine: EngineKind, source: &str) {
    std::fs::write(project.join("main.tex"), source).unwrap();
    let result = builder
        .run(id, project, "main.tex", &request(engine), &|_| {})
        .await;
    assert_eq!(
        result.status,
        BuildStatus::Ok,
        "{}: {result:#?}",
        engine.id()
    );
    assert!(
        result.pdf_fresh && result.pdf_available,
        "{}: {result:#?}",
        engine.id()
    );
    assert_eq!(result.pdf_engine.as_deref(), Some(engine.id()));
    assert!(
        result.engine_version.is_some(),
        "{} has no version",
        engine.id()
    );
    assert!(project.join(".galley/build/last-good.pdf").is_file());
    assert!(project.join(".galley/build/last-good.synctex.gz").is_file());
}

#[tokio::test]
async fn all_engines_and_auxiliary_workflows() {
    if std::env::var("GALLEY_TEST_ENGINE_MATRIX").as_deref() != Ok("1") {
        eprintln!("skipping real-engine matrix; set GALLEY_TEST_ENGINE_MATRIX=1 to require it");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let builder = setup(data.path(), Duration::from_secs(120));
    let available = builder.engine_availability().await;
    for entry in &available {
        assert!(
            entry.available,
            "{} unavailable: {:?}",
            entry.engine.id(),
            entry.reason
        );
    }
    // Compile the checked-in starter project unchanged. Its refs.bib contains only a comment,
    // so latexmk must not turn BibTeX's "no \citation commands" into a failed first build.
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/blank-article");
    std::fs::copy(template.join("main.tex"), project.path().join("main.tex")).unwrap();
    std::fs::copy(template.join("refs.bib"), project.path().join("refs.bib")).unwrap();
    for (i, engine) in EngineKind::ALL.into_iter().enumerate() {
        let result = builder
            .run(
                100 + i as u64,
                project.path(),
                "main.tex",
                &request(engine),
                &|_| {},
            )
            .await;
        assert_eq!(
            result.status,
            BuildStatus::Ok,
            "blank template / {engine:?}: {result:#?}"
        );
        assert!(result.pdf_fresh, "blank template / {engine:?}: {result:#?}");
    }
    let plain = "\\documentclass{article}\n\\begin{document}Hello Galley.\\label{here} See page~\\pageref{here}.\\end{document}\n";
    for (i, engine) in EngineKind::ALL.into_iter().enumerate() {
        good(&builder, project.path(), i as u64 + 1, engine, plain).await;
    }

    let unicode = "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{DejaVu Serif}\n\\begin{document}Español: año, café. Ελληνικά: κόσμος.\\end{document}\n";
    for (i, engine) in [EngineKind::XeLatex, EngineKind::LuaLatex]
        .into_iter()
        .enumerate()
    {
        good(&builder, project.path(), i as u64 + 10, engine, unicode).await;
    }

    std::fs::write(project.path().join("refs.bib"), "@article{knuth1984,author={Knuth, Donald},title={Literate Programming},journal={Computer Journal},year={1984}}\n").unwrap();
    let bibtex = "\\documentclass{article}\n\\begin{document}A citation~\\cite{knuth1984}.\\bibliographystyle{plain}\\bibliography{refs}\\end{document}\n";
    good(&builder, project.path(), 20, EngineKind::PdfLatex, bibtex).await;
    let bbl = project
        .path()
        .join(".galley/build/engines/pdflatex/main.bbl");
    assert!(std::fs::read_to_string(bbl).unwrap().contains("knuth1984"));

    let biber = "\\documentclass{article}\n\\usepackage[backend=biber]{biblatex}\n\\addbibresource{refs.bib}\n\\begin{document}A citation~\\cite{knuth1984}.\\printbibliography\\end{document}\n";
    good(&builder, project.path(), 21, EngineKind::XeLatex, biber).await;
    let bbl = project
        .path()
        .join(".galley/build/engines/xelatex/main.bbl");
    assert!(std::fs::read_to_string(bbl).unwrap().contains("knuth1984"));

    let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 60 60\nnewpath 5 5 moveto 55 55 lineto 2 setlinewidth stroke\nshowpage\n";
    std::fs::write(project.path().join("figure.eps"), eps).unwrap();
    let dvi = "\\documentclass{article}\n\\usepackage{graphicx}\n\\usepackage{pstricks}\n\\begin{document}\\includegraphics{figure.eps}\\begin{pspicture}(0,0)(1,1)\\psline(0,0)(1,1)\\end{pspicture}\\end{document}\n";
    good(&builder, project.path(), 22, EngineKind::Latex, dvi).await;
    assert!(
        project
            .path()
            .join(".galley/build/engines/latex/main.dvi")
            .is_file()
    );
    assert!(
        project
            .path()
            .join(".galley/build/engines/latex/main.ps")
            .is_file()
    );

    let last_good = std::fs::read(project.path().join(".galley/build/last-good.pdf")).unwrap();
    std::fs::write(
        project.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\undefinedgalleycommand\n\\end{document}\n",
    )
    .unwrap();
    let failed = builder
        .run(
            23,
            project.path(),
            "main.tex",
            &request(EngineKind::PdfLatex),
            &|_| {},
        )
        .await;
    assert_eq!(failed.status, BuildStatus::Failed, "{failed:#?}");
    assert!(!failed.pdf_fresh && failed.pdf_available);
    assert_eq!(failed.pdf_engine.as_deref(), Some("latex"));
    assert!(
        failed.errors.iter().any(|d| d.line == Some(3)),
        "{failed:#?}"
    );
    assert_eq!(
        std::fs::read(project.path().join(".galley/build/last-good.pdf")).unwrap(),
        last_good
    );
    let restored = ProjectBuilds::new(Arc::new(builder), project.path(), "main.tex", None)
        .last()
        .await
        .unwrap();
    assert_eq!(restored.engine, "pdflatex");
    assert_eq!(restored.pdf_engine.as_deref(), Some("latex"));
}
