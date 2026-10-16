//! File names written inside a document, such as `\input{sections/intro}`, resolved against the
//! project. A document is user content, so a name in it is checked like a path from a client: it
//! must stay inside the project and must not pass through a symlink.

use std::path::{Component, Path, PathBuf};

/// Directories that a document must never address. These are the git internals and the Galley state.
const RESERVED_ROOTS: &[&str] = &[".git", ".galley"];

/// Normalise a project-relative name from a document, or reject it. It accepts `sections/2.tex`
/// and `./fig.pdf`. It rejects an absolute path, `..`, a backslash, a reserved root, a control
/// character and an empty name. The same rules as `galley_sync::paths::clean_rel_path`.
pub fn clean_name(input: &str) -> Option<String> {
    if input.is_empty() || input.len() > 512 {
        return None;
    }
    if input.contains('\\') || input.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for comp in Path::new(input).components() {
        match comp {
            Component::Normal(s) => {
                let s = s.to_str()?;
                if s.is_empty() || s == ".." {
                    return None;
                }
                parts.push(s);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() || RESERVED_ROOTS.iter().any(|r| r.eq_ignore_ascii_case(parts[0])) {
        return None;
    }
    Some(parts.join("/"))
}

/// The path of `name` inside `root`, or `None` when the name leaves `root` or any part of it that
/// exists is a symlink. The file itself need not exist; reading it then fails as usual.
pub fn inside(root: &Path, name: &str) -> Option<PathBuf> {
    let clean = clean_name(name)?;
    let mut path = root.to_path_buf();
    for part in clean.split('/') {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => return None,
            Ok(_) => {}
            // Nothing further down can exist, so nothing further down can be a symlink.
            Err(_) => break,
        }
    }
    Some(root.join(clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_the_project() {
        assert_eq!(clean_name("sections/2.tex").as_deref(), Some("sections/2.tex"));
        assert_eq!(clean_name("./fig.pdf").as_deref(), Some("fig.pdf"));
        assert_eq!(clean_name("a/./b.tex").as_deref(), Some("a/b.tex"));
        for bad in [
            "",
            "/etc/passwd",
            "../other/main.tex",
            "a/../../x",
            "..",
            ".git/config",
            ".galley/build/main.pdf",
            ".GALLEY/x",
            "a\\b.tex",
            "a\u{0}b",
        ] {
            assert_eq!(clean_name(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn inside_refuses_symlinks_on_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.tex"), "secret").unwrap();
        std::fs::create_dir_all(dir.path().join("sec")).unwrap();
        std::fs::write(dir.path().join("sec/intro.tex"), "intro").unwrap();
        assert_eq!(inside(dir.path(), "sec/intro.tex"), Some(dir.path().join("sec/intro.tex")));
        assert_eq!(inside(dir.path(), "sec/missing.tex"), Some(dir.path().join("sec/missing.tex")));
        assert_eq!(inside(dir.path(), "../x.tex"), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
            std::os::unix::fs::symlink(outside.path().join("secret.tex"), dir.path().join("file.tex")).unwrap();
            assert_eq!(inside(dir.path(), "link/secret.tex"), None);
            assert_eq!(inside(dir.path(), "file.tex"), None);
        }
    }
}
