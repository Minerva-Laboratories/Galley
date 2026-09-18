//! Venue presets (SPEC §13.8), bundled from `templates/venues/*.toml`. A user copies one into
//! their own config to add a venue. The server needs only the name, the limits, and the anonymity.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Venue {
    pub id: String,
    pub name: String,
    pub short: String,
    #[serde(default)]
    pub pages: Option<u32>,
    #[serde(default)]
    pub font_size: Option<String>,
    #[serde(default)]
    pub anonymous: bool,
    pub engine: String,
}

const BUNDLED: &[&str] = &[
    include_str!("../../../templates/venues/arxiv.toml"),
    include_str!("../../../templates/venues/neurips.toml"),
    include_str!("../../../templates/venues/acl.toml"),
    include_str!("../../../templates/venues/tpami.toml"),
];

pub fn all() -> Vec<Venue> {
    BUNDLED.iter().filter_map(|t| toml::from_str(t).ok()).collect()
}

pub fn find(id: &str) -> Option<Venue> {
    all().into_iter().find(|v| v.id == id)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_presets_parse() {
        let v = super::all();
        assert_eq!(v.len(), 4);
        assert_eq!(super::find("neurips").unwrap().pages, Some(9));
        assert!(super::find("arxiv").unwrap().pages.is_none());
    }
}
