//! The bibliography as a graph: what you cite, where you cite it, and what you cite together.
//!
//! Two works cited in the same section are related in the author's own mind. That signal is
//! stronger than catalogue data, and it needs no network. Catalogue data adds candidates on top
//! (galley-lit). This module works without the network.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::bib::{audit, Issue};
use crate::parse::Paper;

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub key: String,
    pub title: Option<String>,
    pub year: Option<String>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    /// How many sections cite it.
    pub cited: usize,
    /// Section numbers that cite it, in reading order.
    pub sections: Vec<String>,
    /// Issue codes from the audit, so the view can mark an entry that needs attention.
    pub issues: Vec<String>,
    /// For a duplicate, the entry to merge into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    pub file: String,
    pub line: u32,
}

/// Two entries cited in the same section. `weight` is how many sections do that.
#[derive(Debug, Clone, Serialize)]
pub struct Edge {
    pub a: String,
    pub b: String,
    pub weight: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Keys cited with no entry. They have no node, but the reader needs to know.
    pub missing: Vec<String>,
}

/// Build the graph from the project alone.
pub fn citation_graph(paper: &Paper) -> Graph {
    let findings = audit(paper);
    let mut issues: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut duplicate_of: BTreeMap<&str, String> = BTreeMap::new();
    for f in &findings {
        issues.entry(f.key.as_str()).or_default().push(f.issue.code().to_string());
        if f.issue == Issue::Duplicate {
            if let Some(r) = &f.related {
                duplicate_of.insert(f.key.as_str(), r.clone());
            }
        }
    }
    let cites = paper.cite_counts();

    let mut nodes = Vec::new();
    for entry in &paper.bib {
        let sections: Vec<String> = paper
            .sections
            .iter()
            .filter(|s| s.cites.contains(&entry.key))
            .map(|s| s.number.clone())
            .collect();
        nodes.push(Node {
            key: entry.key.clone(),
            title: entry.title.clone(),
            year: entry.year.clone(),
            venue: entry.venue.clone(),
            doi: entry.doi.clone(),
            cited: sections.len(),
            sections,
            issues: issues.get(entry.key.as_str()).cloned().unwrap_or_default(),
            duplicate_of: duplicate_of.get(entry.key.as_str()).cloned(),
            file: entry.file.clone(),
            line: entry.line,
        });
    }
    nodes.sort_by(|a, b| b.cited.cmp(&a.cited).then(a.key.cmp(&b.key)));

    // Co-citation: every pair of keys a section cites together.
    let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
    for section in &paper.sections {
        let mut keys: Vec<&String> = section.cites.iter().filter(|k| cites.contains_key(k.as_str())).collect();
        keys.sort();
        keys.dedup();
        // A section citing forty works says little about any pair of them.
        if keys.len() > 25 {
            continue;
        }
        for (i, a) in keys.iter().enumerate() {
            for b in keys.iter().skip(i + 1) {
                *pairs.entry(((*a).clone(), (*b).clone())).or_default() += 1;
            }
        }
    }
    let known: Vec<&str> = paper.bib.iter().map(|b| b.key.as_str()).collect();
    let mut edges: Vec<Edge> = pairs
        .into_iter()
        .filter(|((a, b), _)| known.contains(&a.as_str()) && known.contains(&b.as_str()))
        .map(|((a, b), weight)| Edge { a, b, weight })
        .collect();
    edges.sort_by(|x, y| y.weight.cmp(&x.weight).then(x.a.cmp(&y.a)).then(x.b.cmp(&y.b)));

    let missing = findings.iter().filter(|f| f.issue == Issue::Missing).map(|f| f.key.clone()).collect();
    Graph { nodes, edges, missing }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::scan;

    const MAIN: &str = r#"
\section{Introduction}\label{sec:intro}
Transformers~\cite{attention,bert} changed everything, and so did \cite{adam}.
\section{Method}\label{sec:method}
We follow \cite{attention} and \cite{bert}, and we also lean on \cite{ghost}.
\section{Results}\label{sec:results}
Only \cite{adam} matters here.
\bibliography{refs}
"#;

    const BIB: &str = r#"@inproceedings{attention, author={Vaswani, A}, title={Attention Is All You Need}, year={2017}, booktitle={NeurIPS}, doi={10.5555/x}}
@inproceedings{bert, author={Devlin, J}, title={BERT}, year={2019}, booktitle={NAACL}, doi={10.18653/v1/N19-1423}}
@misc{adam, author={Kingma, D}, title={Adam}, year={2015}, eprint={1412.6980}, archivePrefix={arXiv}}
@misc{lonely, author={Nobody}, title={Uncited}, year={2001}}
"#;

    fn graph() -> (tempfile::TempDir, Graph) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), MAIN).unwrap();
        std::fs::write(dir.path().join("refs.bib"), BIB).unwrap();
        let paper = scan(dir.path(), "main.tex");
        let g = citation_graph(&paper);
        (dir, g)
    }

    #[test]
    fn nodes_carry_where_they_are_cited_and_what_is_wrong() {
        let (_d, g) = graph();
        let keys: Vec<&str> = g.nodes.iter().map(|n| n.key.as_str()).collect();
        // Most-cited first: attention and bert are in two sections each, adam in two, lonely in none.
        assert_eq!(keys.len(), 4);
        assert_eq!(keys[3], "lonely");

        let attention = g.nodes.iter().find(|n| n.key == "attention").unwrap();
        assert_eq!(attention.cited, 2);
        assert_eq!(attention.sections, vec!["1", "2"]);
        assert!(attention.issues.is_empty(), "{:?}", attention.issues);
        assert_eq!(attention.title.as_deref(), Some("Attention Is All You Need"));

        let lonely = g.nodes.iter().find(|n| n.key == "lonely").unwrap();
        assert_eq!(lonely.cited, 0);
        assert!(lonely.issues.iter().any(|i| i == "bib-uncited-entry"), "{:?}", lonely.issues);
        let adam = g.nodes.iter().find(|n| n.key == "adam").unwrap();
        assert!(adam.issues.iter().any(|i| i == "bib-preprint"));

        assert_eq!(g.missing, vec!["ghost"], "a cited key with no entry is reported separately");
    }

    #[test]
    fn edges_are_works_cited_together() {
        let (_d, g) = graph();
        let weight = |a: &str, b: &str| {
            g.edges.iter().find(|e| (e.a == a && e.b == b) || (e.a == b && e.b == a)).map(|e| e.weight)
        };
        // attention+bert share two sections. Each shares one with adam, the introduction.
        assert_eq!(weight("attention", "bert"), Some(2));
        assert_eq!(weight("adam", "attention"), Some(1));
        assert_eq!(g.edges[0].weight, 2, "heaviest first");
        // `ghost` has no entry, so it is in no edge.
        assert!(!g.edges.iter().any(|e| e.a == "ghost" || e.b == "ghost"));
        // An uncited entry is a node with no edges.
        assert!(!g.edges.iter().any(|e| e.a == "lonely" || e.b == "lonely"));
    }
}
