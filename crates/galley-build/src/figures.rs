//! Persistent figure cache (SPEC §13.1). The `external` library of TikZ runs in `list and make`
//! mode, without shell escape, through a wrapper that loads it when tikz loads. Galley compiles
//! each picture on its own into `.galley/build/galley-fig-figure<N>.pdf`. The main pass includes
//! those PDFs.
//!
//! TikZ includes a figure PDF whenever it exists, even if the picture changed. Galley must
//! therefore find the stale figures itself. It compares the `.md5` of each figure, which TikZ
//! writes on every pass, with the md5 that the PDF was built from. The md5 does not cover the
//! preamble, the style files, the data files or the engine. These go into a project-wide key that
//! drops every figure when it changes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const WRAPPER_STEM: &str = "galley-fig";
const STATE: &str = "figcache.json";
const FIGURE_PREFIX: &str = "galley-fig-figure";
/// The eligibility answer for a project with nothing to cache. The UI does not show it.
pub const NO_PICTURES: &str = "no TikZ pictures";

/// What the cache remembers between builds, in `.galley/build/figcache.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FigCache {
    /// Project-wide key. A change to it drops every figure.
    pub key: String,
    /// A map from figure name to the md5 of the picture code that its PDF was built from.
    pub figures: BTreeMap<String, String>,
    /// A map from figure name to the md5 that failed to compile on its own. Galley tries it again
    /// only when the picture changes.
    pub failed: BTreeMap<String, String>,
    /// Last measured durations, used to decide between regenerating now and a plain compile.
    pub plain_ms: u64,
    pub wrapper_ms: u64,
    pub figure_ms: u64,
}

impl FigCache {
    pub fn load(out_dir: &Path) -> FigCache {
        std::fs::read(out_dir.join(STATE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, out_dir: &Path) {
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(out_dir.join(STATE), json);
        }
    }
}

/// Figure-cache statistics reported with a build.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FigureStats {
    /// `cached` (the main pass used figure PDFs), `plain` (a normal compile), or `off`.
    pub mode: String,
    pub total: usize,
    pub cached: usize,
    /// Figures still to be generated in the background.
    pub pending: usize,
    /// Why the cache is off or was not used, in words for the Problems drawer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl FigureStats {
    pub fn off(reason: &str) -> FigureStats {
        FigureStats { mode: "off".into(), reason: Some(reason.to_string()), ..Default::default() }
    }
}

static BEGIN_PICTURE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\begin\{tikzpicture\}").unwrap());
static PICTURE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\\begin\{tikzpicture\}(.*?)\\end\{tikzpicture\}").unwrap());
static INPUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\(input|include|InputIfFileExists)\b").unwrap());
static OWN_EXTERNAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\tikzexternalize|\\usetikzlibrary\{[^}]*\bexternal\b").unwrap());
static BEAMER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\documentclass(\[[^\]]*\])?\{beamer\}").unwrap());
static MD5: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\tikzexternallastkey\s*\{([0-9A-Fa-f]+)\}").unwrap());

/// Whether externalizing is safe for this project, or why not. `tex` is every `.tex` file.
pub fn eligibility(main_text: &str, tex: &[(String, String)]) -> Result<(), String> {
    if !tex.iter().any(|(_, t)| BEGIN_PICTURE.is_match(t)) {
        return Err(NO_PICTURES.into());
    }
    if BEAMER.is_match(main_text) {
        return Err("beamer overlays do not survive externalization".into());
    }
    for (path, text) in tex {
        let code = strip_comments(text);
        if OWN_EXTERNAL.is_match(&code) {
            return Err(format!("{path} configures TikZ externalization itself"));
        }
        if code.contains("remember picture") || code.contains("overlay") {
            return Err(format!("{path} uses remember picture or overlay, which need the page"));
        }
        for m in PICTURE.captures_iter(&code) {
            if INPUT.is_match(&m[1]) {
                return Err(format!("{path} reads a file inside a picture, which the cache cannot track"));
            }
        }
    }
    Ok(())
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut end = line.len();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'%' => {
                    end = i;
                    break;
                }
                _ => i += 1,
            }
        }
        out.push_str(&line[..end.min(line.len())]);
        out.push('\n');
    }
    out
}

/// The project-wide key. It covers the engine identity, the main preamble, and every file that a
/// picture can read and that is not a `.tex` body, such as a style file, a data file or an image.
/// The key hashes the text. A large file contributes its size and its modification time.
pub fn project_key(project_dir: &Path, main_text: &str, engine_id: &str) -> String {
    let mut h = Fnv::new();
    h.write(engine_id.as_bytes());
    let preamble = main_text.split("\\begin{document}").next().unwrap_or(main_text);
    h.write(preamble.as_bytes());
    let mut files = Vec::new();
    collect(project_dir, project_dir, &mut files);
    files.sort();
    for rel in files {
        let lower = rel.to_ascii_lowercase();
        if lower.ends_with(".tex") || lower.ends_with(".bib") {
            continue;
        }
        let full = project_dir.join(&rel);
        let Ok(meta) = std::fs::metadata(&full) else { continue };
        h.write(rel.as_bytes());
        if meta.len() <= 2 * 1024 * 1024 {
            if let Ok(bytes) = std::fs::read(&full) {
                h.write(&bytes);
                continue;
            }
        }
        h.write(&meta.len().to_le_bytes());
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        h.write(&mtime.to_le_bytes());
    }
    format!("{:016x}", h.0)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name();
        if name == ".git" || name == ".galley" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// FNV-1a. It is stable across Rust releases, unlike `DefaultHasher`, so the key survives an
/// upgrade.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // A separator so ("ab","c") and ("a","bc") differ.
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// The figures the last wrapper pass listed, in document order.
pub fn figure_list(out_dir: &Path) -> Vec<String> {
    std::fs::read_to_string(out_dir.join(format!("{WRAPPER_STEM}.figlist")))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(FIGURE_PREFIX))
        .map(str::to_string)
        .collect()
}

/// The md5 TikZ computed for each figure on the last pass.
pub fn current_md5(out_dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(out_dir.join(format!("{name}.md5"))).ok()?;
    MD5.captures(&text).map(|c| c[1].to_ascii_uppercase())
}

/// Figures whose PDF is missing, or was built from different picture code.
pub fn stale(out_dir: &Path, cache: &FigCache, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| {
            let pdf = out_dir.join(format!("{n}.pdf"));
            let now = current_md5(out_dir, n);
            !pdf.is_file() || now.is_none() || cache.figures.get(*n) != now.as_ref()
        })
        .cloned()
        .collect()
}

/// The wrapper for the main pass. It loads `external` directly after tikz, in `list and make` mode,
/// so no shell escape is needed. The md5 check makes TikZ write the key for each figure that Galley
/// compares.
pub fn wrapper_source(main_file: &str) -> String {
    let main_no_ext = main_file.strip_suffix(".tex").unwrap_or(main_file);
    format!(
        "\\AddToHook{{package/tikz/after}}{{%\n  \\usetikzlibrary{{external}}%\n  \\tikzexternalize[mode=list and make, up to date check=md5]%\n}}\n\\input{{{main_no_ext}}}\n"
    )
}

/// A figure job. TikZ sees its own name as the job and typesets only that picture.
pub fn figure_source() -> String {
    format!("\\def\\tikzexternalrealjob{{{WRAPPER_STEM}}}\\input{{{WRAPPER_STEM}}}\n")
}

/// Remove every cached figure and the state file.
pub fn clear(out_dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(out_dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(WRAPPER_STEM) || name == STATE {
                if name.starts_with(FIGURE_PREFIX) && name.ends_with(".pdf") {
                    n += 1;
                }
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect()
    }

    #[test]
    fn eligibility_rules() {
        let pic = "\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}";
        assert!(eligibility("\\documentclass{article}", &tex(&[("main.tex", pic)])).is_ok());
        assert!(eligibility("\\documentclass{article}", &tex(&[("main.tex", "no pictures")])).is_err());
        assert!(eligibility("\\documentclass[10pt]{beamer}", &tex(&[("main.tex", pic)])).is_err());
        let remember = "\\begin{tikzpicture}[remember picture]\\end{tikzpicture}";
        assert!(eligibility("", &tex(&[("a.tex", remember)])).unwrap_err().contains("remember picture"));
        let own = format!("\\usetikzlibrary{{calc,external}}\n{pic}");
        assert!(eligibility("", &tex(&[("a.tex", &own)])).is_err());
        let inside = "\\begin{tikzpicture}\\input{coords}\\end{tikzpicture}";
        assert!(eligibility("", &tex(&[("a.tex", inside)])).is_err());
        // Commented-out uses do not count.
        let commented = format!("% remember picture\n% \\tikzexternalize\n{pic}");
        assert!(eligibility("", &tex(&[("a.tex", &commented)])).is_ok());
    }

    #[test]
    fn key_tracks_preamble_and_data_but_not_body_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.csv"), "1,2\n").unwrap();
        std::fs::write(dir.path().join("intro.tex"), "body").unwrap();
        let main = "\\usepackage{pgfplots}\n\\begin{document}\nHello\n";
        let k1 = project_key(dir.path(), main, "tectonic-1");
        assert_eq!(k1, project_key(dir.path(), &main.replace("Hello", "Goodbye"), "tectonic-1"));
        std::fs::write(dir.path().join("intro.tex"), "changed body").unwrap();
        assert_eq!(k1, project_key(dir.path(), main, "tectonic-1"));
        assert_ne!(k1, project_key(dir.path(), &main.replace("pgfplots", "tikz"), "tectonic-1"));
        assert_ne!(k1, project_key(dir.path(), main, "tectonic-2"));
        std::fs::write(dir.path().join("data.csv"), "1,3\n").unwrap();
        assert_ne!(k1, project_key(dir.path(), main, "tectonic-1"));
    }

    #[test]
    fn stale_compares_md5_with_what_was_built() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        std::fs::write(out.join("galley-fig.figlist"), "galley-fig-figure0\ngalley-fig-figure1\n").unwrap();
        for (n, k) in [("galley-fig-figure0", "AAA1"), ("galley-fig-figure1", "BBB2")] {
            std::fs::write(out.join(format!("{n}.md5")), format!("\\def \\tikzexternallastkey {{{k}}}%\n")).unwrap();
        }
        std::fs::write(out.join("galley-fig-figure0.pdf"), b"%PDF").unwrap();
        std::fs::write(out.join("galley-fig-figure1.pdf"), b"%PDF").unwrap();
        let names = figure_list(out);
        assert_eq!(names, vec!["galley-fig-figure0", "galley-fig-figure1"]);
        let mut cache = FigCache::default();
        cache.figures.insert("galley-fig-figure0".into(), "AAA1".into());
        cache.figures.insert("galley-fig-figure1".into(), "OLD".into());
        assert_eq!(stale(out, &cache, &names), vec!["galley-fig-figure1"]);
        std::fs::remove_file(out.join("galley-fig-figure0.pdf")).unwrap();
        assert_eq!(stale(out, &cache, &names).len(), 2);
        assert_eq!(clear(out), 1);
        assert!(figure_list(out).is_empty());
    }
}
