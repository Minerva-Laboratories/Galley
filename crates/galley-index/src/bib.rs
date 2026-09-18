//! Bibliography hygiene, from the project's own files: entries cited but not defined, the same work
//! entered twice, entries nothing cites, missing fields, and preprints that may have been published
//! since. Everything here is offline. To confirm a preprint's published version needs the network,
//! so that work lives elsewhere (docs/RETRIEVAL.md §5).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::parse::Paper;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Issue {
    /// A `\cite` key with no entry: the build will print a `?` where the citation should be.
    Missing,
    /// Two entries for the same work, by DOI, arXiv id or title.
    Duplicate,
    /// An entry nothing cites. The packer drops it from a submission.
    Uncited,
    /// An entry without the fields a reader needs.
    Incomplete,
    /// An arXiv entry with no DOI or venue.
    Preprint,
}

impl Issue {
    pub fn code(self) -> &'static str {
        match self {
            Issue::Missing => "bib-missing-entry",
            Issue::Duplicate => "bib-duplicate-entry",
            Issue::Uncited => "bib-uncited-entry",
            Issue::Incomplete => "bib-incomplete-entry",
            Issue::Preprint => "bib-preprint",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub issue: Issue,
    pub key: String,
    /// The other entry involved: for a duplicate, the one to keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related: Option<String>,
    /// Where to look: the .bib entry, or the first `\cite` of a missing key.
    pub file: String,
    pub line: u32,
    pub message: String,
    pub hint: Option<String>,
}

/// Audit the bibliography against what the document cites.
pub fn audit(paper: &Paper) -> Vec<Finding> {
    let mut out = Vec::new();
    let cites = paper.cite_counts();
    let by_key: BTreeMap<&str, &crate::parse::BibEntry> = paper.bib.iter().map(|b| (b.key.as_str(), b)).collect();

    // Cited, but nothing defines it. Point at the section that cites it, which is where the author is.
    for (key, count) in &cites {
        if by_key.contains_key(key) {
            continue;
        }
        let site = paper.sections.iter().find(|s| s.cites.iter().any(|c| c == key));
        let (file, line) = site.map(|s| (s.file.clone(), s.line)).unwrap_or((paper.main_file.clone(), 1));
        out.push(Finding {
            issue: Issue::Missing,
            key: (*key).to_string(),
            related: None,
            file,
            line,
            message: format!(
                "\\cite{{{key}}} has no entry in the bibliography (cited in {} section{})",
                count,
                if *count == 1 { "" } else { "s" }
            ),
            hint: Some("Add the entry, or fix the key. LaTeX prints [?] for it.".into()),
        });
    }

    // The same work twice: DOI first, then arXiv id, then a normalised title.
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for entry in &paper.bib {
        let identities = [
            entry.doi.clone().filter(|d| !d.starts_with("10.48550/arxiv.")).map(|d| format!("doi:{d}")),
            entry.arxiv_id().map(|a| format!("arxiv:{a}")),
            entry.title_key().map(|t| format!("title:{t}")),
        ];
        for id in identities.into_iter().flatten() {
            match seen.get(&id) {
                Some(first) if *first != entry.key => {
                    let what = id.split_once(':').map(|(k, _)| k).unwrap_or("title");
                    out.push(Finding {
                        issue: Issue::Duplicate,
                        key: entry.key.clone(),
                        related: Some(first.to_string()),
                        file: entry.file.clone(),
                        line: entry.line,
                        message: format!("{} and {} are the same work (same {what})", entry.key, first),
                        hint: Some(format!("Keep one key and point every \\cite at it; {first} came first.")),
                    });
                    break;
                }
                Some(_) => break,
                None => {
                    seen.insert(id, entry.key.as_str());
                }
            }
        }
    }

    for entry in &paper.bib {
        if !cites.contains_key(entry.key.as_str()) {
            out.push(Finding {
                issue: Issue::Uncited,
                key: entry.key.clone(),
                related: None,
                file: entry.file.clone(),
                line: entry.line,
                message: format!("{} is never cited", entry.key),
                hint: Some("The submission packer drops uncited entries; remove it or cite it.".into()),
            });
        }
        let missing = entry.missing_fields();
        if !missing.is_empty() {
            out.push(Finding {
                issue: Issue::Incomplete,
                key: entry.key.clone(),
                related: None,
                file: entry.file.clone(),
                line: entry.line,
                message: format!("{} has no {}", entry.key, missing.join(", no ")),
                hint: Some("Paste the DOI or arXiv id and Galley can fill the entry in.".into()),
            });
        }
        if entry.looks_like_preprint() {
            out.push(Finding {
                issue: Issue::Preprint,
                key: entry.key.clone(),
                related: None,
                file: entry.file.clone(),
                line: entry.line,
                message: format!("{} cites arXiv:{} with no DOI or venue", entry.key, entry.arxiv.clone().unwrap_or_default()),
                hint: Some("If it has been published since, cite the published version.".into()),
            });
        }
    }

    out.sort_by(|a, b| (a.issue as u8).cmp(&(b.issue as u8)).then(a.key.cmp(&b.key)));
    out
}

/// The audit as a report for a person or an agent.
pub fn render(paper: &Paper, findings: &[Finding]) -> String {
    let cited = paper.cite_counts().len();
    let mut out = format!(
        "bibliography: {} entr{} · {cited} cited · {} issue{}\n",
        paper.bib.len(),
        if paper.bib.len() == 1 { "y" } else { "ies" },
        findings.len(),
        if findings.len() == 1 { "" } else { "s" }
    );
    if findings.is_empty() {
        out.push_str("Nothing to fix.\n");
        return out;
    }
    let mut last = None;
    for f in findings {
        if last != Some(f.issue) {
            out.push_str(&format!("\n{}\n", f.issue.code()));
            last = Some(f.issue);
        }
        out.push_str(&format!("  {} · {}:{}\n", f.message, f.file, f.line));
        if let Some(h) = &f.hint {
            out.push_str(&format!("      {h}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::scan;

    const MAIN: &str = r#"
\section{Intro}
We cite \cite{knuth84}, \cite{same1}, \cite{ghost} and \cite{ghost} again, plus \cite{draft}.
\bibliography{refs}
"#;

    const BIB: &str = r#"@article{knuth84,
  author = {Donald Knuth}, title = {Literate Programming}, year = {1984},
  journal = {The Computer Journal}, doi = {10.1093/comjnl/27.2.97}}

@article{same1, author = {A Smith}, title = {On the Same Thing}, year = {2024},
  doi = {10.1000/XYZ}, journal = {J. Things}}

@misc{same2, author = {A Smith}, title = {On the same thing}, year = {2024},
  doi = {https://doi.org/10.1000/xyz}}

@misc{draft, author = {B Jones}, title = {A Preprint}, year = {2025},
  eprint = {2406.09246}, archivePrefix = {arXiv}}

@misc{loose, title = {No author and no year}}
"#;

    fn paper() -> (tempfile::TempDir, Paper) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), MAIN).unwrap();
        std::fs::write(dir.path().join("refs.bib"), BIB).unwrap();
        let p = scan(dir.path(), "main.tex");
        (dir, p)
    }

    #[test]
    fn parses_the_fields_that_identify_a_work() {
        let (_d, p) = paper();
        let knuth = p.bib.iter().find(|b| b.key == "knuth84").unwrap();
        assert_eq!(knuth.doi.as_deref(), Some("10.1093/comjnl/27.2.97"));
        assert_eq!(knuth.venue.as_deref(), Some("The Computer Journal"));
        assert_eq!(knuth.author.as_deref(), Some("Donald Knuth"));
        let draft = p.bib.iter().find(|b| b.key == "draft").unwrap();
        assert_eq!(draft.arxiv.as_deref(), Some("2406.09246"));
        assert!(draft.looks_like_preprint());
        assert!(!knuth.looks_like_preprint());

        // An arXiv DOI names the same preprint as an eprint field, and is not a publication.
        let arxiv_doi = crate::parse::BibEntry {
            doi: Some("10.48550/arxiv.2406.09246".into()),
            ..draft.clone()
        };
        assert_eq!(arxiv_doi.arxiv_id().as_deref(), Some("2406.09246"));
        assert!(arxiv_doi.looks_like_preprint());
    }

    #[test]
    fn finds_what_is_wrong_with_the_bibliography() {
        let (_d, p) = paper();
        let f = audit(&p);
        let of = |i: Issue| f.iter().filter(|x| x.issue == i).map(|x| x.key.as_str()).collect::<Vec<_>>();

        assert_eq!(of(Issue::Missing), vec!["ghost"], "cited with no entry");
        assert!(f.iter().any(|x| x.issue == Issue::Missing && x.message.contains("cited in 1 section")));
        // The DOI matches whether or not it is written as a URL, and case does not matter.
        assert_eq!(of(Issue::Duplicate), vec!["same2"]);
        let dup = f.iter().find(|x| x.issue == Issue::Duplicate).unwrap();
        assert_eq!(dup.related.as_deref(), Some("same1"), "the entry to merge into");
        assert_eq!(of(Issue::Uncited), vec!["loose", "same2"]);
        assert_eq!(of(Issue::Incomplete), vec!["loose"]);
        assert_eq!(of(Issue::Preprint), vec!["draft"]);

        let report = render(&p, &f);
        assert!(report.starts_with("bibliography: 5 entries · 4 cited · 6 issues"), "{report}");
        assert!(report.contains("bib-missing-entry"), "{report}");
        assert!(report.contains("same2 and same1 are the same work (same doi)"), "{report}");
    }

    #[test]
    fn a_clean_bibliography_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), "\\section{A}\nText \\cite{ok}.\n\\bibliography{refs}\n").unwrap();
        std::fs::write(
            dir.path().join("refs.bib"),
            "@article{ok, author={A}, title={T}, year={2020}, journal={J}, doi={10.1/x}}\n",
        )
        .unwrap();
        let p = scan(dir.path(), "main.tex");
        let f = audit(&p);
        assert!(f.is_empty(), "{f:?}");
        assert!(render(&p, &f).contains("Nothing to fix"));
    }
}

// ---------------------------------------------------------------------------------------------
// Editing a .bib file
//
// The manager rewrites entries in place. It leaves everything else in the file exactly as it was:
// comments, @string macros, and the author's own formatting of other entries.

use std::sync::LazyLock;

use regex::Regex;

static ENTRY_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*@([a-zA-Z]+)[ \t]*\{[ \t]*([^,\s]+)[ \t]*,").unwrap());

/// The fields a manager writes, in the order a reader expects them.
pub const FIELD_ORDER: &[&str] =
    &["author", "title", "year", "journal", "booktitle", "publisher", "volume", "number", "pages", "doi", "eprint", "archivePrefix", "url", "note"];

/// An entry to write: its type, key and fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: String,
    pub key: String,
    pub fields: Vec<(String, String)>,
}

impl Entry {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(f, _)| f.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// BibTeX, in field order, with the two-space indentation the bundled template uses.
    pub fn to_bibtex(&self) -> String {
        let mut ordered: Vec<&(String, String)> = Vec::new();
        for want in FIELD_ORDER {
            if let Some(f) = self.fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(want)) {
                ordered.push(f);
            }
        }
        for f in &self.fields {
            if !ordered.iter().any(|(n, _)| n.eq_ignore_ascii_case(&f.0)) {
                ordered.push(f);
            }
        }
        let body: Vec<String> = ordered
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(n, v)| format!("  {n} = {{{}}}", v.trim()))
            .collect();
        format!("@{}{{{},\n{}\n}}\n", self.kind, self.key, body.join(",\n"))
    }
}

/// The byte range of one entry in a `.bib` file, including its trailing newline.
pub fn entry_span(text: &str, key: &str) -> Option<(usize, usize)> {
    let starts: Vec<(usize, String)> =
        ENTRY_START.captures_iter(text).map(|m| (m.get(0).unwrap().start(), m[2].to_string())).collect();
    let at = starts.iter().position(|(_, k)| k == key)?;
    let start = starts[at].0;
    let end = starts.get(at + 1).map(|(s, _)| *s).unwrap_or(text.len());
    Some((start, end))
}

/// Replace an entry, or append it when the key is new. Returns the file's new text.
pub fn upsert(text: &str, entry: &Entry) -> String {
    let rendered = entry.to_bibtex();
    match entry_span(text, &entry.key) {
        Some((start, end)) => {
            let tail = &text[end..];
            // Keep the blank line that separated this entry from the next.
            let separator = if tail.is_empty() || tail.starts_with('\n') { "" } else { "\n" };
            format!("{}{rendered}{separator}{tail}", &text[..start])
        }
        None => {
            let mut out = text.trim_end().to_string();
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&rendered);
            out
        }
    }
}

/// Remove an entry. Returns `None` when the key is not there.
pub fn remove(text: &str, key: &str) -> Option<String> {
    let (start, end) = entry_span(text, key)?;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(text[end..].trim_start_matches('\n'));
    Some(out)
}

/// Parse pasted BibTeX into the first entry it contains.
pub fn from_bibtex(src: &str) -> Option<Entry> {
    let m = ENTRY_START.captures(src)?;
    let start = m.get(0).unwrap().start();
    let kind = m[1].to_ascii_lowercase();
    let key = m[2].to_string();
    let end = ENTRY_START
        .captures_iter(&src[start + 1..])
        .next()
        .map(|n| start + 1 + n.get(0).unwrap().start())
        .unwrap_or(src.len());
    let body = &src[start..end];
    let mut fields = Vec::new();
    for name in FIELD_ORDER.iter().map(|f| f.to_string()).chain(["abstract".into(), "editor".into(), "series".into(), "month".into()]) {
        if let Some(v) = crate::parse::field_public(body, &name) {
            fields.push((name, v));
        }
    }
    Some(Entry { kind, key, fields })
}

/// A key that is unique in this file, derived from the author's surname and the year.
pub fn suggest_key(text: &str, author: Option<&str>, year: Option<&str>, title: Option<&str>) -> String {
    let surname = author
        .and_then(|a| a.split(" and ").next())
        .map(|a| {
            let first = a.split(',').next().unwrap_or(a);
            first.split_whitespace().last().unwrap_or(first).to_string()
        })
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // No author: the first real words of the title, skipping "a", "the" and the like.
            title.map(|t| t.split_whitespace().filter(|w| w.len() > 2).take(2).collect::<Vec<_>>().join(""))
        })
        .unwrap_or_else(|| "entry".into());
    let base: String = surname
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    let base = if base.is_empty() { "entry".to_string() } else { base };
    let stem = format!("{base}{}", year.unwrap_or(""));
    if entry_span(text, &stem).is_none() {
        return stem;
    }
    for suffix in 'a'..='z' {
        let candidate = format!("{stem}{suffix}");
        if entry_span(text, &candidate).is_none() {
            return candidate;
        }
    }
    format!("{stem}x")
}

#[cfg(test)]
mod edit_tests {
    use super::*;

    const FILE: &str = r#"% My references
@string{jcs = {Journal of Computer Science}}

@article{knuth84,
  author = {Donald Knuth},
  title = {Literate Programming},
  year = {1984}}

@misc{draft, title = {A Preprint}, year = {2025}, eprint = {2406.09246}}
"#;

    #[test]
    fn rewrites_one_entry_and_leaves_the_rest_alone() {
        let entry = Entry {
            kind: "article".into(),
            key: "knuth84".into(),
            fields: vec![
                ("title".into(), "Literate Programming".into()),
                ("author".into(), "Knuth, Donald E.".into()),
                ("year".into(), "1984".into()),
                ("journal".into(), "The Computer Journal".into()),
                ("doi".into(), "10.1093/comjnl/27.2.97".into()),
            ],
        };
        let out = upsert(FILE, &entry);
        assert!(out.contains("% My references"), "the file's own text survives");
        assert!(out.contains("@string{jcs"), "{out}");
        assert!(out.contains("@misc{draft"), "later entries survive: {out}");
        // Fields come out in reading order, author first.
        let block = &out[out.find("@article{knuth84").unwrap()..out.find("@misc{draft").unwrap()];
        let author_at = block.find("author").unwrap();
        assert!(author_at < block.find("title").unwrap() && block.find("doi").unwrap() > block.find("year").unwrap());
        assert!(block.contains("author = {Knuth, Donald E.}"), "{block}");
        assert_eq!(out.matches("@article{knuth84").count(), 1, "no duplicate left behind");
    }

    #[test]
    fn appends_a_new_entry_and_removes_an_old_one() {
        let entry = Entry {
            kind: "inproceedings".into(),
            key: "new2026".into(),
            fields: vec![("title".into(), "Something New".into()), ("year".into(), "2026".into())],
        };
        let out = upsert(FILE, &entry);
        assert!(out.trim_end().ends_with("}"), "{out}");
        assert!(out.contains("@inproceedings{new2026"), "{out}");
        assert_eq!(crate::parse::scan_bib_count(&out), 3, "the two entries plus the new one; @string is not an entry");

        let gone = remove(&out, "draft").unwrap();
        assert!(!gone.contains("@misc{draft"));
        assert!(gone.contains("@article{knuth84") && gone.contains("@inproceedings{new2026"));
        assert!(remove(&gone, "nosuchkey").is_none());
    }

    #[test]
    fn parses_pasted_bibtex_and_suggests_a_free_key() {
        let pasted = "@InProceedings{smith2020tricks, author = {Smith, Jane and Doe, John},\n title = {Tricks of the Trade}, year = {2020}, booktitle = {NeurIPS}}";
        let e = from_bibtex(pasted).unwrap();
        assert_eq!((e.kind.as_str(), e.key.as_str()), ("inproceedings", "smith2020tricks"));
        assert_eq!(e.field("author"), Some("Smith, Jane and Doe, John"));
        assert_eq!(e.field("booktitle"), Some("NeurIPS"));
        assert!(from_bibtex("not bibtex at all").is_none());

        assert_eq!(suggest_key(FILE, Some("Smith, Jane and Doe, John"), Some("2020"), None), "smith2020");
        assert_eq!(suggest_key(FILE, Some("Donald Knuth"), Some("84"), None), "knuth84a", "keys stay unique");
        assert_eq!(suggest_key("", None, Some("2021"), Some("A Grand Title")), "grandtitle2021");
    }
}

static FIELD_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)([a-z][a-z0-9_-]*)\s*=\s*").unwrap());
static CITE_CMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\([cC]ite[a-zA-Z]*|parencite|textcite|autocite|footcite|nocite)(\*?(?:\[[^\]]*\])*)\{([^}]*)\}").unwrap()
});

/// Every field of one entry, in the order the file has them. Unknown fields are kept so that
/// editing an entry never silently drops what the author put there.
pub fn fields_of(entry_text: &str) -> Vec<(String, String)> {
    // Start after the key, so `@article{smith2020,` is not read as a field.
    let body_at = entry_text.find(',').map(|i| i + 1).unwrap_or(0);
    let body = &entry_text[body_at..];
    let mut out: Vec<(String, String)> = Vec::new();
    let mut at = 0usize;
    while let Some(m) = FIELD_START.captures_at(body, at) {
        let whole = m.get(0).unwrap();
        let name = m[1].to_string();
        let rest = &body[whole.end()..];
        let (value, consumed) = match rest.chars().next() {
            Some('{') => match matching_brace_at(rest) {
                Some(end) => (rest[1..end].to_string(), end + 1),
                None => break,
            },
            Some('"') => match rest[1..].find('"') {
                Some(end) => (rest[1..end + 1].to_string(), end + 2),
                None => break,
            },
            _ => {
                let end = rest.find([',', '\n', '}']).unwrap_or(rest.len());
                (rest[..end].to_string(), end)
            }
        };
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !value.is_empty() && !out.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            out.push((name, value));
        }
        at = whole.end() + consumed;
        if at >= body.len() {
            break;
        }
    }
    out
}

fn matching_brace_at(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Point every `\cite` of `from` at `to`. Returns the new text and how many keys moved.
pub fn rename_cite(text: &str, from: &str, to: &str) -> (String, usize) {
    let mut moved = 0;
    let out = CITE_CMD.replace_all(text, |c: &regex::Captures| {
        let keys: Vec<String> = c[3]
            .split(',')
            .map(|k| {
                let trimmed = k.trim();
                if trimmed == from {
                    moved += 1;
                    to.to_string()
                } else {
                    trimmed.to_string()
                }
            })
            .collect();
        // Drop a duplicate left behind by \cite{a,b} when a becomes b.
        let mut seen: Vec<String> = Vec::new();
        for k in keys {
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        format!("\\{}{}{{{}}}", &c[1], &c[2], seen.join(","))
    });
    (out.into_owned(), moved)
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn reads_every_field_including_ones_we_do_not_know() {
        let entry = r#"@article{smith2020,
  author = {Smith, Jane},
  title = {A Title, with a Comma},
  year = 2020,
  note = "quoted value",
  keywords = {one, two},
  abstract = {Nested {braces} survive}}"#;
        let f = fields_of(entry);
        let get = |n: &str| f.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        assert_eq!(get("author"), Some("Smith, Jane"));
        assert_eq!(get("title"), Some("A Title, with a Comma"));
        assert_eq!(get("year"), Some("2020"), "a bare value");
        assert_eq!(get("note"), Some("quoted value"));
        assert_eq!(get("keywords"), Some("one, two"));
        assert_eq!(get("abstract"), Some("Nested {braces} survive"));
        assert!(!f.iter().any(|(k, _)| k == "smith2020"), "the key is not a field");
    }

    #[test]
    fn merging_a_duplicate_moves_every_citation() {
        let tex = r#"We cite \cite{openvla2024} and \citep[see][p. 3]{openvla2024}.
Together: \cite{octo,openvla2024} and \cite{openvla,openvla2024} already both.
Unrelated: \cite{bert}."#;
        let (out, moved) = rename_cite(tex, "openvla2024", "openvla");
        assert_eq!(moved, 4);
        assert!(!out.contains("openvla2024"), "{out}");
        assert!(out.contains("\\citep[see][p. 3]{openvla}"), "options are preserved: {out}");
        assert!(out.contains("\\cite{octo,openvla}"), "{out}");
        assert!(out.contains("\\cite{openvla}") && !out.contains("openvla,openvla"), "no duplicate key: {out}");
        assert!(out.contains("\\cite{bert}"));
        assert_eq!(rename_cite(tex, "nothere", "x").1, 0);
    }
}
