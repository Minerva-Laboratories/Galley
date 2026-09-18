//! Fill in what a bibliography entry is missing, and find preprints that have since been
//! published. Every suggestion names its source. Nothing is written until the author accepts it.

use std::collections::BTreeMap;

use galley_index::{BibEntry, Paper};
use serde::Serialize;
use serde_json::Value;

use crate::client::{crossref_search, crossref_work, Client};
use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suggestion {
    pub key: String,
    /// The BibTeX field to add or correct.
    pub field: String,
    pub value: String,
    /// Where it came from, for the author to judge.
    pub source: String,
}

/// Look up each entry that needs something: missing fields by DOI, published versions by title.
pub async fn enrich(client: &Client, paper: &Paper) -> (Vec<Suggestion>, Vec<Error>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for entry in &paper.bib {
        let wants = entry.missing_fields();
        if let Some(doi) = entry.doi.as_deref().filter(|_| !wants.is_empty()) {
            match client.json("Crossref", &crossref_work(doi)).await {
                Ok(v) => out.extend(from_crossref(entry, &v["message"], &wants)),
                Err(Error::NotFound(_)) => {}
                Err(e) => problems.push(e),
            }
        }
        if entry.looks_like_preprint() {
            let Some(title) = entry.title.as_deref() else { continue };
            match client.json("Crossref", &crossref_search(title)).await {
                Ok(v) => {
                    if let Some(found) = published_version(title, &v, entry.year.as_deref()) {
                        out.push(Suggestion {
                            key: entry.key.clone(),
                            field: "doi".into(),
                            value: found.0.clone(),
                            source: format!("Crossref: {} ({})", found.1, found.2),
                        });
                        if entry.venue.is_none() && !found.1.is_empty() {
                            out.push(Suggestion {
                                key: entry.key.clone(),
                                field: "booktitle".into(),
                                value: found.1,
                                source: "Crossref".into(),
                            });
                        }
                    }
                }
                Err(Error::NotFound(_)) => {}
                Err(e) => problems.push(e),
            }
        }
    }
    (out, problems)
}

/// Fields a Crossref record can fill in for an entry.
pub fn from_crossref(entry: &BibEntry, message: &Value, wants: &[&str]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let mut add = |field: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            out.push(Suggestion { key: entry.key.clone(), field: field.into(), value: v, source: "Crossref".into() });
        }
    };
    if wants.contains(&"title") {
        add("title", message["title"].get(0).and_then(Value::as_str).map(str::to_string));
    }
    if wants.contains(&"year") {
        add("year", message["issued"]["date-parts"].get(0).and_then(|p| p.get(0)).and_then(Value::as_i64).map(|y| y.to_string()));
    }
    if wants.contains(&"author") {
        let names: Vec<String> = message["author"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| match (p["family"].as_str(), p["given"].as_str()) {
                        (Some(f), Some(g)) => Some(format!("{f}, {g}")),
                        (Some(f), None) => Some(f.to_string()),
                        _ => p["name"].as_str().map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        add("author", (!names.is_empty()).then(|| names.join(" and ")));
    }
    if entry.venue.is_none() {
        add("journal", message["container-title"].get(0).and_then(Value::as_str).map(str::to_string));
    }
    out
}

/// A Crossref search result that is the same work, published: returns (DOI, venue, year).
///
/// A title match alone is not enough. The same title turns up years later in a book chapter or a
/// reprint, which the author did not cite. When the entry has a year, the published version must
/// be from that year or the few years after it.
pub fn published_version(title: &str, search: &Value, entry_year: Option<&str>) -> Option<(String, String, String)> {
    let items = search["message"]["items"].as_array()?;
    let want = normalise(title);
    for item in items {
        let kind = item["type"].as_str().unwrap_or("");
        // `posted-content` is the preprint itself.
        if kind == "posted-content" {
            continue;
        }
        let found = item["title"].get(0).and_then(Value::as_str).unwrap_or("");
        if normalise(found) != want {
            continue;
        }
        let doi = item["DOI"].as_str()?.to_lowercase();
        let venue = item["container-title"].get(0).and_then(Value::as_str).unwrap_or("").to_string();
        let year = item["issued"]["date-parts"].get(0).and_then(|p| p.get(0)).and_then(Value::as_i64);
        if let (Some(entry), Some(found)) = (entry_year.and_then(|y| y.parse::<i64>().ok()), year) {
            if found < entry || found > entry + 4 {
                continue;
            }
        }
        return Some((doi, venue, year.map(|y| y.to_string()).unwrap_or_default()));
    }
    None
}

/// Titles compare on their words alone: punctuation and case differ between catalogues.
pub fn normalise(title: &str) -> String {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Suggestions grouped by entry, as a report.
pub fn render(suggestions: &[Suggestion], problems: &[Error]) -> String {
    let mut out = String::new();
    let mut by_key: BTreeMap<&str, Vec<&Suggestion>> = BTreeMap::new();
    for s in suggestions {
        by_key.entry(s.key.as_str()).or_default().push(s);
    }
    if by_key.is_empty() {
        out.push_str("Nothing to fill in.\n");
    }
    for (key, items) in by_key {
        out.push_str(&format!("{key}\n"));
        for s in items {
            let value: String = s.value.chars().take(90).collect();
            out.push_str(&format!("  {} = {{{}}}   ({})\n", s.field, value, s.source));
        }
    }
    for p in problems.iter().take(3) {
        out.push_str(&format!("note: {p}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str) -> BibEntry {
        BibEntry { key: key.into(), kind: "misc".into(), ..BibEntry::default() }
    }

    #[test]
    fn crossref_fills_only_what_is_missing() {
        let message: Value = serde_json::from_str(
            r#"{"title":["Literate Programming"],"issued":{"date-parts":[[1984,2,1]]},
                "container-title":["The Computer Journal"],
                "author":[{"family":"Knuth","given":"D. E."},{"name":"The LaTeX Project"}]}"#,
        )
        .unwrap();
        let e = entry("knuth84");
        let got = from_crossref(&e, &message, &["author", "year"]);
        let fields: Vec<(&str, &str)> = got.iter().map(|s| (s.field.as_str(), s.value.as_str())).collect();
        assert!(fields.contains(&("author", "Knuth, D. E. and The LaTeX Project")), "{fields:?}");
        assert!(fields.contains(&("year", "1984")));
        assert!(fields.contains(&("journal", "The Computer Journal")));
        assert!(!fields.iter().any(|(f, _)| *f == "title"), "title was not asked for: {fields:?}");
    }

    #[test]
    fn a_published_version_is_recognised_and_a_preprint_is_not() {
        let search: Value = serde_json::from_str(
            r#"{"message":{"items":[
              {"DOI":"10.48550/ARXIV.2406.09246","type":"posted-content","title":["OpenVLA: An Open-Source Vision-Language-Action Model"]},
              {"DOI":"10.15607/RSS.2024.XX.016","type":"proceedings-article","title":["OpenVLA: an open source Vision Language Action model!"],
               "container-title":["Robotics: Science and Systems"],"issued":{"date-parts":[[2024]]}}
            ]}}"#,
        )
        .unwrap();
        let found = published_version("OpenVLA: An Open-Source Vision-Language-Action Model", &search, Some("2024")).unwrap();
        assert_eq!(found.0, "10.15607/rss.2024.xx.016", "the proceedings version, lowercased");
        assert_eq!(found.1, "Robotics: Science and Systems");
        assert_eq!(found.2, "2024");

        // A different paper with a similar name is not a match.
        let other: Value = serde_json::from_str(
            r#"{"message":{"items":[{"DOI":"10.1/x","type":"journal-article","title":["A Completely Different Paper"]}]}}"#,
        )
        .unwrap();
        assert!(published_version("OpenVLA: An Open-Source Vision-Language-Action Model", &other, None).is_none());
    }

    #[test]
    fn a_much_later_reprint_is_not_the_published_version() {
        // The same title in a 2026 book is not where a 2020 paper appeared.
        let search: Value = serde_json::from_str(
            r#"{"message":{"items":[
              {"DOI":"10.65525/chapter","type":"book-chapter","title":["Language Models are Few-Shot Learners"],
               "container-title":["Neural Horizons"],"issued":{"date-parts":[[2026]]}}
            ]}}"#,
        )
        .unwrap();
        assert!(published_version("Language Models are Few-Shot Learners", &search, Some("2020")).is_none());
        // Within a few years of the preprint, it is the publication.
        assert!(published_version("Language Models are Few-Shot Learners", &search, Some("2023")).is_some());
        // With no year on the entry, the author judges from the report.
        assert!(published_version("Language Models are Few-Shot Learners", &search, None).is_some());
    }

    #[test]
    fn report_groups_by_entry() {
        let s = vec![
            Suggestion { key: "a".into(), field: "year".into(), value: "1984".into(), source: "Crossref".into() },
            Suggestion { key: "a".into(), field: "author".into(), value: "Knuth".into(), source: "Crossref".into() },
        ];
        let text = render(&s, &[]);
        assert!(text.starts_with("a\n"), "{text}");
        assert!(text.contains("year = {1984}   (Crossref)"), "{text}");
        assert!(render(&[], &[]).contains("Nothing to fill in"));
    }
}
