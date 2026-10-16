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
    // The fixture cache is Galley's own warm-up, so the test marks it trusted.
    std::fs::write(data.join("tectonic").join(galley_build::engine::TRUSTED_CACHE), "").unwrap();
    builder(data)
}

fn builder(data: &Path) -> Builder {
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
    galley_build::zones::prepare(project.path()).unwrap();
    let leaf = galley_build::zones::Leaf::run(project.path()).unwrap();
    std::fs::write(project.path().join("main.tex"), "original").unwrap();
    std::fs::write(galley_build::zones::build_dir(project.path()).join("last.log"), "galley's").unwrap();
    let sandbox = Sandbox {
        kind: SandboxKind::Bwrap,
        docker_image: String::new(),
        memory_mb: 256,
        cpus: 1.0,
        systemd_scope: false,
    };
    let code = r#"from pathlib import Path
import os
try:
    Path('/work/main.tex').write_text('changed')
    raise AssertionError('source was writable')
except OSError as error:
    assert error.errno in (13, 30), error
# Galley's own folder is hidden, not just read-only.
assert not Path('/work/.galley/build').exists(), os.listdir('/work/.galley')
Path('/tmp/galley-out/output.txt').write_text('allowed')
assert not Path('/proc/net/route').read_text().splitlines()[1:], 'network route available'
"#;
    let spec = galley_build::sandbox::Spec {
        project_dir: project.path().into(),
        mounts: vec![galley_build::sandbox::Mount {
            host: leaf.path().to_path_buf(),
            guest: galley_build::zones::GUEST_OUT.into(),
            writable: true,
        }],
        masked: vec!["/work/.galley".into()],
        env: Vec::new(),
        network: false,
        program: "/usr/bin/python3".into(),
        args: vec!["-c".into(), code.into()],
        name: "galley-isolation-test".into(),
    };
    spec.check_mounts().unwrap();
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
        std::fs::read_to_string(leaf.path().join("output.txt")).unwrap(),
        "allowed"
    );
}

/// A document that plants links where Galley reads, and tries to reach Galley's own folder, gets
/// nothing out of either: the PDF is its own, the log is not another file, and nothing outside its
/// scratch folder changes.
#[tokio::test]
async fn a_document_that_plants_links_reaches_nothing_outside_its_scratch_folder() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let builder = setup(data.path());
    let secrets = tempfile::tempdir().unwrap();
    let canary = secrets.path().join("canary.txt");
    std::fs::write(&canary, "GALLEY-CANARY").unwrap();
    let project = tempfile::tempdir().unwrap();
    let target = canary.display().to_string();
    let plant = |names: &[&str]| -> String {
        names
            .iter()
            .map(|n| format!("lfs.link('{target}', '/tmp/galley-out/{n}', true) "))
            .collect()
    };
    let source = format!(
        "\\documentclass{{article}}\n\\begin{{document}}\nHello.\n\\directlua{{\n         {}\n         lfs.link('/', '/tmp/galley-out/root', true)\n         local f = io.open('/work/.galley/build/last-good.pdf', 'w')\n         if f then f:write('planted') f:close() end\n         }}\n\\end{{document}}\n",
        plant(&["main.bbl", "last.log", "last-good.pdf", "current", "galley-fig.figlist"])
    );
    std::fs::write(project.path().join("main.tex"), &source).unwrap();
    let result = builder
        .run(1, project.path(), "main.tex", &request(EngineKind::LuaLatex), &|_| {})
        .await;
    assert_eq!(result.status, BuildStatus::Ok, "{result:#?}");
    let out = galley_build::zones::build_dir(project.path());
    let pdf = std::fs::read(out.join("last-good.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF"), "the published PDF is the compile's own");
    let log = std::fs::read_to_string(out.join("last.log")).unwrap();
    assert!(!log.contains("GALLEY-CANARY"));
    assert_eq!(std::fs::read_to_string(&canary).unwrap(), "GALLEY-CANARY");
    // Galley's folder holds only Galley's links.
    for entry in std::fs::read_dir(&out).unwrap().flatten() {
        if entry.file_type().unwrap().is_symlink() {
            let link = std::fs::read_link(entry.path()).unwrap();
            assert!(link.starts_with("current") || link.starts_with("published"), "{link:?}");
        }
    }

    // A log swapped for a link at the end of the run is not read either.
    let swap = format!(
        "\\documentclass{{article}}\n\\begin{{document}}\nHello.\n\\directlua{{\n         os.remove('/tmp/galley-out/main.log') lfs.link('{target}', '/tmp/galley-out/main.log', true)\n         }}\n\\end{{document}}\n"
    );
    std::fs::write(project.path().join("main.tex"), &swap).unwrap();
    let _ = builder
        .run(2, project.path(), "main.tex", &request(EngineKind::LuaLatex), &|_| {})
        .await;
    let log = std::fs::read_to_string(out.join("last.log")).unwrap_or_default();
    assert!(!log.contains("GALLEY-CANARY"));
    assert_eq!(std::fs::read_to_string(&canary).unwrap(), "GALLEY-CANARY");
}

/// The split into scratch folders must not slow a warm build down. The times are printed so CI
/// keeps a record; the bound only catches a large regression.
#[tokio::test]
async fn warm_builds_stay_fast() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let builder = setup(data.path());
    let project = tempfile::tempdir().unwrap();
    for engine in [EngineKind::Tectonic, EngineKind::PdfLatex] {
        std::fs::write(
            project.path().join("main.tex"),
            "\\documentclass{article}\n\\begin{document}\nHello Galley.\n\\end{document}\n",
        )
        .unwrap();
        let cold = std::time::Instant::now();
        let first = builder.run(1, project.path(), "main.tex", &request(engine), &|_| {}).await;
        assert_eq!(first.status, BuildStatus::Ok, "{first:#?}");
        let cold = cold.elapsed();
        let mut warm = Vec::new();
        for i in 0..3 {
            std::fs::write(
                project.path().join("main.tex"),
                format!("\\documentclass{{article}}\n\\begin{{document}}\nHello {i}.\n\\end{{document}}\n"),
            )
            .unwrap();
            let t = std::time::Instant::now();
            let r = builder.run(2 + i, project.path(), "main.tex", &request(engine), &|_| {}).await;
            assert_eq!(r.status, BuildStatus::Ok, "{r:#?}");
            warm.push(t.elapsed());
        }
        let best = warm.iter().min().unwrap();
        println!("{engine:?}: cold {cold:?}, warm {warm:?}");
        assert!(*best < Duration::from_secs(20), "{engine:?} warm build took {best:?}");
        // No scratch folder is left behind by finished builds.
        let scratch = project.path().join(".galley/scratch");
        let runs = std::fs::read_dir(&scratch)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("run-"))
            .count();
        assert_eq!(runs, 0);
    }
}

/// Every file below `dir`, with its size, to compare a folder before and after a build.
fn listing(dir: &Path) -> Vec<(std::path::PathBuf, u64)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                todo.push(entry.path());
            } else {
                out.push((entry.path().strip_prefix(dir).unwrap().to_path_buf(), meta.len()));
            }
        }
    }
    out.sort();
    out
}

/// The online package fetch loads files from the project's Tectonic cache, so a document must
/// not be able to write there. A new project still compiles from the warm cache it is given.
#[tokio::test]
async fn a_tectonic_document_cannot_plant_files_in_the_package_cache() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let builder = setup(data.path());
    let project = tempfile::tempdir().unwrap();
    let cache = galley_build::zones::cache_dir(project.path());
    std::fs::write(
        project.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nHello Galley.\n\\end{document}\n",
    )
    .unwrap();
    assert!(!cache.exists(), "a new project has no cache of its own yet");
    let first = builder.run(1, project.path(), "main.tex", &request(EngineKind::Tectonic), &|_| {}).await;
    assert_eq!(first.status, BuildStatus::Ok, "{first:#?}");
    assert!(first.pdf_fresh);
    let before = listing(&cache);
    assert!(!before.is_empty());

    let plant = "\\documentclass{article}\n\\begin{document}\n\\newwrite\\plant\n\\immediate\\openout\\plant=/galley/cache/planted.cls\n\\immediate\\write\\plant{\\relax}\n\\immediate\\closeout\\plant\n\\immediate\\openout\\plant=/galley/cache/formats/planted.fmt\n\\immediate\\write\\plant{\\relax}\n\\immediate\\closeout\\plant\nHello.\n\\end{document}\n";
    std::fs::write(project.path().join("main.tex"), plant).unwrap();
    let _ = builder.run(2, project.path(), "main.tex", &request(EngineKind::Tectonic), &|_| {}).await;
    assert_eq!(listing(&cache), before, "the compile changed the package cache");
}

/// With no warm cache, the first build fetches what it needs with Galley's own document, which
/// also builds Tectonic's format files, and the author's compile then runs from a read-only cache.
/// A package added later is fetched the same way. Needs network: set GALLEY_TEST_NETWORK=1.
#[tokio::test]
async fn a_cold_cache_is_filled_by_galleys_fetch_and_then_read_only() {
    if !enabled() || std::env::var("GALLEY_TEST_NETWORK").as_deref() != Ok("1") {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data.path().join("tectonic/cache")).unwrap();
    let builder = builder(data.path());
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nHello Galley.\n\\end{document}\n",
    )
    .unwrap();
    let first = builder.run(1, project.path(), "main.tex", &request(EngineKind::Tectonic), &|_| {}).await;
    assert_eq!(first.status, BuildStatus::Ok, "{first:#?}");
    let formats = galley_build::zones::cache_dir(project.path()).join("formats");
    assert!(std::fs::read_dir(&formats).is_ok_and(|mut d| d.next().is_some()), "the fetch built the formats");

    std::fs::write(
        project.path().join("main.tex"),
        "\\documentclass{article}\n\\usepackage{lipsum}\n\\begin{document}\n\\lipsum[1]\n\\end{document}\n",
    )
    .unwrap();
    let added = builder.run(2, project.path(), "main.tex", &request(EngineKind::Tectonic), &|_| {}).await;
    assert_eq!(added.status, BuildStatus::Ok, "{added:#?}");
    assert!(added.pdf_fresh);

    // A fetch cut off before it built the formats leaves a cache the author's compile cannot
    // finish. It counts as cold, so the next build fetches again instead of failing for good.
    std::fs::remove_dir_all(&formats).unwrap();
    let recovered = builder.run(3, project.path(), "main.tex", &request(EngineKind::Tectonic), &|_| {}).await;
    assert_eq!(recovered.status, BuildStatus::Ok, "{recovered:#?}");
    assert!(std::fs::read_dir(&formats).is_ok_and(|mut d| d.next().is_some()));
}
