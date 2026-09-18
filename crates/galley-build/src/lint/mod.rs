//! Style and hygiene rules (SPEC §13.5). They run after every build over the text files of the
//! project. They produce `Level::Lint` diagnostics, which never change the build status. Each rule
//! can be switched off per project through `lint_disabled` in `.galley/project.toml`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::log::{Diagnostic, Fix, Level};

pub const RULES: &[&str] = &[
    "unused-label",
    "unused-entry",
    "doubled-word",
    "breakable-ref",
    "long-bold",
    // From the paper index in galley_index::bib. These switch off in the same way.
    "bib-missing-entry",
    "bib-duplicate-entry",
    "bib-incomplete-entry",
    "bib-preprint",
];

static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\label\{([^}]+)\}").unwrap());
static REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\(?:ref|eqref|autoref|cref|Cref|pageref|nameref|hyperref)\*?(?:\[[^\]]*\])?\{([^}]+)\}").unwrap());
static CITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:[cC]ite[a-zA-Z]*|parencite|textcite|autocite|footcite|nocite)\*?(?:\[[^\]]*\])*\{([^}]+)\}").unwrap()
});
static BIB_ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@[a-zA-Z]+\s*\{\s*([^,\s]+)\s*,").unwrap());
static BREAKABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(Section|Sections|Table|Tables|Figure|Figures|Fig\.|Eq\.|Equation|Chapter|Appendix|Algorithm|Theorem|Lemma) \\(ref|eqref|cref|Cref|autoref)\{")
        .unwrap()
});
static LONG_BOLD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\textbf\{[^}]{80,}\}").unwrap());

/// Each entry in `files` is the project-relative path and the text of a `.tex` or `.bib` file.
pub fn lint(files: &[(String, String)], disabled: &[String]) -> Vec<Diagnostic> {
    let on = |rule: &str| !disabled.iter().any(|d| d == rule);
    let mut out = Vec::new();
    let tex: Vec<&(String, String)> = files.iter().filter(|(p, _)| p.ends_with(".tex")).collect();
    let bib: Vec<&(String, String)> = files.iter().filter(|(p, _)| p.ends_with(".bib")).collect();

    // Collect the facts across the files first. These are the labels, the refs and the citations.
    let mut labels: BTreeMap<String, (String, u32)> = BTreeMap::new();
    let mut refs: BTreeSet<String> = BTreeSet::new();
    let mut cited: BTreeSet<String> = BTreeSet::new();
    let mut nocite_all = false;
    for (path, text) in &tex {
        for (i, line) in text.lines().enumerate() {
            let code = strip_comment(line);
            for m in LABEL.captures_iter(code) {
                labels.entry(m[1].trim().to_string()).or_insert((path.clone(), i as u32 + 1));
            }
            for m in REF.captures_iter(code) {
                for k in m[1].split(',') {
                    refs.insert(k.trim().to_string());
                }
            }
            for m in CITE.captures_iter(code) {
                for k in m[1].split(',') {
                    let k = k.trim();
                    if k == "*" {
                        nocite_all = true;
                    }
                    cited.insert(k.to_string());
                }
            }
        }
    }

    if on("unused-label") {
        for (label, (file, line)) in &labels {
            if refs.contains(label) {
                continue;
            }
            out.push(diag(
                file,
                *line,
                "unused-label",
                format!("Label \"{label}\" is never referenced"),
                "Harmless, but stale labels make renames confusing. Remove it or reference it.",
                None,
            ));
        }
    }

    if on("unused-entry") && !nocite_all {
        for (path, text) in &bib {
            for (i, line) in text.lines().enumerate() {
                let Some(m) = BIB_ENTRY.captures(line) else { continue };
                let key = m[1].to_string();
                if cited.contains(&key) {
                    continue;
                }
                out.push(diag(
                    path,
                    i as u32 + 1,
                    "unused-entry",
                    format!("Bib entry \"{key}\" is never cited"),
                    "Unused entries are dropped from the submission package automatically.",
                    None,
                ));
            }
        }
    }

    for (path, text) in &tex {
        for (i, line) in text.lines().enumerate() {
            let code = strip_comment(line);
            let n = i as u32 + 1;
            if on("doubled-word") {
                if let Some((first, pair)) = doubled_word(code) {
                    out.push(diag(
                        path,
                        n,
                        "doubled-word",
                        format!("Doubled word \"{pair}\""),
                        "Probably a typo.",
                        Some(Fix::Replace { label: "Remove duplicate".into(), file: path.clone(), line: n, find: pair.clone(), text: first }),
                    ));
                }
            }
            if on("breakable-ref") {
                if let Some(m) = BREAKABLE.captures(code) {
                    let (word, cmd) = (&m[1], &m[2]);
                    out.push(diag(
                        path,
                        n,
                        "breakable-ref",
                        format!("Line may break between \"{word}\" and its number"),
                        &format!("Use a non-breaking space: {word}~\\{cmd}{{…}}."),
                        Some(Fix::Replace {
                            label: "Insert ~".into(),
                            file: path.clone(),
                            line: n,
                            find: format!("{word} \\{cmd}{{"),
                            text: format!("{word}~\\{cmd}{{"),
                        }),
                    ));
                }
            }
            if on("long-bold") && LONG_BOLD.is_match(code) {
                out.push(diag(
                    path,
                    n,
                    "long-bold",
                    "Long bold run".to_string(),
                    "Bold spans of a full sentence usually read as shouting; consider \\emph or plain text.",
                    None,
                ));
            }
        }
    }
    out
}

fn diag(file: &str, line: u32, code: &str, message: String, hint: &str, fix: Option<Fix>) -> Diagnostic {
    Diagnostic {
        level: Level::Lint,
        file: Some(file.to_string()),
        line: Some(line),
        code: code.to_string(),
        message,
        hint: Some(hint.to_string()),
        explain: None,
        fix,
        raw: String::new(),
    }
}

/// The line up to an unescaped `%`.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'%' => return &line[..i],
            _ => i += 1,
        }
    }
    line
}

/// Find two identical words in sequence, separated by one space and outside a command. Returns the
/// first word and the pair exactly as written, so that a fix can find it again.
fn doubled_word(code: &str) -> Option<(String, String)> {
    let mut prev: Option<(usize, &str)> = None;
    for (start, word) in words(code) {
        if let Some((pstart, pw)) = prev {
            let adjacent = pstart + pw.len() + 1 == start && code.as_bytes()[start - 1] == b' ';
            if adjacent && pw.eq_ignore_ascii_case(word) {
                return Some((pw.to_string(), code[pstart..start + word.len()].to_string()));
            }
        }
        prev = Some((start, word));
    }
    None
}

/// The alphabetic runs and their byte offsets. A run directly after a backslash is a command.
fn words(code: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in code.char_indices() {
        if c.is_alphabetic() {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            if !code[..s].ends_with('\\') {
                out.push((s, &code[s..i]));
            }
        }
    }
    if let Some(s) = start {
        if !code[..s].ends_with('\\') {
            out.push((s, &code[s..]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect()
    }

    #[test]
    fn unused_labels_and_entries_are_found_across_files() {
        let f = files(&[
            ("main.tex", "\\label{sec:a}\nSee \\cref{sec:b,fig:c}\n\\input{b}\n\\cite{knuth84}"),
            ("b.tex", "\\label{sec:b}\n\\label{fig:c} % auto"),
            ("refs.bib", "@article{knuth84, title={x}}\n@book{lamport94,\n title={y}}"),
        ]);
        let d = lint(&f, &[]);
        let codes: Vec<(String, String)> = d.iter().map(|d| (d.code.clone(), d.message.clone())).collect();
        assert!(codes.contains(&("unused-label".into(), "Label \"sec:a\" is never referenced".into())));
        assert!(codes.iter().any(|(c, m)| c == "unused-entry" && m.contains("lamport94")));
        assert!(!codes.iter().any(|(_, m)| m.contains("sec:b") || m.contains("fig:c") || m.contains("knuth84")));
        let entry = d.iter().find(|d| d.code == "unused-entry").unwrap();
        assert_eq!((entry.file.as_deref(), entry.line), (Some("refs.bib"), Some(2)));
    }

    #[test]
    fn doubled_word_has_a_verbatim_fix_and_ignores_commands() {
        let f = files(&[("main.tex", "This is the the method.\n\\begin{itemize} \\item item\n% the the\n")]);
        let d = lint(&f, &[]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "Doubled word \"the the\"");
        match &d[0].fix {
            Some(Fix::Replace { find, text, line, .. }) => assert_eq!((find.as_str(), text.as_str(), *line), ("the the", "the", 1)),
            other => panic!("unexpected fix {other:?}"),
        }
    }

    #[test]
    fn breakable_ref_and_long_bold() {
        let long = format!("\\textbf{{{}}}", "x".repeat(90));
        let f = files(&[("main.tex", &format!("As Section \\ref{{sec:a}} shows.\n{long}\nFigure~\\ref{{fig:b}}\n\\label{{sec:a}}\\label{{fig:b}}"))]);
        let d = lint(&f, &[]);
        let codes: Vec<&str> = d.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(codes, vec!["breakable-ref", "long-bold"]);
        match &d[0].fix {
            Some(Fix::Replace { find, text, .. }) => assert_eq!((find.as_str(), text.as_str()), ("Section \\ref{", "Section~\\ref{")),
            other => panic!("unexpected fix {other:?}"),
        }
    }

    #[test]
    fn rules_can_be_disabled_and_nocite_star_silences_entries() {
        let f = files(&[("main.tex", "the the \\nocite{*}"), ("r.bib", "@misc{k, x}")]);
        assert!(lint(&f, &["doubled-word".into()]).is_empty());
        assert_eq!(lint(&f, &[]).len(), 1);
    }
}
