//! TeX log parser. It produces plain diagnostics with a file and a line. The `hints` module adds
//! the codes, the hints and the fixes. The parser is written for the log output of Tectonic, shown
//! in tests/fixtures. That output differs a little from TeX Live. It has no `./` prefix, and no
//! extension on an `\input` file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Info,
    Lint,
}

/// A one-click fix, resolved to a concrete edit the client applies through the CRDT.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fix {
    Insert {
        label: String,
        file: String,
        after_line: u32,
        text: String,
    },
    Replace {
        label: String,
        file: String,
        line: u32,
        find: String,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: Level,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
    /// The log excerpt that this came from. The "show raw" control uses it.
    pub raw: String,
}

/// What the parser can tell before the hints database is used.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDiag {
    pub level: Level,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
    /// The source context TeX printed after `l.N`, if any.
    pub context: String,
    pub raw: String,
}

const WRAP_WIDTH: usize = 79;

/// TeX wraps log lines at 79 characters. Join them again.
pub fn unwrap_lines(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    for line in log.lines() {
        let line = line.trim_end_matches('\r');
        match pending.take() {
            Some(mut p) => {
                p.push_str(line);
                if line.chars().count() == WRAP_WIDTH {
                    pending = Some(p);
                } else {
                    out.push(p);
                }
            }
            None => {
                if line.chars().count() == WRAP_WIDTH && !line.starts_with('!') {
                    pending = Some(line.to_string());
                } else {
                    out.push(line.to_string());
                }
            }
        }
    }
    if let Some(p) = pending {
        out.push(p);
    }
    out
}

fn is_file_token(tok: &str) -> bool {
    !tok.is_empty()
        && (tok.contains('.') || tok.contains('/'))
        && !tok.starts_with('<')
        && !tok.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Track the nesting of `(file` and `)` on a line. The caller skips error lines and help lines.
fn scan_parens(line: &str, stack: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '(' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && !chars[j].is_whitespace() && chars[j] != ')' && chars[j] != '(' {
                    j += 1;
                }
                let tok: String = chars[start..j].iter().collect();
                if is_file_token(&tok) {
                    stack.push(tok);
                    i = j;
                    continue;
                } else {
                    // This is not a file. Treat it as ordinary text, but keep the balance for `)`.
                    stack.push(String::new());
                }
            }
            ')' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
}

fn current_file(stack: &[String]) -> Option<String> {
    stack.iter().rev().find(|s| !s.is_empty()).cloned()
}

pub fn parse_log(log: &str) -> Vec<RawDiag> {
    let lines = unwrap_lines(log);
    let mut stack: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let mut i = 0;
    let input_line = regex::Regex::new(r"on input line (\d+)").expect("regex");
    let at_lines = regex::Regex::new(r"at lines (\d+)--(\d+)").expect("regex");
    let latex_warning = regex::Regex::new(r"^(?:LaTeX|Package (\S+)|Class (\S+)) Warning: (.*)$").expect("regex");
    let latex_font_warning = regex::Regex::new(r"^LaTeX Font Warning: (.*)$").expect("regex");
    let box_warning = regex::Regex::new(r"^(Overfull|Underfull) \\([hv])box \(([^)]*)\)(.*)$").expect("regex");

    while i < lines.len() {
        let line = &lines[i];
        if let Some(rest) = line.strip_prefix('!') {
            let message = rest.trim().trim_end_matches('.').to_string();
            let mut raw = vec![line.clone()];
            let mut lno = None;
            let mut context = String::new();
            let mut head = String::new();
            let mut j = i + 1;
            while j < lines.len() && j <= i + 12 {
                let l = &lines[j];
                raw.push(l.clone());
                if let Some(rest) = l.strip_prefix("l.") {
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    lno = digits.parse().ok();
                    // TeX splits the source line where it stopped. The head ends with the token
                    // that caused the error. The next line holds the rest.
                    head = rest[digits.len()..].trim().to_string();
                    context = head.clone();
                    if j + 1 < lines.len() {
                        let cont = lines[j + 1].trim();
                        if !cont.is_empty() {
                            context = format!("{context}{cont}");
                            raw.push(lines[j + 1].clone());
                            j += 1;
                        }
                    }
                    break;
                }
                if l.starts_with('!') {
                    raw.pop();
                    j -= 1;
                    break;
                }
                j += 1;
            }
            let message = decorate(&message, &head);
            out.push(RawDiag {
                level: Level::Error,
                file: current_file(&stack),
                line: lno,
                message,
                context,
                raw: raw.join("\n"),
            });
            i = j + 1;
            continue;
        }

        if let Some(c) = box_warning.captures(line) {
            let kind = &c[1];
            let dir = &c[2];
            let detail = &c[3];
            let tail = c[4].trim();
            let lno = at_lines.captures(tail).and_then(|m| m[1].parse().ok()).or_else(|| {
                input_line.captures(tail).and_then(|m| m[1].parse().ok())
            });
            let mut raw = vec![line.clone()];
            let mut k = i + 1;
            while k < lines.len() && !lines[k].trim().is_empty() && !lines[k].starts_with('!') && k <= i + 4 {
                raw.push(lines[k].clone());
                k += 1;
            }
            out.push(RawDiag {
                level: if kind == "Overfull" { Level::Warning } else { Level::Info },
                file: current_file(&stack),
                line: lno,
                message: format!("{kind} \\{dir}box ({detail}) {tail}").trim().to_string(),
                context: String::new(),
                raw: raw.join("\n"),
            });
            i = k;
            continue;
        }

        if let Some(c) = latex_font_warning.captures(line) {
            let lno = input_line.captures(line).and_then(|m| m[1].parse().ok());
            out.push(RawDiag {
                level: Level::Info,
                file: current_file(&stack),
                line: lno,
                message: format!("Font: {}", c[1].trim_end_matches('.')),
                context: String::new(),
                raw: line.clone(),
            });
            i += 1;
            continue;
        }

        if let Some(c) = latex_warning.captures(line) {
            let pkg = c.get(1).or(c.get(2)).map(|m| m.as_str().to_string());
            let mut message = c[3].trim().to_string();
            let mut raw = vec![line.clone()];
            // Package warnings continue on lines prefixed with `(pkg)`.
            if let Some(p) = &pkg {
                let prefix = format!("({p})");
                let mut k = i + 1;
                while k < lines.len() && lines[k].starts_with(&prefix) {
                    message.push(' ');
                    message.push_str(lines[k][prefix.len()..].trim());
                    raw.push(lines[k].clone());
                    k += 1;
                }
                i = k - 1;
            }
            let lno = input_line.captures(&message).and_then(|m| m[1].parse().ok());
            let message = input_line.replace(&message, "").trim().trim_end_matches('.').trim().to_string();
            let level = if message.starts_with("There were undefined")
                || message.starts_with("Label(s) may have changed")
                || message.starts_with("Citation(s) may have changed")
            {
                Level::Info
            } else {
                Level::Warning
            };
            let message = match &pkg {
                Some(p) => format!("{p}: {message}"),
                None => message,
            };
            out.push(RawDiag {
                level,
                file: current_file(&stack),
                line: lno,
                message,
                context: String::new(),
                raw: raw.join("\n"),
            });
            i += 1;
            continue;
        }

        scan_parens(line, &mut stack);
        i += 1;
    }
    out
}

/// Some messages only make sense with the context TeX printed after them.
fn decorate(message: &str, context: &str) -> String {
    if message == "Undefined control sequence" {
        if let Some(cmd) = last_control_sequence(context) {
            return format!("Undefined control sequence {cmd}");
        }
    }
    message.to_string()
}

fn last_control_sequence(head: &str) -> Option<String> {
    let re = regex::Regex::new(r"\\[a-zA-Z@]+\*?").ok()?;
    re.find_iter(head).last().map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_79_column_lines() {
        let long = "a".repeat(79);
        let joined = unwrap_lines(&format!("{long}\nrest\nshort\n"));
        assert_eq!(joined, vec![format!("{long}rest"), "short".to_string()]);
    }

    #[test]
    fn tracks_file_stack() {
        let mut stack = Vec::new();
        scan_parens("(main.tex (article.cls", &mut stack);
        assert_eq!(current_file(&stack).as_deref(), Some("article.cls"));
        scan_parens(") (sections/method", &mut stack);
        assert_eq!(current_file(&stack).as_deref(), Some("sections/method"));
        scan_parens("(Font) text )", &mut stack);
        assert_eq!(current_file(&stack).as_deref(), Some("main.tex"));
    }

    #[test]
    fn undefined_control_sequence_names_the_command() {
        let log = "(main.tex\n! Undefined control sequence.\nl.9 \\citep\n          {vaswani2017}\nhelp text\n";
        let d = parse_log(log);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "Undefined control sequence \\citep");
        assert_eq!(d[0].line, Some(9));
        assert_eq!(d[0].file.as_deref(), Some("main.tex"));
    }
}
