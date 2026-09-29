//! Import a project from a zip file, such as an Overleaf download. The import reads the archive in
//! memory, drops build output and system files, strips a single wrapping folder, converts text that
//! is not UTF-8, and picks the main file. The caller creates the project from the result, so the
//! person uploading sets nothing.

use std::io::{Cursor, Read};
use std::sync::LazyLock;

use galley_sync::paths::{clean_rel_path, is_text_path};
use regex::Regex;
use serde::Serialize;

/// The largest zip the server accepts.
pub const MAX_ZIP: usize = 60 * 1024 * 1024;
/// A zip that expands past this is refused, so a small archive cannot fill the disk.
const MAX_EXPANDED: u64 = 300 * 1024 * 1024;
const MAX_ENTRIES: usize = 5000;
/// The same per-file cap as an ordinary upload.
const MAX_FILE: u64 = crate::file_routes::MAX_UPLOAD as u64;

/// Build output and editor litter. None of it is source, and keeping it would put stale results
/// into history.
/// A `.bbl` is kept: a project may ship one with no `.bib`, and the build regenerates it anyway.
const SKIPPED_EXTENSIONS: &[&str] = &[
    "aux", "log", "out", "toc", "lof", "lot", "fls", "fdb_latexmk", "blg", "bcf", "nav", "snm", "vrb",
    "xdv", "dvi", "idx", "ilg", "ind", "glo", "gls", "glg", "ist", "loa",
];

pub struct Imported {
    pub files: Vec<(String, Vec<u8>)>,
    pub report: Report,
}

/// What the import did, shown to the person who uploaded the zip.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub main_file: String,
    /// How the main file was chosen, in one sentence.
    pub main_reason: String,
    /// Other files that also start a document, in case the choice was wrong.
    pub other_candidates: Vec<String>,
    pub file_count: usize,
    pub skipped: Vec<String>,
    pub converted: Vec<String>,
    pub warnings: Vec<String>,
    /// A name taken from `\title{}`, when the upload did not give one.
    pub title: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("That file is not a zip archive. In Overleaf, use Menu, then Download, then Source.")]
    NotZip,
    #[error("The zip is larger than {} MB once expanded. Remove large data files and try again.", MAX_EXPANDED / 1024 / 1024)]
    TooLarge,
    #[error("The zip holds more than {MAX_ENTRIES} files. Remove generated files and try again.")]
    TooMany,
    #[error("The zip has no .tex file with \\documentclass, so there is nothing to compile.")]
    NoMain,
    #[error("The zip is damaged: {0}")]
    Damaged(String),
}

static DOCUMENTCLASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\documentclass\s*[\[{]").unwrap());
static BEGIN_DOCUMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\begin\s*\{document\}").unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\title\s*(?:\[[^\]]*\])?\s*\{([^{}]{1,120})\}").unwrap());

/// Read a zip into project files and a report.
pub fn read_zip(bytes: &[u8]) -> Result<Imported, ImportError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| ImportError::NotZip)?;
    if archive.len() > MAX_ENTRIES {
        return Err(ImportError::TooMany);
    }

    let mut raw: Vec<(String, Vec<u8>)> = Vec::new();
    let mut skipped = Vec::new();
    let mut expanded: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| ImportError::Damaged(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        if is_litter(&name) {
            continue;
        }
        if entry.size() > MAX_FILE {
            skipped.push(format!("{name} (larger than {} MB)", MAX_FILE / 1024 / 1024));
            continue;
        }
        expanded += entry.size();
        if expanded > MAX_EXPANDED {
            return Err(ImportError::TooLarge);
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        // Read one byte past the declared size, so an entry that lies about its size is caught.
        (&mut entry).take(MAX_FILE + 1).read_to_end(&mut data).map_err(|e| ImportError::Damaged(e.to_string()))?;
        if data.len() as u64 > MAX_FILE {
            skipped.push(format!("{name} (larger than {} MB)", MAX_FILE / 1024 / 1024));
            continue;
        }
        raw.push((name, data));
    }

    let prefix = common_folder(raw.iter().map(|(n, _)| n.as_str()));
    let mut files = Vec::new();
    let mut converted = Vec::new();
    for (name, data) in raw {
        let stripped = name.strip_prefix(&prefix).unwrap_or(&name).to_string();
        if is_build_output(&stripped) {
            skipped.push(stripped);
            continue;
        }
        let Some(rel) = clean_rel_path(&stripped) else {
            skipped.push(stripped);
            continue;
        };
        let data = if is_text_path(&rel) {
            match to_utf8(data) {
                (text, false) => text,
                (text, true) => {
                    converted.push(rel.clone());
                    text
                }
            }
        } else {
            data
        };
        files.push((rel, data));
    }
    // A PDF with the same name as a .tex file beside it is that file's stale build. Any other PDF is a
    // figure and has to stay, or the first build fails.
    let tex_stems: std::collections::HashSet<String> = files
        .iter()
        .filter_map(|(p, _)| p.strip_suffix(".tex").map(str::to_string))
        .collect();
    files.retain(|(p, _)| match p.strip_suffix(".pdf") {
        Some(stem) if tex_stems.contains(stem) => {
            skipped.push(p.clone());
            false
        }
        _ => true,
    });

    let (main_file, main_reason, other_candidates) = pick_main(&files).ok_or(ImportError::NoMain)?;
    let main_text = files.iter().find(|(p, _)| *p == main_file).map(|(_, d)| String::from_utf8_lossy(d).into_owned());
    let title = main_text.as_deref().and_then(title_of);
    let warnings = warnings_for(&files);
    let file_count = files.len();
    Ok(Imported {
        files,
        report: Report { main_file, main_reason, other_candidates, file_count, skipped, converted, warnings, title },
    })
}

/// System files that no one means to import. They are dropped without a mention.
fn is_litter(name: &str) -> bool {
    name.starts_with("__MACOSX/")
        || name.contains("/__MACOSX/")
        || name.rsplit('/').next().is_some_and(|f| f == ".DS_Store" || f == "Thumbs.db" || f.starts_with("._"))
        || name.starts_with(".git/")
        || name.contains("/.git/")
}

/// Compiler output. The import lists these as skipped, since a person may wonder where they went.
fn is_build_output(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if lower.ends_with(".synctex.gz") || lower.ends_with(".run.xml") {
        return true;
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    SKIPPED_EXTENSIONS.contains(&ext)
}

/// The folder every file sits in, when there is exactly one. A zip made by compressing a folder
/// wraps everything in it, and the project should not start one level down.
fn common_folder<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut first: Option<&str> = None;
    for name in names {
        let Some((top, _)) = name.split_once('/') else { return String::new() };
        match first {
            None => first = Some(top),
            Some(f) if f == top => {}
            Some(_) => return String::new(),
        }
    }
    first.map(|f| format!("{f}/")).unwrap_or_default()
}

/// Text that is not valid UTF-8 was probably saved as Latin-1, which older LaTeX projects used
/// through `\usepackage[latin1]{inputenc}`. Each byte maps to the same code point in Latin-1. The
/// boolean is true when the text was converted.
fn to_utf8(data: Vec<u8>) -> (Vec<u8>, bool) {
    let data = match data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => rest.to_vec(),
        None => data,
    };
    if std::str::from_utf8(&data).is_ok() {
        return (data, false);
    }
    let text: String = data.iter().map(|&b| b as char).collect();
    (text.into_bytes(), true)
}

fn uncommented(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut out = String::new();
            let mut escaped = false;
            for c in line.chars() {
                if c == '%' && !escaped {
                    break;
                }
                escaped = c == '\\' && !escaped;
                out.push(c);
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Choose the file to compile. A candidate declares `\documentclass` outside a comment. Among
/// several, `main.tex` wins, then a file at the top level, then one that also begins the document,
/// then the shortest path.
fn pick_main(files: &[(String, Vec<u8>)]) -> Option<(String, String, Vec<String>)> {
    let mut candidates: Vec<(&str, bool)> = files
        .iter()
        .filter(|(p, _)| p.to_ascii_lowercase().ends_with(".tex"))
        .filter_map(|(p, d)| {
            let text = uncommented(&String::from_utf8_lossy(d));
            DOCUMENTCLASS.is_match(&text).then(|| (p.as_str(), BEGIN_DOCUMENT.is_match(&text)))
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by_key(|(p, begins)| {
        let file = p.rsplit('/').next().unwrap_or(p);
        (file != "main.tex", p.contains('/'), !*begins, p.len(), p.to_string())
    });
    let (chosen, begins) = candidates[0];
    let others: Vec<String> = candidates[1..].iter().map(|(p, _)| p.to_string()).collect();
    let reason = if others.is_empty() {
        format!("{chosen} is the only file with \\documentclass.")
    } else if chosen.rsplit('/').next() == Some("main.tex") {
        format!("{chosen} is called main.tex, and {} other file(s) also have \\documentclass.", others.len())
    } else if !chosen.contains('/') && begins {
        format!("{chosen} is at the top level and begins the document.")
    } else {
        format!("{chosen} was the best of {} files with \\documentclass.", others.len() + 1)
    };
    Some((chosen.to_string(), reason, others))
}

fn title_of(main: &str) -> Option<String> {
    let text = uncommented(main);
    let raw = TITLE.captures(&text)?.get(1)?.as_str();
    let clean: String = raw
        .replace("\\\\", " ")
        .replace(['~', '\n'], " ")
        .chars()
        .filter(|c| !matches!(c, '\\' | '$'))
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    (!clean.is_empty()).then_some(clean)
}

/// Things the import cannot fix and the author should know before the first build.
fn warnings_for(files: &[(String, Vec<u8>)]) -> Vec<String> {
    let all: String = files
        .iter()
        .filter(|(p, _)| p.ends_with(".tex") || p.ends_with(".sty") || p.ends_with(".cls"))
        .map(|(_, d)| uncommented(&String::from_utf8_lossy(d)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    if all.contains("{minted}") || all.contains("\\write18") || all.contains("{pythontex}") {
        out.push(
            "The project runs shell commands (minted, pythontex or \\write18). Galley compiles without shell escape, so \
             those parts will fail. For code listings, the listings package works."
                .to_string(),
        );
    }
    if all.contains("\\directlua") || all.contains("{luacode}") {
        out.push(
            "The project uses LuaTeX features. Galley compiles with a XeTeX-based engine, so \\directlua and luacode will \
             not run."
                .to_string(),
        );
    }
    out
}

/// Pack a project's files into a zip, the reverse of `read_zip`. It leaves out `.git` and
/// `.galley`, so the archive holds the project as an author sees it and imports cleanly into Galley
/// or Overleaf.
pub fn write_zip(workdir: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::Write;
    let mut files = Vec::new();
    collect(workdir, workdir, &mut files)?;
    files.sort();
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for rel in files {
            w.start_file(rel.as_str(), opts).map_err(std::io::Error::other)?;
            w.write_all(&std::fs::read(workdir.join(&rel))?)?;
        }
        w.finish().map_err(std::io::Error::other)?;
    }
    Ok(buf.into_inner())
}

fn collect(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel == ".git" || rel == ".galley" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, out)?;
        } else if kind.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
        buf.into_inner()
    }

    const DOC: &[u8] = b"\\documentclass{article}\n\\title{A Study of Things}\n\\begin{document}\nHi\n\\end{document}\n";

    #[test]
    fn a_flat_overleaf_zip_imports_with_its_main_file() {
        let z = zip_of(&[("main.tex", DOC), ("refs.bib", b"@article{a,title={T}}"), ("figures/plot.png", &[0x89, 0x50])]);
        let got = read_zip(&z).unwrap();
        assert_eq!(got.report.main_file, "main.tex");
        assert_eq!(got.report.file_count, 3);
        assert_eq!(got.report.title.as_deref(), Some("A Study of Things"));
        assert!(got.report.other_candidates.is_empty());
    }

    #[test]
    fn a_wrapping_folder_is_stripped() {
        let z = zip_of(&[("paper/thesis.tex", DOC), ("paper/chapters/intro.tex", b"Intro"), ("paper/refs.bib", b"@x")]);
        let got = read_zip(&z).unwrap();
        assert_eq!(got.report.main_file, "thesis.tex");
        assert!(got.files.iter().any(|(p, _)| p == "chapters/intro.tex"));
    }

    #[test]
    fn main_tex_wins_among_several_documents() {
        let z = zip_of(&[("response.tex", DOC), ("main.tex", DOC), ("supplement.tex", DOC)]);
        let got = read_zip(&z).unwrap();
        assert_eq!(got.report.main_file, "main.tex");
        assert_eq!(got.report.other_candidates.len(), 2);
    }

    #[test]
    fn a_commented_documentclass_does_not_count() {
        let z = zip_of(&[("notes.tex", b"% \\documentclass{article}\nText"), ("paper.tex", DOC)]);
        assert_eq!(read_zip(&z).unwrap().report.main_file, "paper.tex");
    }

    #[test]
    fn build_output_and_system_files_are_dropped() {
        let z = zip_of(&[
            ("main.tex", DOC),
            ("main.aux", b"x"),
            ("main.log", b"x"),
            ("main.pdf", b"%PDF"),
            ("main.synctex.gz", b"x"),
            ("figures/result.pdf", b"%PDF"),
            ("__MACOSX/._main.tex", b"x"),
            (".DS_Store", b"x"),
        ]);
        let got = read_zip(&z).unwrap();
        let paths: Vec<&str> = got.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["main.tex", "figures/result.pdf"]);
        assert!(got.report.skipped.iter().any(|s| s == "main.aux"));
        assert!(!got.report.skipped.iter().any(|s| s.contains("MACOSX")));
    }

    #[test]
    fn latin1_text_is_converted_and_reported() {
        let latin1 = b"\\documentclass{article}\\begin{document}Espa\xf1a\\end{document}";
        let got = read_zip(&zip_of(&[("main.tex", latin1)])).unwrap();
        let text = String::from_utf8(got.files[0].1.clone()).unwrap();
        assert!(text.contains("España"));
        assert_eq!(got.report.converted, vec!["main.tex"]);
    }

    #[test]
    fn unsafe_paths_are_skipped() {
        let z = zip_of(&[("main.tex", DOC), ("../evil.tex", b"x"), (".galley/meta.toml", b"x")]);
        let got = read_zip(&z).unwrap();
        assert_eq!(got.files.len(), 1);
    }

    #[test]
    fn no_document_is_an_error() {
        assert!(matches!(read_zip(&zip_of(&[("a.tex", b"just text")])), Err(ImportError::NoMain)));
        assert!(matches!(read_zip(b"not a zip"), Err(ImportError::NotZip)));
    }

    #[test]
    fn an_export_imports_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sections")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".galley/build")).unwrap();
        std::fs::write(dir.path().join("main.tex"), DOC).unwrap();
        std::fs::write(dir.path().join("sections/intro.tex"), b"Intro").unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), b"ref").unwrap();
        std::fs::write(dir.path().join(".galley/build/main.pdf"), b"%PDF").unwrap();
        let zip = write_zip(dir.path()).unwrap();
        let back = read_zip(&zip).unwrap();
        let paths: Vec<&str> = back.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["main.tex", "sections/intro.tex"]);
        assert_eq!(back.report.main_file, "main.tex");
    }

    #[test]
    fn shell_escape_is_warned_about() {
        let z = zip_of(&[("main.tex", b"\\documentclass{article}\\usepackage{minted}\\begin{document}\\end{document}")]);
        assert_eq!(read_zip(&z).unwrap().report.warnings.len(), 1);
    }
}
