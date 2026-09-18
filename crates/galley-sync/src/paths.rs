//! Checks on project-relative paths. Every path that arrives from a client goes through
//! [`clean_rel_path`] before it touches the filesystem. The project directory is the boundary.

use std::path::{Component, Path};

/// Directories that a client must never address. These are the git internals and the Galley state.
const RESERVED_ROOTS: &[&str] = &[".git", ".galley"];

/// The extensions that Galley syncs as CRDT text. Every other file is a binary asset. Galley
/// commits a binary asset without a change.
const TEXT_EXTENSIONS: &[&str] = &[
    "tex", "bib", "sty", "cls", "bst", "txt", "md", "csv", "tsv", "cfg", "tikz", "dtx", "ins",
    "clo", "def", "ltx", "toml", "yaml", "yml", "json", "bbx", "cbx", "lbx",
];

/// Normalise a relative path from a client, or reject it. It accepts `sections/2.tex`. It rejects
/// an absolute path, `..`, a backslash, a reserved root, a control character and an empty segment.
pub fn clean_rel_path(input: &str) -> Option<String> {
    if input.is_empty() || input.len() > 512 {
        return None;
    }
    if input.contains('\\') || input.chars().any(|c| c.is_control()) {
        return None;
    }
    let path = Path::new(input);
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(s) => {
                let s = s.to_str()?;
                if s.is_empty() || s == "." || s == ".." {
                    return None;
                }
                parts.push(s);
            }
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    if RESERVED_ROOTS.iter().any(|r| r.eq_ignore_ascii_case(parts[0])) {
        return None;
    }
    Some(parts.join("/"))
}

pub fn is_text_path(rel: &str) -> bool {
    match rel.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !stem.ends_with('/') => {
            TEXT_EXTENSIONS.iter().any(|t| t.eq_ignore_ascii_case(ext))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        assert_eq!(clean_rel_path("main.tex").as_deref(), Some("main.tex"));
        assert_eq!(
            clean_rel_path("sections/2.tex").as_deref(),
            Some("sections/2.tex")
        );
        assert_eq!(clean_rel_path("a/b/c.bib").as_deref(), Some("a/b/c.bib"));
        // A `.` segment and a doubled slash are removed by the normalisation. They are not errors.
        assert_eq!(clean_rel_path("a/./b.tex").as_deref(), Some("a/b.tex"));
        assert_eq!(clean_rel_path("a//b.tex").as_deref(), Some("a/b.tex"));
    }

    #[test]
    fn rejects_escapes_and_reserved() {
        for bad in [
            "",
            "/etc/passwd",
            "../x.tex",
            "a/../../x",
            ".git/config",
            ".galley/doc.ybin",
            ".GIT/HEAD",
            "a\\b.tex",
            "a\u{0}b",
            "..",
        ] {
            assert_eq!(clean_rel_path(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn text_detection() {
        assert!(is_text_path("main.tex"));
        assert!(is_text_path("refs.BIB"));
        assert!(!is_text_path("figures/fig1.pdf"));
        assert!(!is_text_path("Makefile"));
        assert!(!is_text_path(".tex"));
    }
}
