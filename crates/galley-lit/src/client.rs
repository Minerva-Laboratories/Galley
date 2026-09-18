//! A small, polite HTTP client with an on-disk cache.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is unavailable right now: {1}")]
    Unavailable(&'static str, String),
    #[error("the literature service returned something unexpected: {0}")]
    Shape(String),
    /// The catalogue does not have this record. This is a normal answer, not an error.
    #[error("{0} has no record of it")]
    NotFound(&'static str),
}

/// Answers older than this are fetched again. Metadata changes slowly.
const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A cap per run, so a large bibliography cannot overload a free service.
pub const MAX_REQUESTS: usize = 80;

pub struct Client {
    http: reqwest::Client,
    cache_dir: PathBuf,
    contact: Option<String>,
    /// Atomic rather than a Cell, because the client is shared across await points on the server.
    spent: AtomicUsize,
}

impl Client {
    /// `cache_dir` is usually `<project>/.galley/lit`. `contact` is an email for the polite pools.
    pub fn new(cache_dir: &Path, contact: Option<String>) -> Client {
        let agent = match &contact {
            Some(c) => format!("{} (mailto:{c})", crate::USER_AGENT_BASE),
            None => crate::USER_AGENT_BASE.to_string(),
        };
        let http = reqwest::Client::builder()
            .user_agent(agent)
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Client { http, cache_dir: cache_dir.to_path_buf(), contact, spent: AtomicUsize::new(0) }
    }

    pub fn requests_made(&self) -> usize {
        self.spent.load(Ordering::Relaxed)
    }

    /// GET a document as text, cached like the rest. arXiv answers in Atom.
    pub async fn text(&self, what: &'static str, url: &str) -> Result<String, Error> {
        let v = self.fetch(what, url, false).await?;
        v.as_str().map(str::to_string).ok_or_else(|| Error::Shape("expected text".into()))
    }

    /// GET a JSON document, from the cache when it is fresh. `what` names the service in errors.
    pub async fn json(&self, what: &'static str, url: &str) -> Result<Value, Error> {
        self.fetch(what, url, true).await
    }

    async fn fetch(&self, what: &'static str, url: &str, as_json: bool) -> Result<Value, Error> {
        let path = self.cache_dir.join(format!("{}.json", hash(url)));
        if let Ok(meta) = std::fs::metadata(&path) {
            let fresh = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age < TTL);
            if fresh {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(v) = serde_json::from_slice(&bytes) {
                        return Ok(v);
                    }
                }
            }
        }
        if self.spent.load(Ordering::Relaxed) >= MAX_REQUESTS {
            return Err(Error::Unavailable(what, format!("stopped after {MAX_REQUESTS} requests in one run")));
        }
        self.spent.fetch_add(1, Ordering::Relaxed);
        let url = match &self.contact {
            Some(c) if url.contains("openalex.org") => {
                format!("{url}{}mailto={c}", if url.contains('?') { "&" } else { "?" })
            }
            _ => url.to_string(),
        };
        let res = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Unavailable(what, e.without_url().to_string()))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::NotFound(what));
        }
        if !res.status().is_success() {
            return Err(Error::Unavailable(what, format!("HTTP {}", res.status().as_u16())));
        }
        let value: Value = if as_json {
            res.json().await.map_err(|e| Error::Shape(e.to_string()))?
        } else {
            Value::String(res.text().await.map_err(|e| Error::Shape(e.to_string()))?)
        };
        let _ = std::fs::create_dir_all(&self.cache_dir);
        if let Ok(bytes) = serde_json::to_vec(&value) {
            let _ = std::fs::write(&path, bytes);
        }
        Ok(value)
    }
}

/// FNV-1a over the URL. It gives a stable cache file name with no dependency.
fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Convert `10.1093/comjnl/27.2.97` into a Crossref work URL.
pub fn crossref_work(doi: &str) -> String {
    format!("https://api.crossref.org/works/{}", urlencode(doi))
}

/// A bibliographic search, for finding the published version of a preprint.
pub fn crossref_search(title: &str) -> String {
    format!(
        "https://api.crossref.org/works?query.bibliographic={}&rows=5&select=DOI,title,type,container-title,issued",
        urlencode(title)
    )
}

pub fn openalex_work(doi: &str) -> String {
    format!("https://api.openalex.org/works/doi:{}", urlencode(doi))
}

pub fn openalex_works(ids: &[String]) -> String {
    format!("https://api.openalex.org/works?filter=openalex_id:{}&per-page=50", ids.join("|"))
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_escaped_and_stable() {
        assert_eq!(crossref_work("10.1093/comjnl/27.2.97"), "https://api.crossref.org/works/10.1093/comjnl/27.2.97");
        assert!(crossref_search("A Title: with punctuation & spaces").contains("A%20Title:%20with%20punctuation%20%26%20spaces"));
        assert_eq!(hash("x"), hash("x"));
        assert_ne!(hash("x"), hash("y"));
    }

    #[tokio::test]
    async fn a_fresh_cache_entry_is_used_without_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let c = Client::new(dir.path(), None);
        // Pre-seed the cache for a URL that would otherwise be fetched.
        let url = "https://api.openalex.org/works/doi:10.1/x";
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join(format!("{}.json", hash(url))), br#"{"title":"Cached"}"#).unwrap();
        let v = c.json("OpenAlex", url).await.unwrap();
        assert_eq!(v["title"], "Cached");
        assert_eq!(c.requests_made(), 0, "the cache must not spend a request");
    }
}
