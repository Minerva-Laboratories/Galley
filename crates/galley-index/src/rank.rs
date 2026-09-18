//! Which parts of the paper matter to the current question.
//!
//! Aider ranks a repository with PageRank over a graph of files and symbols. A paper's graph is
//! smaller and already labelled, so the same idea runs in a few iterations over sections, objects
//! and bibliography entries. The seed is what the author or the agent is looking at.

use std::collections::BTreeMap;

use crate::parse::Paper;

/// What the map should be about: nothing in particular, a file, or a section, label or citation key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Whole,
    File(String),
    /// A label, a citation key, or a section title. They are matched in that order.
    Name(String),
}

impl Focus {
    /// `""` means the whole paper. `main.tex` means that file. Anything else is a name.
    pub fn parse(s: &str) -> Focus {
        let s = s.trim();
        if s.is_empty() {
            Focus::Whole
        } else if s.ends_with(".tex") || s.ends_with(".bib") {
            Focus::File(s.trim_start_matches("./").to_string())
        } else {
            Focus::Name(s.to_string())
        }
    }
}

/// Scores in `0..=1`, keyed by section index. Objects and bib entries inherit their section's score
/// when the map is rendered, so one table is enough here.
#[derive(Debug, Clone, Default)]
pub struct Ranked {
    pub sections: Vec<f32>,
    /// Labels and citation keys the focus points at, for highlighting.
    pub highlights: Vec<String>,
}

const DAMPING: f32 = 0.85;
const ITERATIONS: usize = 20;

/// Personalised PageRank over three kinds of edge. A section to a section it refers into, a section
/// to sections that cite the same work, and a section to its neighbours in reading order. A paper is
/// read in order, unlike a codebase.
pub fn rank(paper: &Paper, focus: &Focus) -> Ranked {
    let n = paper.sections.len();
    if n == 0 {
        return Ranked::default();
    }
    let label_owner: BTreeMap<&str, usize> = paper
        .sections
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.label.as_deref().map(|l| (l, i)))
        .chain(paper.objects.iter().filter_map(|o| Some((o.label.as_deref()?, o.section?))))
        .collect();

    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, s) in paper.sections.iter().enumerate() {
        for r in &s.refs {
            if let Some(&target) = label_owner.get(r.as_str()) {
                if target != i {
                    edges[i].push(target);
                    edges[target].push(i);
                }
            }
        }
        if i + 1 < n {
            edges[i].push(i + 1);
            edges[i + 1].push(i);
        }
    }
    // Sections that cite the same work are related.
    let mut by_cite: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in paper.sections.iter().enumerate() {
        for c in &s.cites {
            by_cite.entry(c.as_str()).or_default().push(i);
        }
    }
    for (_, group) in by_cite.iter().filter(|(_, g)| g.len() > 1 && g.len() <= 8) {
        for (a, &x) in group.iter().enumerate() {
            for &y in group.iter().skip(a + 1) {
                edges[x].push(y);
                edges[y].push(x);
            }
        }
    }

    // The seed: what the caller is looking at, or everything.
    let mut seed = vec![0f32; n];
    let mut highlights = Vec::new();
    match focus {
        Focus::Whole => seed.iter_mut().for_each(|s| *s = 1.0 / n as f32),
        Focus::File(path) => {
            for (i, s) in paper.sections.iter().enumerate() {
                if s.file == *path {
                    seed[i] = 1.0;
                }
            }
        }
        Focus::Name(name) => {
            let lower = name.to_ascii_lowercase();
            if let Some(&i) = label_owner.get(name.as_str()) {
                seed[i] = 1.0;
                highlights.push(name.clone());
            }
            for (i, s) in paper.sections.iter().enumerate() {
                if s.cites.iter().any(|c| c == name) {
                    seed[i] += 1.0;
                    if !highlights.contains(name) {
                        highlights.push(name.clone());
                    }
                }
                if s.title.to_ascii_lowercase().contains(&lower) || s.number == *name {
                    seed[i] += 1.0;
                }
            }
        }
    }
    let total: f32 = seed.iter().sum();
    if total <= 0.0 {
        seed.iter_mut().for_each(|s| *s = 1.0 / n as f32);
    } else {
        seed.iter_mut().for_each(|s| *s /= total);
    }

    let mut score = seed.clone();
    for _ in 0..ITERATIONS {
        let mut next = vec![0f32; n];
        for (i, out) in edges.iter().enumerate() {
            if out.is_empty() {
                continue;
            }
            let share = score[i] / out.len() as f32;
            for &j in out {
                next[j] += share;
            }
        }
        for (i, v) in next.iter_mut().enumerate() {
            *v = (1.0 - DAMPING) * seed[i] + DAMPING * *v;
        }
        score = next;
    }
    let max = score.iter().cloned().fold(f32::MIN, f32::max).max(f32::MIN_POSITIVE);
    for s in &mut score {
        *s /= max;
    }
    Ranked { sections: score, highlights }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::scan;

    fn paper(files: &[(&str, &str)]) -> (tempfile::TempDir, Paper) {
        let dir = tempfile::tempdir().unwrap();
        for (p, t) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, t).unwrap();
        }
        let p = scan(dir.path(), "main.tex");
        (dir, p)
    }

    const DOC: &str = r#"
\section{Intro}\label{sec:intro}
Words about nothing in particular.
\section{Method}\label{sec:method}
The method, described at length with many words in it.
\begin{equation}\label{eq:key}a=b\end{equation}
\section{Results}\label{sec:results}
We use \eqref{eq:key} and cite \cite{smith}.
\section{Unrelated}\label{sec:other}
Nothing links here at all.
\section{Discussion}
We cite \cite{smith} again.
"#;

    #[test]
    fn focus_moves_the_ranking() {
        let (_d, p) = paper(&[("main.tex", DOC)]);
        let whole = rank(&p, &Focus::Whole);
        assert_eq!(whole.sections.len(), 5);

        // Focusing an equation lifts the section that defines it and the one that references it.
        let eq = rank(&p, &Focus::Name("eq:key".into()));
        let method = p.sections.iter().position(|s| s.title == "Method").unwrap();
        let results = p.sections.iter().position(|s| s.title == "Results").unwrap();
        let other = p.sections.iter().position(|s| s.title == "Unrelated").unwrap();
        assert!(eq.sections[method] > eq.sections[other], "{:?}", eq.sections);
        assert!(eq.sections[results] > eq.sections[other], "{:?}", eq.sections);
        assert_eq!(eq.highlights, vec!["eq:key"]);

        // A citation key lifts every section citing it.
        let cited = rank(&p, &Focus::Name("smith".into()));
        let discussion = p.sections.iter().position(|s| s.title == "Discussion").unwrap();
        assert!(cited.sections[discussion] > cited.sections[other]);

        // An unknown name falls back to the whole paper rather than returning nothing.
        let unknown = rank(&p, &Focus::Name("nothing-here".into()));
        assert!(unknown.sections.iter().all(|s| *s > 0.0));
    }

    #[test]
    fn focus_parses_and_empty_papers_are_safe() {
        assert_eq!(Focus::parse(" "), Focus::Whole);
        assert_eq!(Focus::parse("sections/method.tex"), Focus::File("sections/method.tex".into()));
        assert_eq!(Focus::parse("tab:main"), Focus::Name("tab:main".into()));
        let empty = Paper::default();
        assert!(rank(&empty, &Focus::Whole).sections.is_empty());
    }
}
