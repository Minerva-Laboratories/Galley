//! The map as text, within a token budget: the highest-ranked sections in reading order. Each
//! section lists the labelled objects it owns and the works it cites. The parse notes come last.

use std::collections::BTreeSet;

use crate::parse::Paper;
use crate::rank::Ranked;
use crate::CHARS_PER_TOKEN;

/// `budget_tokens` is approximate. The map is built section by section and stops before it
/// exceeds the budget, so the result always holds complete lines.
pub fn render(paper: &Paper, ranked: &Ranked, budget_tokens: usize) -> String {
    let budget = budget_tokens.max(200) * CHARS_PER_TOKEN;
    let mut out = String::new();
    out.push_str(&format!(
        "{} · {} section{} · {} words · {} file{}\n",
        paper.main_file,
        paper.sections.len(),
        plural(paper.sections.len()),
        paper.total_words,
        paper.files.len(),
        plural(paper.files.len()),
    ));

    // Rank decides what fits. Reading order decides how it is shown.
    let mut order: Vec<usize> = (0..paper.sections.len()).collect();
    order.sort_by(|a, b| {
        ranked.sections.get(*b).unwrap_or(&0.0).total_cmp(ranked.sections.get(*a).unwrap_or(&0.0)).then(a.cmp(b))
    });
    let refs = paper.ref_counts();
    let tail = tail_notes(paper);
    let mut chosen: BTreeSet<usize> = BTreeSet::new();
    let mut used = out.len() + tail.len();
    for i in order {
        let block = section_block(paper, i, &refs);
        if used + block.len() > budget && !chosen.is_empty() {
            continue;
        }
        used += block.len();
        chosen.insert(i);
    }

    let mut last_file = String::new();
    for i in &chosen {
        let s = paper.section(*i);
        if s.file != last_file {
            out.push_str(&format!("\n{}\n", s.file));
            last_file = s.file.clone();
        }
        out.push_str(&section_block(paper, *i, &refs));
    }
    if chosen.len() < paper.sections.len() {
        out.push_str(&format!("\n… {} more section(s) not shown; ask for a focus or a larger budget.\n", paper.sections.len() - chosen.len()));
    }
    out.push_str(&tail);
    out
}

fn section_block(paper: &Paper, i: usize, refs: &std::collections::BTreeMap<&str, usize>) -> String {
    let s = paper.section(i);
    let indent = "  ".repeat(s.level.max(1) as usize);
    let mut block = format!("{indent}§{} {}", s.number, s.title);
    if let Some(l) = &s.label {
        block.push_str(&format!(" [{l}]"));
    }
    block.push_str(&format!(" · {}:{} · {} words\n", s.file, s.line, s.words));
    for o in &s.objects {
        let o = &paper.objects[*o];
        let label = o.label.as_deref().unwrap_or("(unlabelled)");
        let mut line = format!("{indent}  {} {label}", o.kind.as_str());
        if let Some(c) = &o.caption {
            let short: String = c.chars().take(60).collect();
            line.push_str(&format!(" — {short}{}", if c.chars().count() > 60 { "…" } else { "" }));
        }
        let used = o.label.as_deref().and_then(|l| refs.get(l)).copied().unwrap_or(0);
        line.push_str(&match used {
            0 => " · never referenced".to_string(),
            1 => " · referenced once".to_string(),
            n => format!(" · referenced {n}×"),
        });
        block.push_str(&line);
        block.push('\n');
    }
    if !s.cites.is_empty() {
        let keys: Vec<&str> = s.cites.iter().take(12).map(String::as_str).collect();
        block.push_str(&format!(
            "{indent}  cites: {}{}\n",
            keys.join(", "),
            if s.cites.len() > 12 { format!(", +{} more", s.cites.len() - 12) } else { String::new() }
        ));
    }
    block
}

/// What the parse noticed: broken references, labels nobody uses, uncited entries.
fn tail_notes(paper: &Paper) -> String {
    let mut out = String::new();
    if !paper.undefined_refs.is_empty() {
        let list: Vec<String> =
            paper.undefined_refs.iter().take(8).map(|(r, f, l)| format!("{r} ({f}:{l})")).collect();
        out.push_str(&format!("\nundefined references: {}\n", list.join(", ")));
    }
    let refs = paper.ref_counts();
    let unused: Vec<&str> = paper
        .objects
        .iter()
        .filter_map(|o| o.label.as_deref())
        .filter(|l| !refs.contains_key(l))
        .take(8)
        .collect();
    if !unused.is_empty() {
        out.push_str(&format!("never referenced: {}\n", unused.join(", ")));
    }
    if !paper.bib.is_empty() {
        let cites = paper.cite_counts();
        let uncited: Vec<&str> =
            paper.bib.iter().map(|b| b.key.as_str()).filter(|k| !cites.contains_key(k)).take(8).collect();
        out.push_str(&format!(
            "bibliography: {} entries, {} cited{}\n",
            paper.bib.len(),
            cites.len(),
            if uncited.is_empty() { String::new() } else { format!(", uncited: {}", uncited.join(", ")) }
        ));
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::scan;
    use crate::rank::{rank, Focus};

    const DOC: &str = r#"
\section{Introduction}\label{sec:intro}
Some words here to count, and a citation \cite{knuth84}.
\section{Method}\label{sec:method}
\begin{figure}\caption{A very long caption that goes on and on past the sixty character limit for sure}\label{fig:one}\end{figure}
See \ref{fig:one} and \ref{sec:nowhere}.
\section{Results}\label{sec:results}
\begin{table}\caption{Numbers}\label{tab:unused}\end{table}
\bibliography{refs}
"#;

    fn build() -> (tempfile::TempDir, Paper) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), DOC).unwrap();
        std::fs::write(dir.path().join("refs.bib"), "@misc{knuth84, title={T}}\n@misc{never, title={U}}\n").unwrap();
        let p = scan(dir.path(), "main.tex");
        (dir, p)
    }

    #[test]
    fn map_shows_structure_and_notes() {
        let (_d, p) = build();
        let map = render(&p, &rank(&p, &Focus::Whole), 2000);
        assert!(map.starts_with("main.tex · 3 sections"), "{map}");
        assert!(map.contains("§2 Method [sec:method]"), "{map}");
        assert!(map.contains("figure fig:one — A very long caption"), "{map}");
        assert!(map.contains("…"), "long captions are cut: {map}");
        assert!(map.contains("referenced once"), "{map}");
        assert!(map.contains("cites: knuth84"), "{map}");
        assert!(map.contains("undefined references: sec:nowhere (main.tex:6)"), "{map}");
        assert!(map.contains("never referenced: tab:unused"), "{map}");
        assert!(map.contains("bibliography: 2 entries, 1 cited, uncited: never"), "{map}");
    }

    #[test]
    fn a_small_budget_keeps_the_focus_and_says_what_it_dropped() {
        // Forty sections, one of which the focus points at.
        let mut doc = String::from("\\section{Target}\\label{sec:target}\nThe part we care about.\n");
        for i in 0..40 {
            doc.push_str(&format!("\\section{{Filler {i}}}\\label{{sec:f{i}}}\nPadding words in a section that is not the focus.\n"));
        }
        doc.push_str("\\section{Last}\nWe refer to \\ref{sec:target}.\n");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), &doc).unwrap();
        let p = scan(dir.path(), "main.tex");
        let ranked = rank(&p, &Focus::Name("sec:target".into()));

        let small = render(&p, &ranked, 300);
        assert!(small.contains("§1 Target"), "the focused section survives: {small}");
        assert!(small.contains("more section(s) not shown"), "{small}");
        assert!(small.len() < render(&p, &ranked, 8000).len());
        assert!(!render(&p, &ranked, 8000).contains("more section(s) not shown"));
    }
}
