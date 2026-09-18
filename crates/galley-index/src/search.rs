//! Section-aware passage retrieval: BM25 over the paragraphs the parse collected. A paper gives two
//! boosts. A section whose title or label matches, and passages that contain the query as a phrase.
//! No index on disk and no model. One pass over a document scores in microseconds.

use std::collections::BTreeMap;

use crate::parse::Paper;
use crate::CHARS_PER_TOKEN;

const K1: f32 = 1.2;
const B: f32 = 0.75;
/// A section whose title, label or number matches the query.
const SECTION_BOOST: f32 = 1.25;
/// The whole query appears as a phrase.
const PHRASE_BOOST: f32 = 1.6;

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk: usize,
    pub score: f32,
    /// Query terms this passage contains, to show why it matched.
    pub terms: Vec<String>,
}

/// Words as the index sees them: lowercase, no commands, hyphenated words both ways.
pub fn tokenize(text: &str) -> Vec<String> {
    let cleaned = crate::parse::prose(text).to_lowercase();
    let mut out = Vec::new();
    for raw in cleaned.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-')) {
        let word = raw.trim_matches(|c: char| c == '\'' || c == '-');
        if word.len() < 2 || !word.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        out.push(word.to_string());
        if word.contains('-') {
            out.extend(word.split('-').filter(|p| p.len() >= 2).map(str::to_string));
        }
    }
    out
}

/// The best passages for `query`, highest first.
pub fn search(paper: &Paper, query: &str, limit: usize) -> Vec<Hit> {
    let terms = tokenize(query);
    if terms.is_empty() || paper.chunks.is_empty() {
        return Vec::new();
    }
    let docs: Vec<Vec<String>> = paper.chunks.iter().map(|c| tokenize(&c.text)).collect();
    let n = docs.len() as f32;
    let avg_len = docs.iter().map(|d| d.len()).sum::<usize>() as f32 / n;

    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for doc in &docs {
        let mut seen: Vec<&str> = Vec::new();
        for w in doc {
            if !seen.contains(&w.as_str()) {
                seen.push(w.as_str());
                *df.entry(w.as_str()).or_default() += 1;
            }
        }
    }

    let phrase = query.trim().to_lowercase();
    let phrase = (terms.len() > 1).then(|| phrase.clone());
    let mut hits: Vec<Hit> = Vec::new();
    for (i, doc) in docs.iter().enumerate() {
        let len = doc.len() as f32;
        let mut score = 0.0;
        let mut matched: Vec<String> = Vec::new();
        for term in &terms {
            let tf = doc.iter().filter(|w| *w == term).count() as f32;
            if tf == 0.0 {
                continue;
            }
            if !matched.contains(term) {
                matched.push(term.clone());
            }
            let df = *df.get(term.as_str()).unwrap_or(&0) as f32;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            score += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * len / avg_len));
        }
        if score <= 0.0 {
            continue;
        }
        // The section's own name is evidence about its passages.
        if let Some(s) = paper.chunks[i].section.and_then(|s| paper.sections.get(s)) {
            let title = tokenize(&s.title);
            let label = s.label.as_deref().map(tokenize).unwrap_or_default();
            if terms.iter().any(|t| title.contains(t) || label.contains(t) || s.number == *t) {
                score *= SECTION_BOOST;
            }
        }
        if let Some(p) = &phrase {
            if paper.chunks[i].text.to_lowercase().contains(p) {
                score *= PHRASE_BOOST;
            }
        }
        hits.push(Hit { chunk: i, score, terms: matched });
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.cmp(&b.chunk)));
    hits.truncate(limit);
    hits
}

/// Hits as text for an agent. The default is one line each, which is enough to choose from.
/// `detail` returns the passages themselves (docs/RETRIEVAL.md §4, progressive disclosure).
pub fn render_hits(paper: &Paper, query: &str, hits: &[Hit], budget_tokens: usize, detail: bool) -> String {
    if hits.is_empty() {
        return format!("No passage matches {query:?}. Try fewer or different words, or call map for the structure.\n");
    }
    // One passage is always worth returning, so the floor is small.
    let budget = budget_tokens.max(60) * CHARS_PER_TOKEN;
    let mut out = format!("{} passage(s) for {query:?}, best first:\n", hits.len());
    for hit in hits {
        let c = &paper.chunks[hit.chunk];
        let where_ = match c.section.and_then(|s| paper.sections.get(s)) {
            Some(s) => {
                let label = s.label.as_deref().map(|l| format!(" [{l}]")).unwrap_or_default();
                format!("§{} {}{}", s.number, s.title, label)
            }
            None => "(before the first section)".to_string(),
        };
        let block = if detail {
            let passage = excerpt(&c.text, &hit.terms, 420);
            format!("\n{where_} · {}:{} · matched: {}\n  {passage}\n", c.file, c.line, hit.terms.join(", "))
        } else {
            format!("  {where_} · {}:{} · {}\n", c.file, c.line, excerpt(&c.text, &hit.terms, 90))
        };
        if out.len() + block.len() > budget && out.lines().count() > 2 {
            out.push_str("… more matches; narrow the query or raise the budget.\n");
            break;
        }
        out.push_str(&block);
    }
    if !detail && !hits.is_empty() {
        out.push_str("Ask again with detail for the passages themselves.\n");
    }
    out
}

/// A window of the passage around its first matching term, on word boundaries.
fn excerpt(text: &str, terms: &[String], width: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= width {
        return flat;
    }
    let lower = flat.to_lowercase();
    let at = terms.iter().filter_map(|t| lower.find(t.as_str())).min().unwrap_or(0);
    let start = flat[..at].char_indices().rev().nth(width / 3).map(|(i, _)| i).unwrap_or(0);
    let start = flat[..start].rfind(' ').map(|i| i + 1).unwrap_or(start);
    let end = flat[start..].char_indices().nth(width).map(|(i, _)| start + i).unwrap_or(flat.len());
    let end = flat[..end].rfind(' ').unwrap_or(end);
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        flat[start..end].trim(),
        if end < flat.len() { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::scan;

    const MAIN: &str = r#"
\section{Introduction}\label{sec:intro}
Vision-language-action policies are large and slow, and a smaller policy would be cheaper to deploy
on a real robot arm in a laboratory.

\section{Self-exposure}\label{sec:seal}
Self-exposure collects corrective labels along the states the policy actually visits, rather than
along the expert's own trajectory. The corrective label is affine in the visited state.

\section{Results}\label{sec:results}
The policy reaches a mean success of 0.831 on the long-horizon suite, which is better than models
nine times larger, and the table below breaks this down by suite.

\section{Compute}\label{sec:compute}
Training runs on one accelerator for eleven hours; inference is deterministic and fits in memory.
"#;

    fn paper() -> (tempfile::TempDir, Paper) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), MAIN).unwrap();
        let p = scan(dir.path(), "main.tex");
        (dir, p)
    }

    #[test]
    fn finds_the_right_section_not_the_first_one() {
        let (_d, p) = paper();
        assert_eq!(p.chunks.len(), 4, "one passage per paragraph");

        let hits = search(&p, "corrective labels along visited states", 3);
        let best = p.chunks[hits[0].chunk].section.unwrap();
        assert_eq!(p.sections[best].title, "Self-exposure");

        let hits = search(&p, "how long does training take", 3);
        let best = p.chunks[hits[0].chunk].section.unwrap();
        assert_eq!(p.sections[best].title, "Compute", "{:?}", hits);
    }

    #[test]
    fn a_section_name_lifts_its_own_passages() {
        let (_d, p) = paper();
        // "policy" appears in three passages. Naming the section decides which comes first.
        let plain = search(&p, "policy", 4);
        let named = search(&p, "results policy", 4);
        let first = p.chunks[named[0].chunk].section.unwrap();
        assert_eq!(p.sections[first].title, "Results");
        assert!(plain.len() >= 3);
    }

    #[test]
    fn renders_with_provenance_and_respects_the_budget() {
        let (_d, p) = paper();
        let hits = search(&p, "corrective label", 4);
        let text = render_hits(&p, "corrective label", &hits, 2000, true);
        assert!(text.contains("§2 Self-exposure [sec:seal] · main.tex:"), "{text}");
        assert!(text.contains("matched: corrective, label"), "{text}");
        assert!(!text.contains("\\section{"), "passages start after the heading: {text}");

        // With several hits, a small budget stops early and says so.
        let many = search(&p, "policy", 4);
        assert!(many.len() >= 3, "{many:?}");
        let full = render_hits(&p, "policy", &many, 2000, true);
        let tight = render_hits(&p, "policy", &many, 60, true);
        assert!(tight.len() < full.len() && tight.contains("more matches"), "{tight}");
        assert!(search(&p, "quantum chromodynamics", 4).is_empty());
        assert!(render_hits(&p, "quantum chromodynamics", &[], 500, false).starts_with("No passage matches"));

        // The default answer is one line per passage, and says how to get more.
        let compact = render_hits(&p, "corrective label", &hits, 2000, false);
        assert!(compact.lines().count() < text.lines().count(), "compact:\n{compact}\nfull:\n{text}");
        assert!(compact.contains("§2 Self-exposure [sec:seal] · main.tex:"), "{compact}");
        assert!(compact.contains("Ask again with detail"), "{compact}");
        // Across several hits, one line each instead of a passage each saves much context.
        let many = search(&p, "policy", 4);
        let compact_many = render_hits(&p, "policy", &many, 4000, false);
        let full_many = render_hits(&p, "policy", &many, 4000, true);
        assert!(compact_many.len() < full_many.len() * 3 / 4, "{} vs {}", compact_many.len(), full_many.len());
        assert!(compact_many.lines().all(|l| l.chars().count() < 160), "{compact_many}");
    }

    #[test]
    fn excerpt_windows_around_the_match() {
        let long = format!("{} corrective label {}", "filler ".repeat(80), "tail ".repeat(80));
        let e = excerpt(&long, &["corrective".to_string()], 60);
        assert!(e.contains("corrective label"), "{e}");
        assert!(e.starts_with('…') && e.ends_with('…'), "{e}");
        assert!(e.chars().count() < 100, "{e}");
    }
}
