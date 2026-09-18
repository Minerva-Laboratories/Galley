//! The submission packer (SPEC §13.8). It flattens `\input` and `\include`, strips the comments,
//! keeps only the figures that are referenced and the bib entries that are cited, takes the `.bbl`
//! from a from-scratch compile of the staged tree, writes `00README.XXX` and makes a tar file. It
//! makes the tar file only when that clean build succeeds.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::hints::ProjectView;
use crate::log::{Diagnostic, Level};
use crate::runner::{needs_fetch, parse_pages, Builder};
use crate::Result;

pub const STAGE: &str = ".galley/pack/stage";
pub const ARCHIVE: &str = ".galley/pack/package.tar.gz";

#[derive(Debug, Clone, Serialize)]
pub struct PackItem {
    pub path: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackReport {
    pub included: Vec<PackItem>,
    pub left_out: Vec<PackItem>,
    pub build_ok: bool,
    pub errors: Vec<Diagnostic>,
    pub pages: Option<u32>,
    /// True when the archive exists and can be downloaded.
    pub archive: bool,
}

static INPUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\(?:input|include)\{([^}]+)\}").unwrap());
static GRAPHIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\includegraphics\*?(?:\[[^\]]*\])?\{([^}]+)\}").unwrap());
static INCLUDESVG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\includesvg\*?(?:\[[^\]]*\])?\{([^}]+)\}").unwrap());
static GRAPHICSPATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\graphicspath\{((?:\{[^}]*\})+)\}").unwrap());
static CITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:[cC]ite[a-zA-Z]*|parencite|textcite|autocite|footcite|nocite)\*?(?:\[[^\]]*\])*\{([^}]+)\}").unwrap()
});
static BIBLIOGRAPHY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\(?:bibliography|addbibresource)\{([^}]+)\}").unwrap());
static BIB_ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^@([a-zA-Z]+)\s*\{\s*([^,\s]+)\s*,").unwrap());
const GRAPHIC_EXTS: &[&str] = &["pdf", "png", "jpg", "jpeg", "eps"];

/// Inline `\input` and `\include` recursively, relative to the project. Records what was inlined.
pub fn flatten(project_dir: &Path, main_file: &str) -> (String, Vec<String>) {
    let mut inlined = Vec::new();
    let mut seen = BTreeSet::new();
    let text = std::fs::read_to_string(project_dir.join(main_file)).unwrap_or_default();
    seen.insert(main_file.to_string());
    let out = inline(project_dir, &text, &mut inlined, &mut seen, 0);
    (out, inlined)
}

fn inline(root: &Path, text: &str, inlined: &mut Vec<String>, seen: &mut BTreeSet<String>, depth: usize) -> String {
    if depth > 20 {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let code = strip_comment(line);
        let Some(m) = INPUT.captures(code) else {
            out.push_str(line);
            continue;
        };
        let raw = m[1].trim().trim_start_matches("./");
        let rel = if raw.ends_with(".tex") { raw.to_string() } else { format!("{raw}.tex") };
        if !seen.insert(rel.clone()) {
            out.push_str(line);
            continue;
        }
        match std::fs::read_to_string(root.join(&rel)) {
            Ok(sub) => {
                inlined.push(rel.clone());
                let before = &line[..m.get(0).unwrap().start()];
                let after = &line[m.get(0).unwrap().end()..];
                out.push_str(before);
                if !before.trim().is_empty() {
                    out.push('\n');
                }
                let body = inline(root, &sub, inlined, seen, depth + 1);
                out.push_str(&body);
                if !body.ends_with('\n') {
                    out.push('\n');
                }
                // The inlined body already ends the line. Keep the remainder only if it has content.
                if !after.trim().is_empty() {
                    out.push_str(after.trim_start_matches(' '));
                }
            }
            Err(_) => out.push_str(line),
        }
    }
    out
}

/// The line up to an unescaped `%`.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'%' => return &line[..i],
            _ => i += 1,
        }
    }
    line
}

/// Remove the comments. A line that was only a comment disappears. Other lines keep their text.
/// Returns the text and the number of comments removed, whole-line and trailing.
pub fn strip_comments(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let code = strip_comment(body);
        if code.len() == body.len() {
            out.push_str(line);
            continue;
        }
        removed += 1;
        if !code.trim().is_empty() {
            out.push_str(code.trim_end());
            out.push('\n');
        }
    }
    (out, removed)
}

/// Figure files the text references, resolved against `\graphicspath` and the usual extensions.
pub fn referenced_graphics(project_dir: &Path, text: &str) -> (Vec<String>, Vec<String>) {
    let mut dirs: Vec<String> = vec![String::new()];
    for m in GRAPHICSPATH.captures_iter(text) {
        for d in m[1].trim_matches(['{', '}']).split("}{") {
            dirs.push(d.trim().to_string());
        }
    }
    let (mut found, mut missing) = (Vec::new(), Vec::new());
    for m in GRAPHIC.captures_iter(text) {
        let name = m[1].trim().trim_start_matches("./");
        let mut hit = None;
        'outer: for d in &dirs {
            let base = format!("{d}{name}");
            let candidates = if Path::new(&base).extension().is_some() {
                vec![base.clone()]
            } else {
                GRAPHIC_EXTS.iter().map(|e| format!("{base}.{e}")).collect()
            };
            for c in candidates {
                if project_dir.join(&c).is_file() {
                    hit = Some(c);
                    break 'outer;
                }
            }
        }
        match hit {
            Some(p) => {
                if !found.contains(&p) {
                    found.push(p)
                }
            }
            None => {
                if !missing.contains(&name.to_string()) {
                    missing.push(name.to_string())
                }
            }
        }
    }
    (found, missing)
}

pub fn cited_keys(text: &str) -> (BTreeSet<String>, bool) {
    let mut keys = BTreeSet::new();
    let mut all = false;
    for m in CITE.captures_iter(text) {
        for k in m[1].split(',') {
            let k = k.trim();
            if k == "*" {
                all = true;
            } else if !k.is_empty() {
                keys.insert(k.to_string());
            }
        }
    }
    (keys, all)
}

/// The bib files the text names, as project-relative paths.
pub fn bib_files(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for m in BIBLIOGRAPHY.captures_iter(text) {
        for f in m[1].split(',') {
            let f = f.trim();
            if f.is_empty() {
                continue;
            }
            let p = if f.ends_with(".bib") { f.to_string() } else { format!("{f}.bib") };
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Keep only the entries with a cited key. Keep every entry when the text has `\nocite{*}`.
/// Returns the filtered text, the number of entries kept and the total.
pub fn filter_bib(text: &str, cited: &BTreeSet<String>, keep_all: bool) -> (String, usize, usize) {
    let starts: Vec<(usize, String)> = BIB_ENTRY.captures_iter(text).map(|m| (m.get(0).unwrap().start(), m[2].to_string())).collect();
    let total = starts.len();
    let mut out = String::new();
    let mut kept = 0;
    // Keep everything before the first entry as it is. This includes @preamble, @string and comments.
    out.push_str(&text[..starts.first().map(|s| s.0).unwrap_or(text.len())]);
    for (i, (start, key)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map(|s| s.0).unwrap_or(text.len());
        if keep_all || cited.contains(key) {
            out.push_str(&text[*start..end]);
            kept += 1;
        }
    }
    (out, kept, total)
}

impl Builder {
    /// Stage, verify with a clean compile, and archive. `venue` is used for the README and copy.
    pub async fn pack(
        &self,
        project_dir: &Path,
        main_file: &str,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<PackReport> {
        let stage = project_dir.join(STAGE);
        let _ = std::fs::remove_dir_all(&stage);
        std::fs::create_dir_all(&stage)?;
        let _ = std::fs::remove_file(project_dir.join(ARCHIVE));
        let mut included = Vec::new();
        let mut left_out = Vec::new();

        progress("Flattening the sources…".into());
        let (flat, inlined) = flatten(project_dir, main_file);
        let (text, comments) = strip_comments(&flat);
        let main_name = Path::new(main_file).file_name().and_then(|n| n.to_str()).unwrap_or("main.tex").to_string();
        std::fs::write(stage.join(&main_name), &text)?;
        included.push(PackItem {
            path: main_name.clone(),
            why: format!(
                "flattened · {} lines · {comments} comment{} stripped",
                text.lines().count(),
                if comments == 1 { "" } else { "s" }
            ),
        });
        for f in &inlined {
            left_out.push(PackItem { path: f.clone(), why: format!("inlined into {main_name}") });
        }

        let (figs, missing) = referenced_graphics(project_dir, &text);
        for f in &figs {
            let dest = stage.join(f);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(project_dir.join(f), &dest)?;
            included.push(PackItem { path: f.clone(), why: "referenced".into() });
        }
        for f in &missing {
            left_out.push(PackItem { path: f.clone(), why: "referenced but not found in the project".into() });
        }
        // The venue has no Inkscape, so ship the files converted from \includesvg. Put them where
        // the svg package looks when shell escape is off, next to the SVG that it still checks for.
        crate::svg::prepare(project_dir);
        let mut svgs_seen = BTreeSet::new();
        for m in INCLUDESVG.captures_iter(&text) {
            let raw = m[1].trim().trim_start_matches("./");
            let rel = if raw.to_ascii_lowercase().ends_with(".svg") { raw.to_string() } else { format!("{raw}.svg") };
            if !svgs_seen.insert(rel.clone()) {
                continue;
            }
            let stem = Path::new(&rel).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let converted = project_dir.join(crate::svg::ROOT).join("svg-inkscape");
            if !project_dir.join(&rel).is_file() || !converted.join(format!("{stem}_svg-tex.pdf")).is_file() {
                left_out.push(PackItem { path: rel, why: "an \\includesvg figure that is missing or could not be converted".into() });
                continue;
            }
            let dest = stage.join(&rel);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(project_dir.join(&rel), &dest)?;
            std::fs::create_dir_all(stage.join("svg-inkscape"))?;
            for suffix in ["_svg-tex.pdf", "_svg-tex.pdf_tex", "_svg-raw.pdf"] {
                let name = format!("{stem}{suffix}");
                std::fs::copy(converted.join(&name), stage.join("svg-inkscape").join(&name))?;
            }
            included.push(PackItem { path: rel, why: "SVG, with its PDF conversion in svg-inkscape/ (no Inkscape needed)".into() });
        }

        let (cited, keep_all) = cited_keys(&text);
        for b in bib_files(&text) {
            let Ok(src) = std::fs::read_to_string(project_dir.join(&b)) else {
                left_out.push(PackItem { path: b, why: "named in \\bibliography but not found".into() });
                continue;
            };
            let (filtered, kept, total) = filter_bib(&src, &cited, keep_all);
            std::fs::write(stage.join(&b), filtered)?;
            let dropped = total - kept;
            included.push(PackItem {
                path: b,
                why: if total == 0 {
                    "no entries".to_string()
                } else if dropped > 0 {
                    format!("{kept} of {total} entries · {dropped} uncited dropped")
                } else {
                    format!("{kept} entr{}", if kept == 1 { "y" } else { "ies" })
                },
            });
        }

        std::fs::write(stage.join("00README.XXX"), format!("{main_name} toplevelfile\n"))?;
        included.push(PackItem { path: "00README.XXX".into(), why: "arXiv build hints (main file)".into() });
        left_out.push(PackItem { path: ".galley/, .git/".into(), why: "never shipped".into() });

        progress("Compiling in a clean sandbox…".into());
        let engine = self.ensure_engine(progress).await?;
        let cold = crate::runner::cache_cold(&engine);
        let spec = engine.compile(&self.sandbox, &stage, &main_name, false, cold, self.timeout.as_secs())?;
        let mut run = self.run_spec(spec).await?;
        if needs_fetch(&run.raw, &stage) {
            let spec = engine.compile(&self.sandbox, &stage, &main_name, false, true, self.timeout.as_secs())?;
            run = self.run_spec(spec).await?;
        }
        let view = ProjectView { main_file: &main_name, main_text: &text };
        let errors: Vec<Diagnostic> = run.raw.into_iter().map(|d| self.hints.annotate(d, &view)).collect();
        let stage_out = stage.join(".galley").join("build");
        let pdf_ok = run.exit_ok && !run.timed_out && stage_out.join(format!("{}.pdf", run.stem)).is_file();
        let build_ok = pdf_ok && !errors.iter().any(|d| d.level == Level::Error);
        let pages = parse_pages(&run.log);

        // The bibliography as the venue sees it. Put the .bbl of the clean build next to the sources.
        let stem = main_name.trim_end_matches(".tex").to_string();
        let bbl = stage_out.join(format!("{stem}.bbl"));
        if bbl.is_file() {
            std::fs::copy(&bbl, stage.join(format!("{stem}.bbl")))?;
            included.push(PackItem { path: format!("{stem}.bbl"), why: format!("generated · {} cited entr{}", cited.len(), if cited.len() == 1 { "y" } else { "ies" }) });
        }

        let mut archive = false;
        if build_ok {
            progress("Writing the archive…".into());
            let out = project_dir.join(ARCHIVE);
            let stage2 = stage.clone();
            tokio::task::spawn_blocking(move || write_archive(&stage2, &out))
                .await
                .map_err(|e| crate::Error::Engine(format!("archive task failed: {e}")))??;
            archive = true;
        }
        Ok(PackReport { included, left_out, build_ok, errors, pages, archive })
    }
}

/// tar.gz of the staged tree without its own `.galley` build directory.
fn write_archive(stage: &Path, out: &Path) -> std::io::Result<()> {
    let file = std::fs::File::create(out)?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    fn walk(tar: &mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>, root: &Path, dir: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let rel: PathBuf = path.strip_prefix(root).unwrap().to_path_buf();
            if rel.starts_with(".galley") {
                continue;
            }
            if path.is_dir() {
                walk(tar, root, &path)?;
            } else {
                tar.append_path_with_name(&path, &rel)?;
            }
        }
        Ok(())
    }
    walk(&mut tar, stage, stage)?;
    tar.into_inner()?.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments_but_keeps_escaped_percent() {
        let (t, n) = strip_comments("a % note\n% whole line\nb \\% 5\\%\n");
        assert_eq!(t, "a\nb \\% 5\\%\n");
        assert_eq!(n, 2);
    }

    #[test]
    fn flattens_inputs_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), "A\n\\input{sec/intro}\nZ\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sec")).unwrap();
        std::fs::write(dir.path().join("sec/intro.tex"), "I1\n\\include{deep}\n").unwrap();
        std::fs::write(dir.path().join("deep.tex"), "D").unwrap();
        let (text, inlined) = flatten(dir.path(), "main.tex");
        assert_eq!(text, "A\nI1\nD\nZ\n");
        assert_eq!(inlined, vec!["sec/intro.tex", "deep.tex"]);
    }

    #[test]
    fn keeps_only_cited_entries_and_finds_graphics() {
        let bib = "@string{x=1}\n@article{a, t={1}}\n@book{b,\n t={2}}\n@misc{c, t={3}}\n";
        let cited: BTreeSet<String> = ["a", "c"].iter().map(|s| s.to_string()).collect();
        let (out, kept, total) = filter_bib(bib, &cited, false);
        assert_eq!((kept, total), (2, 3));
        assert!(out.starts_with("@string{x=1}\n@article{a") && out.contains("@misc{c") && !out.contains("@book"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("figs")).unwrap();
        std::fs::write(dir.path().join("figs/one.pdf"), b"x").unwrap();
        let (found, missing) = referenced_graphics(dir.path(), "\\graphicspath{{figs/}}\n\\includegraphics[width=1in]{one}\\includegraphics{two.png}");
        assert_eq!(found, vec!["figs/one.pdf"]);
        assert_eq!(missing, vec!["two.png"]);
        assert_eq!(bib_files("\\bibliography{refs,extra.bib}"), vec!["refs.bib", "extra.bib"]);
    }
}
