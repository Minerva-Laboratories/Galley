//! Real integration checks, enabled with the same strict environment as engine_matrix.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use galley_build::{
    BuildRequest, BuildStatus, Builder, EngineKind, Hints, ProjectBuilds, Sandbox, SandboxKind,
};

fn enabled() -> bool {
    std::env::var("GALLEY_TEST_ENGINE_MATRIX").as_deref() == Ok("1")
}

fn setup(data: &Path) -> Builder {
    let cache = std::env::var("GALLEY_TEST_TECTONIC_CACHE").expect("warm Tectonic cache required");
    std::fs::create_dir_all(data.join("tectonic")).unwrap();
    std::os::unix::fs::symlink(cache, data.join("tectonic/cache")).unwrap();
    Builder::new(
        Sandbox {
            kind: SandboxKind::Bwrap,
            docker_image: String::new(),
            memory_mb: 2048,
            cpus: 2.0,
            systemd_scope: false,
        },
        Hints::bundled().unwrap(),
        Duration::from_secs(120),
        data,
        Some(std::env::var("GALLEY_TEST_TECTONIC").unwrap()),
        2,
    )
    .with_texlive_path(Some(std::env::var("GALLEY_TEST_TEXLIVE_PATH").unwrap()))
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

#[tokio::test]
async fn drafts_synctex_lua_and_submission_comparison_use_the_requested_engine() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let builder = Arc::new(setup(data.path()));
    let source = "\\documentclass{article}\n\\begin{document}\nHello Galley.\n\\end{document}\n";
    std::fs::write(project.path().join("main.tex"), source).unwrap();
    for (i, engine) in EngineKind::ALL.into_iter().enumerate() {
        let mut req = request(engine);
        req.draft = true;
        let result = builder
            .run(i as u64 + 1, project.path(), "main.tex", &req, &|_| {})
            .await;
        assert_eq!(result.status, BuildStatus::Ok, "{engine:?}: {result:#?}");
        assert!(result.draft);
        let builds = ProjectBuilds::new(builder.clone(), project.path(), "main.tex", None);
        let sync = builds.synctex().unwrap();
        let location = sync.forward("main.tex", 3).expect("forward SyncTeX target");
        let reverse = sync
            .inverse(location.page, location.x, location.y)
            .expect("inverse SyncTeX target");
        assert_eq!(reverse.file, "main.tex", "{engine:?}: {reverse:?}");
    }
    let lua =
        "\\documentclass{article}\n\\begin{document}\\directlua{tex.print(6*7)}\\end{document}\n";
    std::fs::write(project.path().join("main.tex"), lua).unwrap();
    let result = builder
        .run(
            10,
            project.path(),
            "main.tex",
            &request(EngineKind::LuaLatex),
            &|_| {},
        )
        .await;
    assert_eq!(result.status, BuildStatus::Ok, "{result:#?}");
    let text = std::process::Command::new("pdftotext")
        .arg(project.path().join(".galley/build/last-good.pdf"))
        .arg("-")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&text.stdout).contains("42"));
    for engine in EngineKind::ALL {
        std::fs::write(project.path().join("main.tex"), source).unwrap();
        builder
            .latexdiff(
                project.path(),
                source,
                &source.replace("Hello", "Welcome"),
                engine,
                &|_| {},
            )
            .await
            .unwrap();
        assert!(project.path().join(".galley/build/last-diff.pdf").is_file());
        let report = builder
            .pack(project.path(), "main.tex", engine, &|_| {})
            .await
            .unwrap();
        assert!(report.build_ok && report.archive, "{engine:?}: {report:#?}");
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(project.path().join(".galley/pack/stage/galley-build.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["engine"], engine.id());
    }
}

#[tokio::test]
async fn pdf_figure_caches_survive_warming_and_are_invalidated_between_engines() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let builder = setup(data.path());
    // Five figures exceed the runtime's maximum four parallel jobs, exercising background warm.
    let source = format!(
        "\\documentclass{{article}}\n\\usepackage{{tikz}}\n\\begin{{document}}\n{}\\end{{document}}\n",
        "\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}\n".repeat(5)
    );
    std::fs::write(project.path().join("main.tex"), &source).unwrap();
    let mut previous = None;
    for (i, engine) in [
        EngineKind::Tectonic,
        EngineKind::PdfLatex,
        EngineKind::XeLatex,
        EngineKind::LuaLatex,
    ]
    .into_iter()
    .enumerate()
    {
        let mut req = request(engine);
        req.figure_cache = true;
        let result = builder
            .run(i as u64 + 1, project.path(), "main.tex", &req, &|_| {})
            .await;
        assert_eq!(result.status, BuildStatus::Ok, "{engine:?}: {result:#?}");
        if !result.pending_figures.is_empty() {
            let generated = builder
                .warm_figures(project.path(), engine, result.pending_figures, || async {
                    false
                })
                .await;
            assert!(generated > 0, "{engine:?} must generate cached PDFs");
        } else {
            assert_eq!(
                result.figures.as_ref().unwrap().mode,
                "cached",
                "{result:#?}"
            );
        }
        let cached = builder
            .run(i as u64 + 10, project.path(), "main.tex", &req, &|_| {})
            .await;
        assert_eq!(cached.status, BuildStatus::Ok, "{engine:?}: {cached:#?}");
        let stats = cached.figures.as_ref().expect("figure statistics");
        assert_eq!(stats.mode, "cached", "{engine:?}: {cached:#?}");
        let cache = galley_build::figures::FigCache::load(&project.path().join(".galley/build"));
        if let Some(previous) = &previous {
            assert_ne!(&cache.key, previous);
        }
        previous = Some(cache.key);
    }
    let mut req = request(EngineKind::Latex);
    req.figure_cache = true;
    let result = builder
        .run(30, project.path(), "main.tex", &req, &|_| {})
        .await;
    assert_eq!(result.status, BuildStatus::Ok, "{result:#?}");
    assert_eq!(result.figures.unwrap().mode, "off");
}

#[tokio::test]
async fn real_compilation_ignores_latexmkrc_and_shell_escape_and_respects_timeout() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut builder = setup(data.path());
    std::fs::write(
        project.path().join("latexmkrc"),
        "die 'PROJECT RC MUST NOT EXECUTE';\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join(".latexmkrc"),
        "die 'PROJECT RC MUST NOT EXECUTE';\n",
    )
    .unwrap();
    let source = "\\documentclass{article}\n\\begin{document}\n\\immediate\\write18{touch .galley/build/shell-escaped}\nHello Galley.\\end{document}\n";
    std::fs::write(project.path().join("main.tex"), source).unwrap();
    for (i, engine) in [
        EngineKind::PdfLatex,
        EngineKind::XeLatex,
        EngineKind::LuaLatex,
        EngineKind::Latex,
    ]
    .into_iter()
    .enumerate()
    {
        let result = builder
            .run(
                i as u64 + 1,
                project.path(),
                "main.tex",
                &request(engine),
                &|_| {},
            )
            .await;
        assert_eq!(result.status, BuildStatus::Ok, "{engine:?}: {result:#?}");
        assert!(!project.path().join(".galley/build/shell-escaped").exists());
    }
    let previous = std::fs::read(project.path().join(".galley/build/last-good.pdf")).unwrap();
    std::fs::write(
        project.path().join("bad.eps"),
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 60 60\nundefinedGalleyPostScript\nshowpage\n",
    )
    .unwrap();
    std::fs::write(project.path().join("main.tex"), "\\documentclass{article}\n\\usepackage{graphicx}\n\\begin{document}\\includegraphics{bad.eps}\\end{document}\n").unwrap();
    let failed_conversion = builder
        .run(
            10,
            project.path(),
            "main.tex",
            &request(EngineKind::Latex),
            &|_| {},
        )
        .await;
    assert_eq!(
        failed_conversion.status,
        BuildStatus::Failed,
        "{failed_conversion:#?}"
    );
    assert!(
        !failed_conversion.errors.is_empty(),
        "converter failure must explain itself"
    );
    assert_eq!(
        std::fs::read(project.path().join(".galley/build/last-good.pdf")).unwrap(),
        previous
    );

    std::fs::write(project.path().join("main.tex"), "\\documentclass{article}\n\\usepackage{galley_nonexistent_package}\n\\begin{document}Hello.\\end{document}\n").unwrap();
    let missing_package = builder
        .run(
            11,
            project.path(),
            "main.tex",
            &request(EngineKind::PdfLatex),
            &|_| {},
        )
        .await;
    assert_eq!(
        missing_package.status,
        BuildStatus::Failed,
        "{missing_package:#?}"
    );
    assert!(
        missing_package
            .errors
            .iter()
            .any(|d| d.message.contains("galley_nonexistent_package")),
        "{missing_package:#?}"
    );
    assert_eq!(
        std::fs::read(project.path().join(".galley/build/last-good.pdf")).unwrap(),
        previous
    );
    let lua = "\\documentclass{article}\n\\begin{document}\\directlua{while true do end}\\end{document}\n";
    std::fs::write(project.path().join("main.tex"), lua).unwrap();
    builder.timeout = Duration::from_secs(2);
    let started = std::time::Instant::now();
    let result = builder
        .run(
            20,
            project.path(),
            "main.tex",
            &request(EngineKind::LuaLatex),
            &|_| {},
        )
        .await;
    assert_eq!(result.status, BuildStatus::Timeout, "{result:#?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        std::fs::read(project.path().join(".galley/build/last-good.pdf")).unwrap(),
        previous
    );
}

#[tokio::test]
async fn svg_conversion_remains_available_for_pdf_engines_and_disabled_for_dvi() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let builder = setup(data.path());
    let source = "\\documentclass{article}\n\\usepackage{svg}\n\\begin{document}\\includesvg{figure}\\end{document}\n";
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"60\" height=\"60\"><rect width=\"60\" height=\"60\" fill=\"red\"/></svg>";
    for (i, engine) in [
        EngineKind::PdfLatex,
        EngineKind::XeLatex,
        EngineKind::LuaLatex,
        EngineKind::Latex,
    ]
    .into_iter()
    .enumerate()
    {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.tex"), source).unwrap();
        std::fs::write(project.path().join("figure.svg"), svg).unwrap();
        let result = builder
            .run(
                i as u64 + 1,
                project.path(),
                "main.tex",
                &request(engine),
                &|_| {},
            )
            .await;
        if engine == EngineKind::Latex {
            assert!(
                !project.path().join(galley_build::svg::ROOT).exists(),
                "DVI must not convert SVG"
            );
            assert_eq!(result.status, BuildStatus::Failed, "{result:#?}");
        } else {
            assert_eq!(result.status, BuildStatus::Ok, "{engine:?}: {result:#?}");
            assert!(
                project
                    .path()
                    .join(galley_build::svg::ROOT)
                    .join("svg-inkscape/figure_svg-tex.pdf")
                    .is_file()
            );
        }
    }
}

#[tokio::test]
async fn bubblewrap_sources_are_readonly_builds_writable_and_network_is_absent() {
    if !enabled() {
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let out = project.path().join(".galley/build");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(project.path().join("main.tex"), "original").unwrap();
    let sandbox = Sandbox {
        kind: SandboxKind::Bwrap,
        docker_image: String::new(),
        memory_mb: 256,
        cpus: 1.0,
        systemd_scope: false,
    };
    let code = r#"from pathlib import Path
try:
    Path('/work/main.tex').write_text('changed')
    raise AssertionError('source was writable')
except OSError as error:
    assert error.errno in (13, 30), error
Path('/work/.galley/build/output.txt').write_text('allowed')
assert not Path('/proc/net/route').read_text().splitlines()[1:], 'network route available'
"#;
    let spec = galley_build::sandbox::Spec {
        project_dir: project.path().into(),
        out_dir: out.clone(),
        mounts: Vec::new(),
        masked: Vec::new(),
        env: Vec::new(),
        network: false,
        program: "/usr/bin/python3".into(),
        args: vec!["-c".into(), code.into()],
        name: "galley-isolation-test".into(),
    };
    let output = sandbox.command(&spec).output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("main.tex")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("output.txt")).unwrap(),
        "allowed"
    );
}
