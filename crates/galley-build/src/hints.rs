//! The hints database. The files `hints/*.yaml` map message patterns to codes, hints, explanations
//! and one-click fixes. Galley resolves a fix against the project, so the client gets a concrete
//! edit.

use regex::Regex;
use serde::Deserialize;

use crate::log::{Diagnostic, Fix, Level, RawDiag};
use crate::{Error, Result};

const BUNDLED: &str = include_str!("../../../hints/latex.yaml");

#[derive(Debug, Deserialize)]
struct Entry {
    code: String,
    #[serde(rename = "match")]
    pattern: String,
    #[serde(default)]
    when_context: Option<String>,
    /// Replace the severity from the parser, for example a warning that is only information.
    #[serde(default)]
    level: Option<Level>,
    hint: String,
    #[serde(default)]
    explain: Option<String>,
    #[serde(default)]
    fix: Option<FixSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FixSpec {
    AddPackage { package: String, label: String },
    ReplaceOnLine { find: String, text: String, label: String },
}

struct Compiled {
    entry: Entry,
    re: Regex,
    ctx: Option<Regex>,
}

pub struct Hints {
    entries: Vec<Compiled>,
}

/// What the resolver needs from the project to turn a fix template into an edit.
pub struct ProjectView<'a> {
    pub main_file: &'a str,
    pub main_text: &'a str,
}

impl Hints {
    pub fn bundled() -> Result<Hints> {
        Hints::from_yaml(BUNDLED)
    }

    pub fn from_yaml(yaml: &str) -> Result<Hints> {
        let entries: Vec<Entry> = serde_yaml_ng::from_str(yaml).map_err(|e| Error::Hints(e.to_string()))?;
        let mut compiled = Vec::with_capacity(entries.len());
        for entry in entries {
            let re = Regex::new(&entry.pattern).map_err(|e| Error::Hints(format!("{}: {e}", entry.code)))?;
            let ctx = match &entry.when_context {
                Some(p) => Some(Regex::new(p).map_err(|e| Error::Hints(format!("{}: {e}", entry.code)))?),
                None => None,
            };
            compiled.push(Compiled { entry, re, ctx });
        }
        Ok(Hints { entries: compiled })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Attach the code, the hint, the explanation and a resolved fix.
    pub fn annotate(&self, raw: RawDiag, project: &ProjectView<'_>) -> Diagnostic {
        for c in &self.entries {
            let Some(caps) = c.re.captures(&raw.message) else { continue };
            if let Some(ctx) = &c.ctx {
                if !ctx.is_match(&raw.context) {
                    continue;
                }
            }
            let expand = |s: &str| expand_captures(s, &caps);
            let fix = c.entry.fix.as_ref().and_then(|f| match f {
                FixSpec::AddPackage { package, label } => {
                    if package_loaded(project.main_text, package) {
                        return None;
                    }
                    Some(Fix::Insert {
                        label: label.clone(),
                        file: project.main_file.to_string(),
                        after_line: preamble_insert_line(project.main_text)?,
                        text: format!("\\usepackage{{{package}}}"),
                    })
                }
                FixSpec::ReplaceOnLine { find, text, label } => Some(Fix::Replace {
                    label: expand(label),
                    file: raw.file.clone()?,
                    line: raw.line?,
                    find: expand(find),
                    text: expand(text),
                }),
            });
            let hint = Some(expand(&c.entry.hint));
            let explain = c.entry.explain.as_deref().map(expand);
            return Diagnostic {
                level: c.entry.level.unwrap_or(raw.level),
                file: raw.file,
                line: raw.line,
                code: c.entry.code.clone(),
                message: raw.message,
                hint,
                explain,
                fix,
                raw: raw.raw,
            };
        }
        Diagnostic {
            level: raw.level,
            file: raw.file,
            line: raw.line,
            code: match raw.level {
                Level::Error => "unknown-error".into(),
                Level::Warning => "unknown-warning".into(),
                _ => "info".into(),
            },
            message: raw.message,
            hint: None,
            explain: None,
            fix: None,
            raw: raw.raw,
        }
    }
}

fn expand_captures(template: &str, caps: &regex::Captures<'_>) -> String {
    let mut out = template.to_string();
    for i in (1..caps.len()).rev() {
        if let Some(m) = caps.get(i) {
            out = out.replace(&format!("${i}"), m.as_str());
        }
    }
    out
}

fn package_loaded(main: &str, package: &str) -> bool {
    let re = Regex::new(&format!(r"(?m)^[^%\n]*\\usepackage\s*(?:\[[^\]]*\])?\s*\{{[^}}]*\b{}\b[^}}]*\}}", regex::escape(package)))
        .expect("regex");
    re.is_match(main)
}

/// The 1-based line after which a new `\usepackage` goes. This is the line of the last
/// `\usepackage`, or the line of `\documentclass`.
fn preamble_insert_line(main: &str) -> Option<u32> {
    let mut last_pkg = None;
    let mut docclass = None;
    for (i, line) in main.lines().enumerate() {
        let code = line.split('%').next().unwrap_or("");
        if code.contains("\\begin{document}") {
            break;
        }
        if code.contains("\\usepackage") {
            last_pkg = Some(i as u32 + 1);
        }
        if code.contains("\\documentclass") && docclass.is_none() {
            docclass = Some(i as u32 + 1);
        }
    }
    last_pkg.or(docclass)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(level: Level, message: &str, file: &str, line: u32) -> RawDiag {
        RawDiag {
            level,
            file: Some(file.into()),
            line: Some(line),
            message: message.into(),
            context: String::new(),
            raw: String::new(),
        }
    }

    const MAIN: &str = "\\documentclass{article}\n\\usepackage{graphicx}\n\\usepackage{amsmath}\n\n\\begin{document}\nhi\n\\end{document}\n";

    #[test]
    fn bundled_hints_load() {
        let h = Hints::bundled().unwrap();
        assert!(h.len() >= 30);
    }

    #[test]
    fn natbib_fix_inserts_after_last_package() {
        let h = Hints::bundled().unwrap();
        let d = h.annotate(
            raw(Level::Error, "Undefined control sequence \\citep", "main.tex", 9),
            &ProjectView { main_file: "main.tex", main_text: MAIN },
        );
        assert_eq!(d.code, "undefined-control-sequence-natbib");
        assert_eq!(
            d.fix,
            Some(Fix::Insert {
                label: "Add natbib".into(),
                file: "main.tex".into(),
                after_line: 3,
                text: "\\usepackage{natbib}".into()
            })
        );
    }

    #[test]
    fn no_fix_when_package_already_loaded() {
        let h = Hints::bundled().unwrap();
        let d = h.annotate(
            raw(Level::Error, "Undefined control sequence \\text", "main.tex", 9),
            &ProjectView { main_file: "main.tex", main_text: MAIN },
        );
        assert_eq!(d.code, "undefined-control-sequence-amsmath");
        assert_eq!(d.fix, None);
    }

    #[test]
    fn env_mismatch_fix_uses_captures() {
        let h = Hints::bundled().unwrap();
        let d = h.annotate(
            raw(Level::Error, "LaTeX Error: \\begin{itemize} on input line 10 ended by \\end{enumerate}", "main.tex", 12),
            &ProjectView { main_file: "main.tex", main_text: MAIN },
        );
        assert_eq!(d.code, "env-mismatch");
        assert_eq!(
            d.fix,
            Some(Fix::Replace {
                label: "Use \\end{itemize}".into(),
                file: "main.tex".into(),
                line: 12,
                find: "\\\\end\\{enumerate\\}".into(),
                text: "\\end{itemize}".into()
            })
        );
        assert!(d.hint.unwrap().contains("Change \\end{enumerate} to \\end{itemize}"));
    }

    #[test]
    fn unknown_messages_keep_raw() {
        let h = Hints::bundled().unwrap();
        let d = h.annotate(raw(Level::Error, "Something bizarre", "main.tex", 1), &ProjectView { main_file: "main.tex", main_text: MAIN });
        assert_eq!(d.code, "unknown-error");
        assert_eq!(d.hint, None);
    }
}
