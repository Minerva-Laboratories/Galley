//! Table-driven checks against real Tectonic logs in tests/fixtures.

use galley_build::hints::{Hints, ProjectView};
use galley_build::log::{parse_log, Level};
use galley_build::Fix;

const ERRS: &str = include_str!("fixtures/errs.log");
const CLEAN: &str = include_str!("fixtures/clean.log");

const MAIN: &str = "\\documentclass[11pt]{article}\n\\usepackage{graphicx}\n\\begin{document}\n\\end{document}\n";

#[test]
fn clean_log_has_no_errors_or_warnings() {
    let hints = Hints::bundled().unwrap();
    let view = ProjectView { main_file: "main.tex", main_text: MAIN };
    let d: Vec<_> = parse_log(CLEAN).into_iter().map(|d| hints.annotate(d, &view)).collect();
    assert!(d.iter().all(|x| x.level != Level::Error), "{d:#?}");
    assert!(d.iter().all(|x| x.level != Level::Warning), "{d:#?}");
    // The empty bibliography of a new project is information. It is not a warning.
    assert!(d.iter().any(|x| x.code == "empty-bibliography" && x.level == Level::Info), "{d:#?}");
}

#[test]
fn broken_document_yields_expected_diagnostics() {
    let raw = parse_log(ERRS);
    let hints = Hints::bundled().unwrap();
    let view = ProjectView { main_file: "errs.tex", main_text: MAIN };
    let diags: Vec<_> = raw.into_iter().map(|d| hints.annotate(d, &view)).collect();

    let expect: &[(Level, &str, Option<&str>, Option<u32>)] = &[
        (Level::Warning, "undefined-reference", Some("errs.tex"), Some(5)),
        (Level::Warning, "undefined-citation", Some("errs.tex"), Some(5)),
        (Level::Warning, "overfull-hbox", Some("sections/method"), Some(5)),
        (Level::Error, "undefined-control-sequence", Some("sections/method"), Some(2)),
        (Level::Error, "missing-math", Some("errs.tex"), Some(8)),
        (Level::Error, "undefined-control-sequence-natbib", Some("errs.tex"), Some(9)),
        (Level::Error, "missing-math", Some("errs.tex"), Some(10)),
        (Level::Error, "unknown-error", Some("errs.tex"), Some(10)),
        (Level::Warning, "overfull-hbox", Some("errs.tex"), Some(2)),
        (Level::Error, "lonely-item", Some("errs.tex"), Some(11)),
        (Level::Error, "extra-end", Some("errs.tex"), Some(12)),
        (Level::Error, "extra-end", Some("errs.tex"), Some(12)),
        (Level::Error, "file-not-found-graphic", Some("errs.tex"), Some(13)),
        (Level::Info, "info", Some("errs.tex"), None),
    ];
    let got: Vec<(Level, &str, Option<&str>, Option<u32>)> = diags
        .iter()
        .map(|d| (d.level, d.code.as_str(), d.file.as_deref(), d.line))
        .collect();
    assert_eq!(got, expect, "{diags:#?}");

    let natbib = diags.iter().find(|d| d.code == "undefined-control-sequence-natbib").unwrap();
    assert_eq!(natbib.message, "Undefined control sequence \\citep");
    assert_eq!(
        natbib.fix,
        Some(Fix::Insert { label: "Add natbib".into(), file: "errs.tex".into(), after_line: 2, text: "\\usepackage{natbib}".into() })
    );
    let unknown = diags.iter().find(|d| d.code == "undefined-control-sequence").unwrap();
    assert_eq!(unknown.message, "Undefined control sequence \\unknowncommand");
    assert!(unknown.hint.is_some());
    let figure = diags.iter().find(|d| d.code == "file-not-found-graphic").unwrap();
    assert!(figure.hint.as_deref().unwrap().contains("figures/"));
    let reference = diags.iter().find(|d| d.code == "undefined-reference").unwrap();
    assert!(reference.hint.as_deref().unwrap().contains("\\label{sec:nope}"));
}
