//! A single line-oriented pass over the project's `.tex` files. Comments are stripped first, so a
//! commented-out figure never appears in the map.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

static SECTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\(part|chapter|section|subsection|subsubsection|paragraph)\*?\s*\{").unwrap());
static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\label\s*\{([^}]+)\}").unwrap());
static REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:ref|eqref|autoref|cref|Cref|pageref|nameref|vref)\*?(?:\[[^\]]*\])?\{([^}]+)\}").unwrap()
});
static CITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:[cC]ite[a-zA-Z]*|parencite|textcite|autocite|footcite|nocite)\*?(?:\[[^\]]*\])*\{([^}]+)\}").unwrap()
});
static BEGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\begin\{([a-zA-Z*]+)\}").unwrap());
static CAPTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\caption\s*(?:\[[^\]]*\])?\{").unwrap());
static INPUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\(?:input|include|subfile)\s*\{([^}]+)\}").unwrap());
static BIB_ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^[ \t]*@([a-zA-Z]+)[ \t]*\{[ \t]*([^,\s]+)[ \t]*,").unwrap());
/// Commands whose braced argument is not prose: labels, references, file names, headings.
static STRUCTURAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\\(?:label|ref|eqref|autoref|cref|Cref|pageref|nameref|vref|[cC]ite[a-zA-Z]*|parencite|textcite|autocite|",
        r"footcite|nocite|input|include|subfile|includegraphics|includesvg|usepackage|documentclass|bibliography|",
        r"addbibresource|bibliographystyle|part|chapter|section|subsection|subsubsection|paragraph|begin|end|",
        r"graphicspath|newcommand|renewcommand|definecolor|setlength|tikzsetnextfilename)",
        r"\*?(?:\[[^\]]*\])*(?:\{[^}]*\})*"
    ))
    .unwrap()
});
/// Any other command: the name goes, its argument stays (`\\emph{clear}` is a word).
static COMMAND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\[a-zA-Z@]+\*?(?:\[[^\]]*\])?").unwrap());

/// Document-level markup that often trails a paragraph and is never part of it.
static FURNITURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:begin|end)\{document\}|\\appendix\b|\\(?:bibliography|bibliographystyle|addbibresource)\{[^}]*\}|\\maketitle\b").unwrap()
});

static BIBLIOGRAPHY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\(?:bibliography|addbibresource)\s*\{([^}]+)\}").unwrap());

/// What a labelled object is, for the map's shorthand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Figure,
    Table,
    Equation,
    Theorem,
    Algorithm,
    Listing,
    Section,
    Other,
}

impl Kind {
    fn of_environment(env: &str) -> Option<Kind> {
        let base = env.trim_end_matches('*');
        Some(match base {
            "figure" | "wrapfigure" | "subfigure" => Kind::Figure,
            "table" | "tabular" | "longtable" | "sidewaystable" => Kind::Table,
            "equation" | "align" | "gather" | "multline" | "eqnarray" | "flalign" => Kind::Equation,
            "theorem" | "lemma" | "proposition" | "corollary" | "definition" | "remark" | "claim" | "conjecture" => {
                Kind::Theorem
            }
            "algorithm" | "algorithmic" => Kind::Algorithm,
            "lstlisting" | "verbatim" | "minted" => Kind::Listing,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Figure => "figure",
            Kind::Table => "table",
            Kind::Equation => "equation",
            Kind::Theorem => "theorem",
            Kind::Algorithm => "algorithm",
            Kind::Listing => "listing",
            Kind::Section => "section",
            Kind::Other => "object",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub file: String,
    pub line: u32,
    /// 1 for `\section`, 2 for `\subsection`, … `\part` and `\chapter` come out as 0.
    pub level: u8,
    pub title: String,
    /// `4.2`, computed over the reading order of the files.
    pub number: String,
    pub label: Option<String>,
    pub words: usize,
    pub refs: Vec<String>,
    pub cites: Vec<String>,
    /// Indices into `Paper::objects`.
    pub objects: Vec<usize>,
    pub in_appendix: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Object {
    pub kind: Kind,
    pub label: Option<String>,
    pub caption: Option<String>,
    pub file: String,
    pub line: u32,
    /// Index into `Paper::sections`, when the object sits inside one.
    pub section: Option<usize>,
}

/// A passage: one paragraph of the source, with the section it belongs to. Retrieval works on
/// these rather than on whole files, so a hit can say "§4.2 Method" instead of a line number.
#[derive(Debug, Clone, Serialize)]
pub struct Chunk {
    pub file: String,
    pub line: u32,
    pub section: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[derive(Default)]
pub struct BibEntry {
    pub key: String,
    pub kind: String,
    pub title: Option<String>,
    pub year: Option<String>,
    pub author: Option<String>,
    pub doi: Option<String>,
    /// `2406.09246` from an `eprint`, `archiveprefix` or an arXiv URL.
    pub arxiv: Option<String>,
    /// Journal, booktitle or publisher. Any field that says where this appeared.
    pub venue: Option<String>,
    pub url: Option<String>,
    pub file: String,
    pub line: u32,
}

impl BibEntry {
    /// The arXiv id this entry names, from `eprint` or from a DataCite arXiv DOI.
    pub fn arxiv_id(&self) -> Option<String> {
        self.arxiv.clone().or_else(|| {
            self.doi.as_ref()?.strip_prefix("10.48550/arxiv.").map(str::to_string)
        })
    }

    /// A title reduced to comparable words, for spotting the same work entered twice.
    pub fn title_key(&self) -> Option<String> {
        let t = self.title.as_ref()?.to_lowercase();
        let words: Vec<String> = t
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(str::to_string)
            .collect();
        (!words.is_empty()).then(|| words.join(" "))
    }

    /// An arXiv entry with nothing saying it was published. An arXiv DOI is not a publication.
    pub fn looks_like_preprint(&self) -> bool {
        self.arxiv_id().is_some() && self.venue.is_none() && self.doi.as_deref().is_none_or(|d| d.starts_with("10.48550/arxiv."))
    }

    /// Fields a citation needs to be usable.
    pub fn missing_fields(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.title.is_none() {
            out.push("title");
        }
        if self.author.is_none() {
            out.push("author");
        }
        if self.year.is_none() {
            out.push("year");
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub words: usize,
    /// Files this one pulls in, in order.
    pub inputs: Vec<String>,
}

/// Everything the map is built from.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Paper {
    pub main_file: String,
    pub files: Vec<FileInfo>,
    pub sections: Vec<Section>,
    pub objects: Vec<Object>,
    pub chunks: Vec<Chunk>,
    pub bib: Vec<BibEntry>,
    /// Label → the object or section that defines it.
    pub labels: BTreeMap<String, usize>,
    /// Every `\ref` site: label, file, line.
    pub ref_sites: Vec<(String, String, u32)>,
    /// `\ref` targets that nothing defines, at the line that writes them.
    pub undefined_refs: Vec<(String, String, u32)>,
    pub total_words: usize,
}

impl Paper {
    pub fn section(&self, i: usize) -> &Section {
        &self.sections[i]
    }

    /// How often each label is referenced.
    pub fn ref_counts(&self) -> BTreeMap<&str, usize> {
        let mut out: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.sections {
            for r in &s.refs {
                *out.entry(r.as_str()).or_default() += 1;
            }
        }
        out
    }

    pub fn cite_counts(&self) -> BTreeMap<&str, usize> {
        let mut out: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.sections {
            for c in &s.cites {
                *out.entry(c.as_str()).or_default() += 1;
            }
        }
        out
    }
}

/// Read `main_file` and everything it pulls in, plus the bibliography it names.
pub fn scan(project_dir: &std::path::Path, main_file: &str) -> Paper {
    let mut paper = Paper { main_file: main_file.to_string(), ..Paper::default() };
    let mut seen = Vec::new();
    let mut appendix = false;
    read_file(project_dir, main_file, &mut paper, &mut seen, &mut appendix, 0);

    // Labels defined by sections, then by objects. Numbering follows reading order.
    number_sections(&mut paper);
    for (i, s) in paper.sections.iter().enumerate() {
        if let Some(l) = &s.label {
            paper.labels.entry(l.clone()).or_insert(i);
        }
    }
    let base = paper.sections.len();
    for (i, o) in paper.objects.iter().enumerate() {
        if let Some(l) = &o.label {
            paper.labels.entry(l.clone()).or_insert(base + i);
        }
    }
    let defined: Vec<String> = paper.labels.keys().cloned().collect();
    for (label, file, line) in &paper.ref_sites {
        if !defined.contains(label) {
            paper.undefined_refs.push((label.clone(), file.clone(), *line));
        }
    }
    paper.total_words = paper.files.iter().map(|f| f.words).sum();

    // The bibliography files the document names, if they are in the project.
    let mut bibs = Vec::new();
    for f in &paper.files {
        if let Ok(text) = std::fs::read_to_string(project_dir.join(&f.path)) {
            for m in BIBLIOGRAPHY.captures_iter(&strip_comments(&text)) {
                for name in m[1].split(',') {
                    let name = name.trim();
                    let rel = if name.ends_with(".bib") { name.to_string() } else { format!("{name}.bib") };
                    if !bibs.contains(&rel) {
                        bibs.push(rel);
                    }
                }
            }
        }
    }
    for rel in bibs {
        let Ok(text) = std::fs::read_to_string(project_dir.join(&rel)) else { continue };
        for m in BIB_ENTRY.captures_iter(&text) {
            let start = m.get(0).unwrap().start();
            let line = text[..start].lines().count() as u32 + 1;
            // The entry runs to the line that starts the next one.
            let body_end = text[start + 1..].find("\n@").map(|e| start + 1 + e).unwrap_or(text.len());
            let body = &text[start..body_end];
            let doi = field(body, "doi").map(|d| {
                d.trim_start_matches("https://doi.org/").trim_start_matches("http://dx.doi.org/").to_lowercase()
            });
            let url = field(body, "url");
            let arxiv = field(body, "eprint")
                .filter(|_| field(body, "archiveprefix").is_none_or(|a| a.eq_ignore_ascii_case("arxiv")))
                .or_else(|| {
                    url.as_deref()
                        .filter(|u| u.contains("arxiv.org"))
                        .and_then(|u| u.rsplit('/').next())
                        .map(|id| id.trim_end_matches(".pdf").to_string())
                })
                .map(|id| id.trim_start_matches("arXiv:").to_string());
            paper.bib.push(BibEntry {
                key: m[2].to_string(),
                kind: m[1].to_ascii_lowercase(),
                title: field(body, "title"),
                year: field(body, "year"),
                author: field(body, "author"),
                doi,
                arxiv,
                venue: field(body, "journal").or_else(|| field(body, "booktitle")).or_else(|| field(body, "publisher")),
                url,
                file: rel.clone(),
                line,
            });
        }
    }
    paper
}

/// One field of a BibTeX entry: `title = {…}`, `title="…"` or a bare `year = 2024`. Whitespace
/// around the `=` is free-form, as BibTeX allows.
fn field(entry: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r"(?i)\b{}\s*=\s*", regex::escape(name))).ok()?;
    let m = re.find(entry)?;
    let rest = &entry[m.end()..];
    let raw = match rest.chars().next()? {
        '{' => &rest[1..matching_brace(rest)?],
        '"' => &rest[1..rest[1..].find('"')? + 1],
        // A bare value ends at the next comma or the end of the entry.
        _ => {
            let end = rest.find([',', '\n', '}']).unwrap_or(rest.len());
            &rest[..end]
        }
    };
    let value: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = value.trim_matches(|c| c == '{' || c == '}').trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Offset of the `}` closing the `{` at index 0.
fn matching_brace(s: &str) -> Option<usize> {
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

fn read_file(
    dir: &std::path::Path,
    rel: &str,
    paper: &mut Paper,
    seen: &mut Vec<String>,
    appendix: &mut bool,
    depth: usize,
) {
    if depth > 12 || seen.contains(&rel.to_string()) {
        return;
    }
    seen.push(rel.to_string());
    let Ok(raw) = std::fs::read_to_string(dir.join(rel)) else { return };
    let text = strip_comments(&raw);
    // Claim this file's place before recursing, so the list reads in document order.
    let slot = paper.files.len();
    paper.files.push(FileInfo { path: rel.to_string(), ..FileInfo::default() });
    let mut info = FileInfo { path: rel.to_string(), ..FileInfo::default() };
    let mut current: Option<usize> = None;
    // The paragraph being accumulated: its text, the line it started on, and its section.
    let mut para = String::new();
    let mut para_line = 0u32;
    let mut para_section: Option<usize> = None;
    // Open environments, innermost last, with the line each began on.
    let mut envs: Vec<(String, u32, Option<String>, Option<String>)> = Vec::new();

    for (i, line) in text.lines().enumerate() {
        let line_no = i as u32 + 1;
        if line.contains("\\appendix") {
            *appendix = true;
        }
        // A heading ends the paragraph before it and is not part of the one after it.
        let heading = SECTION.is_match(line);
        if heading {
            flush_chunk(paper, rel, &mut para, para_line, para_section);
            para_section = None;
        }
        if let Some(m) = SECTION.captures(line) {
            let kind = m[1].to_string();
            let start = m.get(0).unwrap().end() - 1;
            let title = matching_brace(&line[start..])
                .map(|e| line[start + 1..start + e].trim().to_string())
                .unwrap_or_default();
            let level = match kind.as_str() {
                "part" | "chapter" => 0,
                "section" => 1,
                "subsection" => 2,
                "subsubsection" => 3,
                _ => 4,
            };
            paper.sections.push(Section {
                file: rel.to_string(),
                line: line_no,
                level,
                title: clean(&title),
                number: String::new(),
                label: None,
                words: 0,
                refs: Vec::new(),
                cites: Vec::new(),
                objects: Vec::new(),
                in_appendix: *appendix,
            });
            current = Some(paper.sections.len() - 1);
        }

        for m in BEGIN.captures_iter(line) {
            envs.push((m[1].to_string(), line_no, None, None));
        }
        if let Some(m) = CAPTION.captures(line) {
            let start = m.get(0).unwrap().end() - 1;
            if let Some(e) = matching_brace(&line[start..]) {
                if let Some(env) = envs.last_mut() {
                    env.2 = Some(clean(&line[start + 1..start + e]));
                }
            }
        }
        for m in LABEL.captures_iter(line) {
            let label = m[1].trim().to_string();
            // A label inside an environment belongs to it. Otherwise it belongs to the section.
            match envs.iter_mut().rev().find(|(env, _, _, _)| Kind::of_environment(env).is_some()) {
                Some(env) => env.3 = Some(label),
                None => {
                    if let Some(c) = current {
                        paper.sections[c].label.get_or_insert(label);
                    }
                }
            }
        }
        if line.contains("\\end{") {
            for env in ["figure", "table", "equation", "align", "gather", "multline", "theorem", "lemma", "proposition",
                "corollary", "definition", "remark", "claim", "conjecture", "algorithm", "lstlisting", "verbatim", "minted",
                "tabular", "longtable", "wrapfigure", "subfigure", "eqnarray", "flalign", "algorithmic"] {
                for name in [env.to_string(), format!("{env}*")] {
                    if !line.contains(&format!("\\end{{{name}}}")) {
                        continue;
                    }
                    if let Some(pos) = envs.iter().rposition(|(e, _, _, _)| *e == name) {
                        let (e, began, caption, label) = envs.remove(pos);
                        if let Some(kind) = Kind::of_environment(&e) {
                            // A tabular inside a table is the same object. Keep the outer one.
                            let nested = envs.iter().any(|(o, _, _, _)| Kind::of_environment(o) == Some(kind));
                            if !nested && (label.is_some() || caption.is_some()) {
                                paper.objects.push(Object {
                                    kind,
                                    label,
                                    caption,
                                    file: rel.to_string(),
                                    line: began,
                                    section: current,
                                });
                                if let Some(c) = current {
                                    let idx = paper.objects.len() - 1;
                                    paper.sections[c].objects.push(idx);
                                }
                            }
                        }
                    }
                }
            }
        }

        for m in REF.captures_iter(line) {
            for r in m[1].split(',') {
                let r = r.trim().to_string();
                paper.ref_sites.push((r.clone(), rel.to_string(), line_no));
                if let Some(c) = current {
                    if !paper.sections[c].refs.contains(&r) {
                        paper.sections[c].refs.push(r);
                    }
                }
            }
        }
        for m in CITE.captures_iter(line) {
            for k in m[1].split(',') {
                let k = k.trim().to_string();
                if k == "*" {
                    continue;
                }
                if let Some(c) = current {
                    if !paper.sections[c].cites.contains(&k) {
                        paper.sections[c].cites.push(k);
                    }
                }
            }
        }

        // Blank lines end a paragraph, as in TeX. Long paragraphs are cut so a hit stays readable.
        if heading {
            // nothing to accumulate
        } else if line.trim().is_empty() {
            flush_chunk(paper, rel, &mut para, para_line, para_section);
            para_section = None;
        } else {
            if para.is_empty() {
                para_line = line_no;
                para_section = current;
            }
            para.push_str(line.trim());
            para.push(' ');
            if para.len() > 1200 {
                flush_chunk(paper, rel, &mut para, para_line, para_section);
                para_section = None;
            }
        }

        let words = count_words(line);
        info.words += words;
        if let Some(c) = current {
            paper.sections[c].words += words;
        }

        for m in INPUT.captures_iter(line) {
            let raw = m[1].trim().trim_start_matches("./");
            let child = if raw.ends_with(".tex") { raw.to_string() } else { format!("{raw}.tex") };
            info.inputs.push(child.clone());
            read_file(dir, &child, paper, seen, appendix, depth + 1);
        }
    }
    flush_chunk(paper, rel, &mut para, para_line, para_section);
    paper.files[slot] = info;
}

/// Keep a paragraph if it has prose in it. Markup-only blocks are not passages.
fn flush_chunk(paper: &mut Paper, rel: &str, para: &mut String, line: u32, section: Option<usize>) {
    let text = FURNITURE.replace_all(para, " ").split_whitespace().collect::<Vec<_>>().join(" ");
    para.clear();
    if count_words(&text) < 4 {
        return;
    }
    paper.chunks.push(Chunk { file: rel.to_string(), line, section, text });
}

/// `4`, `4.2`, `A`, `A.1`. Appendices letter their top level, as LaTeX does.
fn number_sections(paper: &mut Paper) {
    let mut counters = [0usize; 5];
    let mut appendix_letter = 0u8;
    for i in 0..paper.sections.len() {
        let level = paper.sections[i].level.min(4) as usize;
        let level = if level == 0 { 1 } else { level };
        counters[level - 1] += 1;
        for c in counters.iter_mut().skip(level) {
            *c = 0;
        }
        let in_appendix = paper.sections[i].in_appendix;
        if in_appendix && level == 1 {
            appendix_letter += 1;
        }
        let mut parts: Vec<String> = Vec::new();
        for (d, c) in counters.iter().take(level).enumerate() {
            if d == 0 && in_appendix {
                parts.push(((b'A' + appendix_letter.saturating_sub(1)) as char).to_string());
            } else {
                parts.push(c.to_string());
            }
        }
        paper.sections[i].number = parts.join(".");
    }
}

/// Field lookup for the bibliography manager.
pub fn field_public(entry: &str, name: &str) -> Option<String> {
    field(entry, name)
}

/// How many entries a .bib file holds. The manager's tests use this.
#[cfg(test)]
pub fn scan_bib_count(text: &str) -> usize {
    BIB_ENTRY.captures_iter(text).count()
}

/// Comment stripping for other modules (the maths extractor reads raw files too).
pub fn strip_comments_public(text: &str) -> String {
    strip_comments(text)
}

/// Drop everything after an unescaped `%`, keeping line count intact.
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

/// Strip LaTeX down to readable words. Math goes. Structural commands take their arguments with
/// them, because `sec:intro` in a `\label` is not prose. Other commands leave their text behind.
pub fn prose(text: &str) -> String {
    let mut s = text.to_string();
    for (open, close) in [("$$", "$$"), ("$", "$"), ("\\[", "\\]"), ("\\(", "\\)")] {
        while let (Some(a), Some(b)) = (s.find(open), s.rfind(close)) {
            if b <= a + open.len() {
                break;
            }
            s.replace_range(a..b + close.len(), " ");
        }
    }
    let s = STRUCTURAL.replace_all(&s, " ");
    COMMAND.replace_all(&s, " ").replace(['{', '}', '~', '\\'], " ")
}

/// Prose words, for section and file word counts.
fn count_words(line: &str) -> usize {
    prose(line)
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|w| w.chars().any(|c| c.is_alphabetic()) && w.len() > 1)
        .count()
}

/// A title or caption as a person would read it: no commands, no braces, collapsed spaces.
fn clean(text: &str) -> String {
    let re = Regex::new(r"\\[a-zA-Z@]+\*?").expect("regex");
    let text = re.replace_all(text, " ");
    text.replace(['{', '}', '~'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        dir
    }

    const MAIN: &str = r#"
\documentclass{article}
\begin{document}
\section{Introduction}
\label{sec:intro}
We build on prior work~\cite{knuth84,lamport94} and show a result.
% \begin{figure} commented out \label{fig:ghost} \end{figure}
\input{sections/method}
\section{Results}
\label{sec:results}
See Table~\ref{tab:main} and Section~\ref{sec:method}, also \cref{fig:plot,sec:missing}.
\begin{table}[t]
\caption{Success by suite, \textbf{mean} over seeds}
\label{tab:main}
\begin{tabular}{ll}a & b\end{tabular}
\end{table}
\appendix
\section{Extra}
\label{app:extra}
\bibliography{refs}
\end{document}
"#;

    const METHOD: &str = r#"
\subsection{Method}
\label{sec:method}
The method has $x + y$ maths and words here.
\begin{figure}
\includegraphics{plot.pdf}
\caption{A plot of things}
\label{fig:plot}
\end{figure}
\begin{equation}
\label{eq:one}
a = b
\end{equation}
"#;

    const BIB: &str = r#"@article{knuth84, author = {Donald Knuth}, title = {Literate programming}, year = {1984}}
@book{lamport94,
  author = {Leslie Lamport},
  title = {LaTeX: a document preparation system},
  year = {1994}}
@misc{uncited, title = {Nobody cites me}, year = {2020}}
"#;

    #[test]
    fn scans_structure_across_inputs() {
        let dir = project(&[("main.tex", MAIN), ("sections/method.tex", METHOD), ("refs.bib", BIB)]);
        let p = scan(dir.path(), "main.tex");

        let titles: Vec<(&str, &str, u8)> =
            p.sections.iter().map(|s| (s.number.as_str(), s.title.as_str(), s.level)).collect();
        assert_eq!(
            titles,
            vec![("1", "Introduction", 1), ("1.1", "Method", 2), ("2", "Results", 1), ("A", "Extra", 1)]
        );
        assert!(p.sections[3].in_appendix);
        assert_eq!(p.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["main.tex", "sections/method.tex"]);

        // Objects keep their caption, label and the section they sit in.
        let objs: Vec<(&str, Option<&str>)> =
            p.objects.iter().map(|o| (o.kind.as_str(), o.label.as_deref())).collect();
        assert!(objs.contains(&("figure", Some("fig:plot"))), "{objs:?}");
        assert!(objs.contains(&("table", Some("tab:main"))), "{objs:?}");
        assert!(objs.contains(&("equation", Some("eq:one"))), "{objs:?}");
        let table = p.objects.iter().find(|o| o.label.as_deref() == Some("tab:main")).unwrap();
        assert_eq!(table.caption.as_deref(), Some("Success by suite, mean over seeds"));
        assert_eq!(p.sections[table.section.unwrap()].title, "Results");

        // A commented-out label is not structure.
        assert!(!p.labels.contains_key("fig:ghost"));
        // Undefined references are found, defined ones are not reported.
        assert_eq!(p.undefined_refs, vec![("sec:missing".to_string(), "main.tex".to_string(), 11)]);

        let cites = p.cite_counts();
        assert_eq!(cites.get("knuth84"), Some(&1));
        assert_eq!(p.bib.len(), 3);
        let knuth = p.bib.iter().find(|b| b.key == "knuth84").unwrap();
        assert_eq!(knuth.title.as_deref(), Some("Literate programming"));
        assert_eq!(knuth.year.as_deref(), Some("1984"));
        let lamport = p.bib.iter().find(|b| b.key == "lamport94").unwrap();
        assert_eq!(lamport.title.as_deref(), Some("LaTeX: a document preparation system"));

        assert!(p.total_words > 10, "{}", p.total_words);
        let intro = &p.sections[0];
        assert!(intro.words >= 8 && intro.words <= 12, "prose only: {}", intro.words);
    }

    #[test]
    fn missing_input_is_not_fatal() {
        let dir = project(&[("main.tex", "\\section{A}\n\\input{nope}\nText here.\n")]);
        let p = scan(dir.path(), "main.tex");
        assert_eq!(p.sections.len(), 1);
        assert_eq!(p.files[0].inputs, vec!["nope.tex"]);
    }
}
