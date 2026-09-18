//! Work the paper should probably cite: papers that the papers you already cite keep citing.
//!
//! This is a graph query. A candidate carries one number, how many of your own references cite it.
//! The author can then judge the candidate quickly.

use std::collections::BTreeMap;

use galley_index::Paper;
use serde::Serialize;
use serde_json::Value;

use crate::client::{openalex_work, openalex_works, Client};
use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub title: String,
    pub year: Option<i64>,
    pub doi: Option<String>,
    pub openalex: String,
    /// How many of the works you cite reference this one.
    pub shared: usize,
    /// Which of your citation keys those were.
    pub via: Vec<String>,
}

/// Candidates ranked by how many of your references cite them, excluding what you cite already.
pub async fn coverage(client: &Client, paper: &Paper, limit: usize) -> (Vec<Candidate>, usize, Vec<Error>) {
    let mut problems = Vec::new();
    let mut refs_by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut mine: Vec<String> = Vec::new();
    let cited = paper.cite_counts();

    for entry in paper.bib.iter().filter(|b| cited.contains_key(b.key.as_str())) {
        let Some(doi) = entry.doi.clone().or_else(|| entry.arxiv_id().map(|a| format!("10.48550/arXiv.{a}"))) else {
            continue;
        };
        match client.json("OpenAlex", &openalex_work(&doi)).await {
            Ok(work) => {
                if let Some(id) = work["id"].as_str() {
                    mine.push(id.to_string());
                }
                let refs = referenced_works(&work);
                if !refs.is_empty() {
                    refs_by_key.insert(entry.key.clone(), refs);
                }
            }
            Err(Error::NotFound(_)) => {}
            Err(e) => problems.push(e),
        }
    }

    let counted = tally(&refs_by_key, &mine);
    let top: Vec<(String, (usize, Vec<String>))> = counted.into_iter().take(limit).collect();
    if top.is_empty() {
        return (Vec::new(), refs_by_key.len(), problems);
    }
    // One request resolves up to fifty candidate ids to titles.
    let ids: Vec<String> = top.iter().map(|(id, _)| id.rsplit('/').next().unwrap_or(id).to_string()).collect();
    let mut out = Vec::new();
    match client.json("OpenAlex", &openalex_works(&ids)).await {
        Ok(page) => {
            let works: BTreeMap<String, &Value> = page["results"]
                .as_array()
                .map(|r| r.iter().filter_map(|w| Some((w["id"].as_str()?.to_string(), w))).collect())
                .unwrap_or_default();
            for (id, (shared, via)) in top {
                let Some(w) = works.get(&id) else { continue };
                out.push(Candidate {
                    title: w["title"].as_str().unwrap_or("(untitled)").to_string(),
                    year: w["publication_year"].as_i64(),
                    doi: w["doi"].as_str().map(|d| d.trim_start_matches("https://doi.org/").to_string()),
                    openalex: id,
                    shared,
                    via,
                });
            }
        }
        Err(e) => problems.push(e),
    }
    out.sort_by(|a, b| b.shared.cmp(&a.shared).then(a.title.cmp(&b.title)));
    (out, refs_by_key.len(), problems)
}

/// The works a record references, as OpenAlex ids.
pub fn referenced_works(work: &Value) -> Vec<String> {
    work["referenced_works"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Count how many of your references cite each work, dropping the ones you cite yourself.
/// Ties break on the citation keys, so the order never depends on hash map order.
pub fn tally(refs_by_key: &BTreeMap<String, Vec<String>>, mine: &[String]) -> Vec<(String, (usize, Vec<String>))> {
    let mut counts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, refs) in refs_by_key {
        for r in refs {
            if mine.iter().any(|m| m == r) {
                continue;
            }
            let via = counts.entry(r.clone()).or_default();
            if !via.contains(key) {
                via.push(key.clone());
            }
        }
    }
    let mut ranked: Vec<(String, (usize, Vec<String>))> =
        counts.into_iter().filter(|(_, via)| via.len() > 1).map(|(id, via)| (id, (via.len(), via))).collect();
    ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.1 .1.cmp(&b.1 .1)));
    ranked
}

pub fn render(candidates: &[Candidate], from: usize, problems: &[Error]) -> String {
    if candidates.is_empty() {
        let mut out = format!(
            "No candidates. {}\n",
            if from == 0 {
                "None of your cited works had a reference list in OpenAlex. Preprints often do not."
            } else {
                "Nothing is cited by more than one of your references."
            }
        );
        for p in problems.iter().take(3) {
            out.push_str(&format!("note: {p}\n"));
        }
        return out;
    }
    let mut out = format!(
        "Cited by several of your own references, but not by you (from {from} reference list{}):\n",
        if from == 1 { "" } else { "s" }
    );
    for c in candidates {
        let year = c.year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into());
        out.push_str(&format!("\n  {} ({year})\n", c.title));
        out.push_str(&format!("    cited by {} of your references: {}\n", c.shared, c.via.join(", ")));
        if let Some(doi) = &c.doi {
            out.push_str(&format!("    doi: {doi}\n"));
        }
    }
    out.push_str("\nEach is a candidate, not a recommendation: check it before citing it.\n");
    for p in problems.iter().take(3) {
        out.push_str(&format!("note: {p}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_shared_references_and_ignores_what_you_cite() {
        let mut refs = BTreeMap::new();
        refs.insert("smith".to_string(), vec!["W1".to_string(), "W2".to_string(), "MINE".to_string()]);
        refs.insert("jones".to_string(), vec!["W1".to_string(), "W3".to_string(), "MINE".to_string()]);
        refs.insert("lee".to_string(), vec!["W1".to_string(), "W2".to_string()]);
        let ranked = tally(&refs, &["MINE".to_string()]);

        // W1 with three beats W2 with two. W3 appears once and is dropped. MINE is already cited.
        assert_eq!(ranked.len(), 2, "{ranked:?}");
        assert_eq!(ranked[0].0, "W1");
        assert_eq!(ranked[0].1 .0, 3);
        assert_eq!(ranked[0].1 .1, vec!["jones", "lee", "smith"]);
        assert_eq!(ranked[1].0, "W2");
        assert!(!ranked.iter().any(|(id, _)| id == "MINE"));
    }

    #[test]
    fn reads_reference_lists_and_reports_emptiness_honestly() {
        let work: Value =
            serde_json::from_str(r#"{"id":"https://openalex.org/W1","referenced_works":["https://openalex.org/W9"]}"#)
                .unwrap();
        assert_eq!(referenced_works(&work), vec!["https://openalex.org/W9"]);
        let preprint: Value = serde_json::from_str(r#"{"id":"https://openalex.org/W2"}"#).unwrap();
        assert!(referenced_works(&preprint).is_empty());

        assert!(render(&[], 0, &[]).contains("Preprints often do not"));
        assert!(render(&[], 4, &[]).contains("more than one of your references"));
    }

    #[test]
    fn report_shows_the_evidence() {
        let c = Candidate {
            title: "Attention Is All You Need".into(),
            year: Some(2017),
            doi: Some("10.5555/3295222.3295349".into()),
            openalex: "https://openalex.org/W1".into(),
            shared: 3,
            via: vec!["smith".into(), "jones".into()],
        };
        let text = render(&[c], 5, &[]);
        assert!(text.contains("Attention Is All You Need (2017)"), "{text}");
        assert!(text.contains("cited by 3 of your references: smith, jones"), "{text}");
        assert!(text.contains("doi: 10.5555/3295222.3295349"));
        assert!(text.contains("not a recommendation"));
    }
}
