//! Turn a pasted identifier into the fields of a bibliography entry. The identifier is a DOI, an
//! arXiv id, or the URL of either. This is the one catalogue call a project makes without opting
//! in. It happens because someone typed an identifier and pressed Add.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::client::{crossref_work, Client};
use crate::Error;

/// What a catalogue knows about a work, ready to become a BibTeX entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub kind: String,
    pub fields: Vec<(String, String)>,
}

impl Lookup {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(f, _)| f == name).map(|(_, v)| v.as_str())
    }
}

static ARXIV_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:arxiv[:/]|abs/|pdf/)?(\d{4}\.\d{4,5}(?:v\d+)?|[a-z-]+(?:\.[A-Z]{2})?/\d{7})").unwrap());
static DOI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(10\.\d{4,9}/[^\s"<>]+)"#).unwrap());

/// Which catalogue an identifier belongs to.
pub enum Id {
    Doi(String),
    Arxiv(String),
}

/// Read a pasted identifier. The DOI is used when both appear, because it names the published version.
pub fn parse_id(input: &str) -> Option<Id> {
    let text = input.trim();
    if let Some(m) = DOI.captures(text) {
        return Some(Id::Doi(m[1].trim_end_matches(['.', ',', ')']).to_lowercase()));
    }
    if text.to_lowercase().contains("arxiv") || ARXIV_ID.is_match(text) {
        if let Some(m) = ARXIV_ID.captures(text) {
            return Some(Id::Arxiv(m[1].to_string()));
        }
    }
    None
}

/// Fetch an entry's fields for a pasted identifier.
pub async fn lookup(client: &Client, input: &str) -> Result<Lookup, Error> {
    match parse_id(input) {
        Some(Id::Doi(doi)) => {
            let v = client.json("Crossref", &crossref_work(&doi)).await?;
            from_crossref_work(&v["message"]).ok_or(Error::NotFound("Crossref"))
        }
        Some(Id::Arxiv(id)) => {
            let url = format!("https://export.arxiv.org/api/query?id_list={id}&max_results=1");
            let xml = client.text("arXiv", &url).await?;
            from_arxiv(&xml, &id).ok_or(Error::NotFound("arXiv"))
        }
        None => Err(Error::Shape("that is not a DOI or an arXiv id".into())),
    }
}

/// Convert a Crossref record into BibTeX fields.
pub fn from_crossref_work(message: &Value) -> Option<Lookup> {
    let title = message["title"].get(0).and_then(Value::as_str)?.to_string();
    let kind = match message["type"].as_str().unwrap_or("") {
        "journal-article" => "article",
        "proceedings-article" => "inproceedings",
        "book" | "monograph" => "book",
        "book-chapter" => "incollection",
        "posted-content" => "misc",
        "dissertation" => "phdthesis",
        _ => "misc",
    };
    let authors: Vec<String> = message["author"]
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
    let year = message["issued"]["date-parts"]
        .get(0)
        .and_then(|p| p.get(0))
        .and_then(Value::as_i64)
        .map(|y| y.to_string());
    let venue = message["container-title"].get(0).and_then(Value::as_str).map(str::to_string);

    let mut fields = vec![("title".to_string(), title)];
    if !authors.is_empty() {
        fields.push(("author".into(), authors.join(" and ")));
    }
    if let Some(y) = year {
        fields.push(("year".into(), y));
    }
    if let Some(v) = venue {
        fields.push((if kind == "inproceedings" || kind == "incollection" { "booktitle" } else { "journal" }.into(), v));
    }
    if let Some(p) = message["publisher"].as_str().filter(|_| kind == "book") {
        fields.push(("publisher".into(), p.to_string()));
    }
    if let Some(d) = message["DOI"].as_str() {
        fields.push(("doi".into(), d.to_lowercase()));
    }
    if let Some(p) = message["page"].as_str() {
        fields.push(("pages".into(), p.to_string()));
    }
    Some(Lookup { kind: kind.to_string(), fields })
}

/// Convert an arXiv Atom feed into BibTeX fields. The published version comes too when arXiv has one.
pub fn from_arxiv(xml: &str, id: &str) -> Option<Lookup> {
    let flat: String = xml.split_whitespace().collect::<Vec<_>>().join(" ");
    let tag = |name: &str| -> Option<String> {
        let re = Regex::new(&format!(r"(?s)<{0}[^>]*>(.*?)</{0}>", regex::escape(name))).ok()?;
        // The feed's own <title> comes first. An entry's tags come after <entry>.
        let entry_at = flat.find("<entry>").unwrap_or(0);
        re.captures(&flat[entry_at..]).map(|c| c[1].split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let title = tag("title")?;
    let authors: Vec<String> = Regex::new(r"<name>(.*?)</name>")
        .ok()?
        .captures_iter(&flat)
        .map(|c| c[1].split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let year = tag("published").and_then(|p| p.get(..4).map(str::to_string));
    let doi = tag("arxiv:doi");
    let venue = tag("arxiv:journal_ref");

    let mut fields = vec![("title".to_string(), title)];
    if !authors.is_empty() {
        fields.push(("author".into(), authors.join(" and ")));
    }
    if let Some(y) = year {
        fields.push(("year".into(), y));
    }
    if let Some(v) = venue.clone() {
        fields.push(("journal".into(), v));
    }
    if let Some(d) = doi.clone() {
        fields.push(("doi".into(), d.to_lowercase()));
    }
    fields.push(("eprint".into(), id.to_string()));
    fields.push(("archivePrefix".into(), "arXiv".into()));
    Some(Lookup { kind: if venue.is_some() || doi.is_some() { "article".into() } else { "misc".into() }, fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_identifiers_people_paste() {
        assert!(matches!(parse_id("10.1093/comjnl/27.2.97"), Some(Id::Doi(d)) if d == "10.1093/comjnl/27.2.97"));
        assert!(matches!(parse_id("https://doi.org/10.1093/COMJNL/27.2.97"), Some(Id::Doi(d)) if d == "10.1093/comjnl/27.2.97"));
        assert!(matches!(parse_id("arXiv:2406.09246"), Some(Id::Arxiv(a)) if a == "2406.09246"));
        assert!(matches!(parse_id("https://arxiv.org/abs/2406.09246v2"), Some(Id::Arxiv(a)) if a == "2406.09246v2"));
        assert!(matches!(parse_id("2005.14165"), Some(Id::Arxiv(a)) if a == "2005.14165"));
        // The DOI is used because it names the published version.
        assert!(matches!(parse_id("arXiv:2406.09246 doi:10.15607/RSS.2024.XX.016"), Some(Id::Doi(_))));
        assert!(parse_id("just some words").is_none());
    }

    #[test]
    fn crossref_record_becomes_an_entry() {
        let message: Value = serde_json::from_str(
            r#"{"title":["Attention Is All You Need"],"type":"proceedings-article",
                "author":[{"family":"Vaswani","given":"Ashish"},{"name":"Others"}],
                "issued":{"date-parts":[[2017,12]]},"container-title":["NeurIPS"],
                "DOI":"10.5555/ABC","page":"5998-6008"}"#,
        )
        .unwrap();
        let l = from_crossref_work(&message).unwrap();
        assert_eq!(l.kind, "inproceedings");
        assert_eq!(l.field("author"), Some("Vaswani, Ashish and Others"));
        assert_eq!(l.field("booktitle"), Some("NeurIPS"), "a conference gets booktitle, not journal");
        assert_eq!(l.field("year"), Some("2017"));
        assert_eq!(l.field("doi"), Some("10.5555/abc"));
        assert_eq!(l.field("pages"), Some("5998-6008"));
    }

    #[test]
    fn arxiv_feed_becomes_an_entry_and_notices_publication() {
        let xml = r#"<feed><title>ArXiv Query</title><entry>
          <published>2024-06-13T17:54:22Z</published>
          <title>OpenVLA: An Open-Source
            Vision-Language-Action Model</title>
          <author><name>Moo Jin Kim</name></author><author><name>Karl Pertsch</name></author>
          </entry></feed>"#;
        let l = from_arxiv(xml, "2406.09246").unwrap();
        assert_eq!(l.kind, "misc", "no venue yet: a preprint");
        assert_eq!(l.field("title"), Some("OpenVLA: An Open-Source Vision-Language-Action Model"), "{:?}", l.fields);
        assert_eq!(l.field("author"), Some("Moo Jin Kim and Karl Pertsch"));
        assert_eq!(l.field("year"), Some("2024"));
        assert_eq!(l.field("eprint"), Some("2406.09246"));
        assert_eq!(l.field("archivePrefix"), Some("arXiv"));

        let published = xml.replace("</entry>", "<arxiv:journal_ref>RSS 2024</arxiv:journal_ref><arxiv:doi>10.15607/RSS.2024.XX.016</arxiv:doi></entry>");
        let l = from_arxiv(&published, "2406.09246").unwrap();
        assert_eq!(l.kind, "article");
        assert_eq!(l.field("journal"), Some("RSS 2024"));
        assert_eq!(l.field("doi"), Some("10.15607/rss.2024.xx.016"));
    }
}
