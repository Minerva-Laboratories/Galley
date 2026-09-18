//! Grammar and style checking through LanguageTool (SPEC §9.3, §13).
//!
//! LanguageTool needs Java, so it is never bundled and never required. `[grammar] languagetool`
//! is off unless the operator points it at a server. Checking runs on demand. It never runs during
//! a build and it never delays a save. When the server is unreachable, the answer is "unavailable"
//! and the server shows no error card.
//!
//! Document text leaves the machine if the configured server is remote. The default is off, and
//! the setting is the opt-in. The public API at languagetool.org is a remote server like any
//! other. A self-hosted server keeps the text on the machine.

use std::time::Duration;

use galley_build::log::{Diagnostic, Fix, Level};
use serde::Deserialize;

/// Where `"auto"` looks: the port a locally run `languagetool-server` uses by default.
const LOCAL: &str = "http://127.0.0.1:8081";
/// LanguageTool's own limit for an anonymous request is below this. Galley keeps requests small
/// so one long chapter cannot stall the check.
const MAX_CHARS: usize = 40_000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Grammar checking is off. Set [grammar] languagetool in galley.toml to a LanguageTool server URL.")]
    Off,
    #[error("LanguageTool did not answer at {url}. Start it, or set [grammar] languagetool to its URL.")]
    Unreachable { url: String },
    #[error("LanguageTool answered with something unexpected: {0}")]
    Shape(String),
}

/// Resolve the config value to a base URL, or `Off`.
pub fn endpoint(setting: &str) -> Result<String, Error> {
    match setting.trim() {
        "" | "off" | "false" => Err(Error::Off),
        "auto" => Ok(LOCAL.to_string()),
        url => Ok(url.trim_end_matches('/').to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct Response {
    matches: Vec<Match>,
}

#[derive(Debug, Deserialize)]
struct Match {
    message: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    replacements: Vec<Replacement>,
    #[serde(default)]
    rule: Rule,
}

#[derive(Debug, Deserialize)]
struct Replacement {
    value: String,
}

#[derive(Debug, Default, Deserialize)]
struct Rule {
    #[serde(default)]
    id: String,
}

/// Check one file's prose. `language` is a LanguageTool code such as `en-US`, or `auto`.
pub async fn check_file(base: &str, path: &str, text: &str, language: &str) -> Result<Vec<Diagnostic>, Error> {
    let masked = mask(text);
    if masked.trim().is_empty() {
        return Ok(Vec::new());
    }
    let truncated: String = masked.chars().take(MAX_CHARS).collect();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| Error::Shape(e.to_string()))?;
    let body = client
        .post(format!("{base}/v2/check"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("text={}&language={}", form_encode(&truncated), form_encode(language)))
        .send()
        .await
        .map_err(|_| Error::Unreachable { url: base.to_string() })?;
    if !body.status().is_success() {
        return Err(Error::Shape(format!("HTTP {}", body.status())));
    }
    let parsed: Response = body.json().await.map_err(|e| Error::Shape(e.to_string()))?;
    let lines = Lines::new(&truncated);
    Ok(parsed.matches.iter().filter_map(|m| diagnostic(path, text, &lines, m)).collect())
}

fn diagnostic(path: &str, original: &str, lines: &Lines, m: &Match) -> Option<Diagnostic> {
    let (line, col) = lines.locate(m.offset)?;
    // The flagged span is blanked in the mask only where it was LaTeX, so read it from the source.
    let source_line = original.lines().nth(line as usize - 1)?;
    let found: String = source_line.chars().skip(col).take(m.length).collect();
    let suggestion = m.replacements.first().map(|r| r.value.clone());
    let hint = match &suggestion {
        Some(s) if !s.is_empty() => format!("Suggested: {s}"),
        _ => "No replacement suggested.".to_string(),
    };
    let fix = suggestion.filter(|s| !s.is_empty() && !found.trim().is_empty()).map(|s| Fix::Replace {
        label: format!("Replace with “{s}”"),
        file: path.to_string(),
        line,
        find: found.clone(),
        text: s,
    });
    Some(Diagnostic {
        level: Level::Lint,
        file: Some(path.to_string()),
        line: Some(line),
        code: if m.rule.id.is_empty() { "grammar".into() } else { format!("grammar/{}", m.rule.id) },
        message: m.message.clone(),
        hint: Some(hint),
        explain: None,
        fix,
        raw: found,
    })
}

/// Percent-encode one form field value.
fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Convert a character offset to a 1-based line and a 0-based column within that line.
struct Lines {
    /// Offset of the first character of each line.
    starts: Vec<usize>,
}

impl Lines {
    fn new(text: &str) -> Lines {
        let mut starts = vec![0];
        for (i, c) in text.chars().enumerate() {
            if c == '\n' {
                starts.push(i + 1);
            }
        }
        Lines { starts }
    }

    fn locate(&self, offset: usize) -> Option<(u32, usize)> {
        let idx = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.checked_sub(1)?,
        };
        Some((idx as u32 + 1, offset - self.starts[idx]))
    }
}

/// Commands whose braced argument is not prose.
const OPAQUE_ARG: &[&str] = &[
    "label", "ref", "eqref", "cref", "Cref", "autoref", "pageref", "nameref", "cite", "citep", "citet", "citeauthor",
    "citeyear", "parencite", "textcite", "autocite", "footcite", "nocite", "includegraphics", "usepackage",
    "documentclass", "input", "include", "bibliography", "bibliographystyle", "url", "texttt", "verb", "newcommand",
    "renewcommand", "setlength", "bibitem", "hypersetup", "definecolor", "lstinputlisting", "addbibresource",
];

/// Environments whose body is not prose.
const OPAQUE_ENV: &[&str] = &[
    "equation", "equation*", "align", "align*", "gather", "gather*", "multline", "multline*", "eqnarray", "eqnarray*",
    "array", "tabular", "tabular*", "tabularx", "verbatim", "lstlisting", "minted", "tikzpicture", "pgfpicture",
    "matrix", "pmatrix", "bmatrix", "displaymath", "math", "figure*", "thebibliography",
];

/// Replace everything that is LaTeX rather than prose with spaces, one space per character, so a
/// character offset into the result still points at the same place in the source.
pub fn mask(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = chars.iter().map(|c| if *c == '\n' { '\n' } else { ' ' }).collect();
    let mut i = 0;
    // Keep prose by default. Blank each span as the walk recognises it.
    while i < chars.len() {
        let c = chars[i];
        match c {
            '%' if i == 0 || chars[i - 1] != '\\' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '$' => {
                let fence = if chars.get(i + 1) == Some(&'$') { 2 } else { 1 };
                i += fence;
                while i < chars.len() && !(chars[i] == '$' && chars.get(i.wrapping_sub(1)) != Some(&'\\')) {
                    i += 1;
                }
                i = (i + fence).min(chars.len());
            }
            '\\' => i = command(&chars, i),
            '{' | '}' | '&' | '_' | '^' | '~' => i += 1,
            _ => {
                out[i] = c;
                i += 1;
            }
        }
    }
    out.into_iter().collect()
}

/// Handle the command that starts at the backslash at `at`, and return where the walk continues.
/// A command whose argument is prose, such as `\textbf{...}` or `\caption{...}`, hands the walk
/// the text inside its braces, so nested commands are recognised there too.
fn command(chars: &[char], at: usize) -> usize {
    let mut i = at + 1;
    if i >= chars.len() {
        return i;
    }
    if !chars[i].is_ascii_alphabetic() {
        // `\\`, `\%` and `\&` are escapes. They are not commands.
        return i + 1;
    }
    let start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    if chars.get(i) == Some(&'*') {
        i += 1;
    }
    let name: String = chars[start..i].iter().collect();
    // Optional arguments are never prose.
    while chars.get(i) == Some(&'[') {
        match close(chars, i, '[', ']') {
            Some(end) => i = end + 1,
            None => return i + 1,
        }
    }
    if name == "begin" || name == "end" {
        let Some(end) = close(chars, i, '{', '}') else { return i };
        let env: String = chars[i + 1..end].iter().collect();
        if name == "begin" && OPAQUE_ENV.contains(&env.as_str()) {
            return skip_env(chars, end + 1, &env);
        }
        return end + 1;
    }
    if chars.get(i) != Some(&'{') {
        return i;
    }
    match close(chars, i, '{', '}') {
        Some(end) if OPAQUE_ARG.contains(&name.as_str()) => end + 1,
        // The argument is prose. Step into the braces and continue the walk.
        _ => i,
    }
}

/// Position of the matching close, given `chars[at] == open`.
fn close(chars: &[char], at: usize, open: char, shut: char) -> Option<usize> {
    if chars.get(at) != Some(&open) {
        return None;
    }
    let mut depth = 0usize;
    for (offset, c) in chars[at..].iter().enumerate() {
        let escaped = offset > 0 && chars[at + offset - 1] == '\\';
        if escaped {
            continue;
        }
        if *c == open {
            depth += 1;
        } else if *c == shut {
            depth -= 1;
            if depth == 0 {
                return Some(at + offset);
            }
        }
    }
    None
}

/// Skip to just past `\end{env}`.
fn skip_env(chars: &[char], from: usize, env: &str) -> usize {
    let needle: Vec<char> = format!("\\end{{{env}}}").chars().collect();
    let mut i = from;
    while i + needle.len() <= chars.len() {
        if chars[i..i + needle.len()] == needle[..] {
            return i + needle.len();
        }
        i += 1;
    }
    chars.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(s: &str) -> String {
        mask(s)
    }

    #[test]
    fn offsets_survive_masking() {
        let text = "Hello \\cite{smith2020} world.";
        let m = masked(text);
        assert_eq!(m.chars().count(), text.chars().count());
        assert_eq!(m.find("world"), text.find("world"));
        assert!(!m.contains("smith2020"));
    }

    #[test]
    fn prose_inside_text_commands_is_kept() {
        let m = masked("A \\textbf{bold claim} and \\caption{a caption}.");
        assert!(m.contains("bold claim"), "{m:?}");
        assert!(m.contains("a caption"), "{m:?}");
    }

    #[test]
    fn math_comments_and_opaque_environments_are_blanked() {
        let m = masked("Text $x^2 + y$ more % a comment here\nAfter");
        assert!(m.contains("Text") && m.contains("more") && m.contains("After"));
        assert!(!m.contains("comment") && !m.contains("x^2"), "{m:?}");
        let e = masked("Before\n\\begin{align}\n  a &= b \\\\\n\\end{align}\nAfter");
        assert!(e.contains("Before") && e.contains("After"));
        assert!(!e.contains("a &= b"), "{e:?}");
        // Line structure is preserved, so a diagnostic lands on the right line.
        assert_eq!(e.lines().count(), 5);
    }

    #[test]
    fn a_line_and_column_come_back_from_an_offset() {
        let l = Lines::new("one\ntwo\nthree");
        assert_eq!(l.locate(0), Some((1, 0)));
        assert_eq!(l.locate(4), Some((2, 0)));
        assert_eq!(l.locate(9), Some((3, 1)));
    }

    #[test]
    fn form_values_are_encoded() {
        assert_eq!(form_encode("a b&c=d"), "a+b%26c%3Dd");
        assert_eq!(form_encode("café"), "caf%C3%A9");
    }

    /// The whole path. A masked document goes out. An offset comes back as a line, a source
    /// span and a one-click fix.
    #[tokio::test]
    async fn a_match_becomes_a_diagnostic_on_the_right_line() {
        let canned = r#"{"matches":[{"message":"Possible spelling mistake.","offset":38,"length":6,"replacements":[{"value":"result"}],"rule":{"id":"MORFOLOGIK_RULE_EN_US"}}]}"#;
        let app = axum::Router::new().route("/v2/check", axum::routing::post(move || async move {
            ([(axum::http::header::CONTENT_TYPE, "application/json")], canned)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // Offset 38 falls on "reslut" in line 2, after the masked citation on line 1.
        let text = "See \\cite{smith2020} for context.\nThe reslut is clear.\n";
        assert_eq!(mask(text).chars().skip(38).take(6).collect::<String>(), "reslut");
        let out = check_file(&base, "main.tex", text, "en-US").await.unwrap();
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert_eq!((d.line, d.level), (Some(2), Level::Lint));
        assert_eq!(d.code, "grammar/MORFOLOGIK_RULE_EN_US");
        assert_eq!(d.raw, "reslut");
        match d.fix.as_ref().unwrap() {
            Fix::Replace { file, line, find, text, .. } => {
                assert_eq!((file.as_str(), *line, find.as_str(), text.as_str()), ("main.tex", 2, "reslut", "result"));
            }
            other => panic!("expected a replacement, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_server_that_is_not_there_is_unavailable_not_an_error_card() {
        let e = check_file("http://127.0.0.1:1", "main.tex", "Some prose here.", "auto").await.unwrap_err();
        assert!(matches!(e, Error::Unreachable { .. }), "{e:?}");
    }

    #[test]
    fn the_setting_decides_the_endpoint() {
        assert!(matches!(endpoint("off"), Err(Error::Off)));
        assert!(matches!(endpoint(""), Err(Error::Off)));
        assert_eq!(endpoint("auto").unwrap(), LOCAL);
        assert_eq!(endpoint("http://lt.example.org:8010/").unwrap(), "http://lt.example.org:8010");
    }
}
