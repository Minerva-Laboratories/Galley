//! Structural search over the paper's mathematics.
//!
//! Text search cannot find an equation. `a^{*}(s) = -B^{+}(s - s^{*})` and the same relation written
//! with different letters share no words. So each formula is parsed into an operator tree. Every
//! subtree gets a **shape key**, the structure with symbol names replaced by placeholders. A query
//! matches when its shape appears anywhere in a formula's tree (Approach0's substructure idea,
//! docs/RETRIEVAL.md §6). No index on disk, because a paper holds tens of formulas.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::parse::Paper;

/// A formula found in the source, with where it lives.
#[derive(Debug, Clone, Serialize)]
pub struct Formula {
    /// The source as written, whitespace collapsed.
    pub source: String,
    pub label: Option<String>,
    pub file: String,
    pub line: u32,
    /// Index into `Paper::sections`.
    pub section: Option<usize>,
    /// Display maths (an environment or `\[…\]`), as opposed to inline `$…$`.
    pub display: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A named symbol: `x`, `\alpha`, `B`.
    Symbol(String),
    Number(String),
    /// An operator and its operands: `=`, `+`, `frac`, `sup`, `sub`, `sum`, `apply`.
    Op(String, Vec<Node>),
}

impl Node {
    /// The structure alone: every symbol becomes `?`, every number `#`. Two formulas with the same
    /// shape key say the same thing about different letters.
    pub fn shape(&self) -> String {
        match self {
            Node::Symbol(_) => "?".into(),
            Node::Number(_) => "#".into(),
            Node::Op(op, kids) => {
                let inner: Vec<String> = kids.iter().map(Node::shape).collect();
                format!("{op}({})", inner.join(","))
            }
        }
    }

    /// The structure with symbol names kept, for exact matches.
    pub fn exact(&self) -> String {
        match self {
            Node::Symbol(s) => s.clone(),
            Node::Number(n) => n.clone(),
            Node::Op(op, kids) => {
                let inner: Vec<String> = kids.iter().map(Node::exact).collect();
                format!("{op}({})", inner.join(","))
            }
        }
    }

    /// How many nodes. A bigger match is a better match.
    pub fn size(&self) -> usize {
        match self {
            Node::Op(_, kids) => 1 + kids.iter().map(Node::size).sum::<usize>(),
            _ => 1,
        }
    }

    fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        if let Node::Op(_, kids) = self {
            for k in kids {
                k.walk(out);
            }
        }
    }

    /// Every subtree, largest first.
    pub fn subtrees(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        self.walk(&mut out);
        out
    }
}

const DISPLAY_ENVS: &[&str] =
    &["equation", "align", "gather", "multline", "eqnarray", "flalign", "displaymath", "dmath"];
static DISPLAY_ENV: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    DISPLAY_ENVS
        .iter()
        .flat_map(|e| [e.to_string(), format!("{e}*")])
        .map(|e| {
            let e = regex::escape(&e);
            Regex::new(&format!(r"(?s)\\begin\{{{e}\}}(.*?)\\end\{{{e}\}}")).expect("regex")
        })
        .collect()
});
static BRACKET_DISPLAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\\\[(.*?)\\\]").unwrap());
static INLINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$([^$]{3,})\$").unwrap());
static LABEL_IN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\label\s*\{([^}]+)\}").unwrap());

/// Every formula in the project's `.tex` files, in reading order.
pub fn formulas(project_dir: &std::path::Path, paper: &Paper) -> Vec<Formula> {
    let mut out = Vec::new();
    for file in &paper.files {
        let Ok(text) = std::fs::read_to_string(project_dir.join(&file.path)) else { continue };
        let text = crate::parse::strip_comments_public(&text);
        let mut push = |body: &str, at: usize, display: bool| {
            let line = text[..at].lines().count() as u32;
            let label = LABEL_IN.captures(body).map(|c| c[1].trim().to_string());
            let source = LABEL_IN.replace_all(body, " ").split_whitespace().collect::<Vec<_>>().join(" ");
            if source.trim().is_empty() {
                return;
            }
            let section = paper
                .sections
                .iter()
                .enumerate()
                .rev()
                .find(|(_, s)| s.file == file.path && s.line <= line)
                .map(|(i, _)| i);
            out.push(Formula { source, label, file: file.path.clone(), line, section, display });
        };
        for re in DISPLAY_ENV.iter() {
            for m in re.captures_iter(&text) {
                push(&m[1], m.get(0).unwrap().start(), true);
            }
        }
        for m in BRACKET_DISPLAY.captures_iter(&text) {
            push(&m[1], m.get(0).unwrap().start(), true);
        }
        for m in INLINE.captures_iter(&text) {
            push(&m[1], m.get(0).unwrap().start(), false);
        }
    }
    out
}

/// Parse LaTeX maths into an operator tree. Unknown commands become symbols, so nothing is lost.
pub fn parse(source: &str) -> Node {
    let tokens = tokenize(source);
    let mut p = Parser { tokens, at: 0 };
    let node = p.relation();
    node.unwrap_or_else(|| Node::Symbol(String::new()))
}

fn tokenize(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '\\' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
            if i == start + 1 && i < chars.len() {
                i += 1; // an escaped symbol such as \{ or \,
            }
            out.push(chars[start..i].iter().collect());
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            // A letter or a single character of punctuation: one token either way.
            out.push(c.to_string());
            i += 1;
        }
    }
    out
}

struct Parser {
    tokens: Vec<String>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }
    fn next(&mut self) -> Option<String> {
        let t = self.tokens.get(self.at).cloned();
        if t.is_some() {
            self.at += 1;
        }
        t
    }
    fn eat(&mut self, what: &str) -> bool {
        if self.peek() == Some(what) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// `a = b`, `a \le b`, and so on. The loosest binding.
    fn relation(&mut self) -> Option<Node> {
        let mut left = self.sum()?;
        while let Some(op) = self.peek().and_then(relation_op) {
            self.at += 1;
            let right = self.sum().unwrap_or(Node::Symbol(String::new()));
            left = Node::Op(op, vec![left, right]);
        }
        Some(left)
    }

    /// `a + b - c`, flattened: `+` is n-ary, and `a - b` is `+(a, neg(b))`.
    fn sum(&mut self) -> Option<Node> {
        let mut terms = vec![self.product()?];
        loop {
            if self.eat("+") {
                terms.push(self.product().unwrap_or(Node::Symbol(String::new())));
            } else if self.eat("-") {
                let t = self.product().unwrap_or(Node::Symbol(String::new()));
                terms.push(Node::Op("neg".into(), vec![t]));
            } else {
                break;
            }
        }
        Some(if terms.len() == 1 { terms.remove(0) } else { Node::Op("+".into(), terms) })
    }

    /// Juxtaposition and `\cdot` multiply. `/` and `\frac` divide.
    fn product(&mut self) -> Option<Node> {
        let mut factors = vec![self.unary()?];
        loop {
            if self.eat("\\cdot") || self.eat("\\times") || self.eat("*") {
                factors.push(self.unary().unwrap_or(Node::Symbol(String::new())));
            } else if self.eat("/") {
                // Everything so far is the numerator, everything up to the next +, - or relation
                // is the denominator. `\partial a / \partial s` is one derivative, not three factors.
                let num = match factors.len() {
                    1 => factors.remove(0),
                    _ => Node::Op("*".into(), std::mem::take(&mut factors)),
                };
                let mut denom = vec![self.unary().unwrap_or(Node::Symbol(String::new()))];
                while self.starts_factor() {
                    denom.push(self.unary().unwrap_or(Node::Symbol(String::new())));
                }
                let denom = if denom.len() == 1 { denom.remove(0) } else { Node::Op("*".into(), denom) };
                factors.push(Node::Op("frac".into(), vec![num, denom]));
            } else if self.starts_factor() {
                factors.push(self.unary().unwrap_or(Node::Symbol(String::new())));
            } else {
                break;
            }
        }
        Some(if factors.len() == 1 { factors.remove(0) } else { Node::Op("*".into(), factors) })
    }

    fn starts_factor(&self) -> bool {
        match self.peek() {
            None => false,
            Some(t) => {
                !matches!(t, "+" | "-" | ")" | "}" | "]" | "&" | "\\\\" | "/" | "," | ";")
                    && relation_op(t).is_none()
                    && !t.starts_with("\\end")
                    && !t.starts_with("\\right")
            }
        }
    }

    /// A leading `-`, then powers and subscripts.
    fn unary(&mut self) -> Option<Node> {
        if self.eat("-") {
            return Some(Node::Op("neg".into(), vec![self.unary()?]));
        }
        let mut base = self.atom()?;
        loop {
            if self.eat("^") {
                base = Node::Op("sup".into(), vec![base, self.atom().unwrap_or(Node::Symbol(String::new()))]);
            } else if self.eat("_") {
                base = Node::Op("sub".into(), vec![base, self.atom().unwrap_or(Node::Symbol(String::new()))]);
            } else if self.peek() == Some("(") {
                // `f(x)` is application, `(a+b)` is a group, decided by what precedes it.
                let is_apply = matches!(base, Node::Symbol(_) | Node::Op(..));
                if !is_apply {
                    break;
                }
                self.at += 1;
                let arg = self.relation().unwrap_or(Node::Symbol(String::new()));
                self.eat(")");
                base = Node::Op("apply".into(), vec![base, arg]);
            } else {
                break;
            }
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<Node> {
        let token = self.next()?;
        Some(match token.as_str() {
            "{" | "(" | "[" => {
                let inner = self.relation().unwrap_or(Node::Symbol(String::new()));
                self.eat("}");
                self.eat(")");
                self.eat("]");
                inner
            }
            "\\frac" | "\\dfrac" | "\\tfrac" => {
                let a = self.atom().unwrap_or(Node::Symbol(String::new()));
                let b = self.atom().unwrap_or(Node::Symbol(String::new()));
                Node::Op("frac".into(), vec![a, b])
            }
            "\\sqrt" => Node::Op("sqrt".into(), vec![self.atom().unwrap_or(Node::Symbol(String::new()))]),
            "\\sum" | "\\prod" | "\\int" | "\\lim" | "\\max" | "\\min" | "\\arg" => {
                let name = token.trim_start_matches('\\').to_string();
                let mut kids = Vec::new();
                while self.peek() == Some("_") || self.peek() == Some("^") {
                    self.at += 1;
                    kids.push(self.atom().unwrap_or(Node::Symbol(String::new())));
                }
                if self.starts_factor() {
                    kids.push(self.product().unwrap_or(Node::Symbol(String::new())));
                }
                Node::Op(name, kids)
            }
            "\\left" | "\\right" | "\\big" | "\\Big" | "\\bigg" | "\\mathrm" | "\\mathbf" | "\\mathcal" | "\\text"
            | "\\operatorname" | "\\," | "\\;" | "\\!" | "\\quad" | "\\qquad" => {
                self.atom().unwrap_or(Node::Symbol(String::new()))
            }
            t if t.chars().next().is_some_and(|c| c.is_ascii_digit()) => Node::Number(t.to_string()),
            t => Node::Symbol(normalise_symbol(t)),
        })
    }
}

/// `\partial` and `∂` are the same symbol. So are `\mathbf{x}` and `x` once the wrapper is gone.
fn normalise_symbol(token: &str) -> String {
    match token {
        "\\partial" => "∂".into(),
        "\\nabla" => "∇".into(),
        "\\infty" => "∞".into(),
        "\\ast" | "\\star" => "*".into(),
        t => t.trim_start_matches('\\').to_string(),
    }
}

fn relation_op(token: &str) -> Option<String> {
    Some(match token {
        "=" => "=".into(),
        "\\ne" | "\\neq" => "≠".into(),
        "<" | "\\lt" => "<".into(),
        ">" | "\\gt" => ">".into(),
        "\\le" | "\\leq" => "≤".into(),
        "\\ge" | "\\geq" => "≥".into(),
        "\\approx" => "≈".into(),
        "\\equiv" => "≡".into(),
        "\\propto" => "∝".into(),
        "\\sim" => "∼".into(),
        "\\to" | "\\rightarrow" => "→".into(),
        "\\in" => "∈".into(),
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Match {
    /// Index into the formula list.
    pub formula: usize,
    /// How much of the formula the query matched, in nodes.
    pub matched: usize,
    /// True when the symbols matched too, not only the shape.
    pub exact: bool,
}

/// Formulas containing the query's structure, best first.
pub fn search(formulas: &[Formula], query: &str, limit: usize) -> Vec<Match> {
    let wanted = parse(query);
    let want_shape = wanted.shape();
    let want_exact = wanted.exact();
    // Shape matching needs a shape. `B^{+}` as a pattern would match every superscript in the
    // paper, so small queries match literally and only larger ones generalise over symbols.
    let by_shape = wanted.size() >= 5;
    let mut hits: Vec<Match> = Vec::new();
    for (i, f) in formulas.iter().enumerate() {
        let tree = parse(&f.source);
        let mut best: Option<(usize, bool)> = None;
        for sub in tree.subtrees() {
            let exact = sub.exact() == want_exact;
            let shaped = by_shape && sub.shape() == want_shape;
            if !exact && !shaped {
                continue;
            }
            let score = (sub.size(), exact);
            if best.is_none_or(|b| (score.1, score.0) > (b.1, b.0)) {
                best = Some(score);
            }
        }
        if let Some((matched, exact)) = best {
            hits.push(Match { formula: i, matched, exact });
        }
    }
    hits.sort_by(|a, b| {
        b.exact.cmp(&a.exact).then(b.matched.cmp(&a.matched)).then(a.formula.cmp(&b.formula))
    });
    hits.truncate(limit);
    hits
}

/// Which symbols the paper's mathematics uses, and how often. This is the map for maths.
pub fn symbols(formulas: &[Formula]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in formulas {
        for node in parse(&f.source).subtrees() {
            if let Node::Symbol(s) = node {
                // Letters and the named symbols of mathematics. Not the punctuation that survives
                // from `^{*}` or `\|`.
                if s.chars().next().is_some_and(|c| c.is_alphanumeric() || "∂∇∞θλμσΣΩ".contains(c)) {
                    *counts.entry(s.clone()).or_default() += 1;
                }
            }
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// A formula in one line. Enough to choose from, and small enough to save context.
fn one_line(f: &Formula, paper: &Paper, width: usize) -> String {
    let where_ = match f.section.and_then(|s| paper.sections.get(s)) {
        Some(s) => format!("§{}", s.number),
        None => f.file.clone(),
    };
    let name = f.label.clone().unwrap_or_else(|| format!("{}:{}", f.file, f.line));
    let source: String = if f.source.chars().count() > width {
        format!("{}…", f.source.chars().take(width - 1).collect::<String>())
    } else {
        f.source.clone()
    };
    format!("{name} · {where_} · {source}")
}

/// Search results: one line each, with the full source only when asked for.
pub fn render_matches(paper: &Paper, formulas: &[Formula], query: &str, hits: &[Match], detail: bool) -> String {
    if hits.is_empty() {
        return format!(
            "No formula contains {query:?}. The paper has {} formula(s); call math with no query to see the symbols it uses.\n",
            formulas.len()
        );
    }
    let mut out = format!("{} formula(s) matching {query:?}:\n", hits.len());
    for h in hits {
        let f = &formulas[h.formula];
        let how = if h.exact { "exact" } else { "same shape" };
        if detail {
            let section = f.section.and_then(|s| paper.sections.get(s)).map(|s| s.title.clone()).unwrap_or_default();
            out.push_str(&format!(
                "\n{} · {}:{} · {section} · {how}, {} node(s) matched\n  {}\n",
                f.label.clone().unwrap_or_else(|| "(unlabelled)".into()),
                f.file,
                f.line,
                h.matched,
                f.source
            ));
        } else {
            out.push_str(&format!("  {} · {how}\n", one_line(f, paper, 64)));
        }
    }
    if !detail {
        out.push_str("Ask again with detail for the full source and the file positions.\n");
    }
    out
}

/// What the paper's mathematics is made of: how many formulas, and the symbols it uses most.
pub fn render_overview(paper: &Paper, formulas: &[Formula]) -> String {
    if formulas.is_empty() {
        return "This project has no mathematics.\n".to_string();
    }
    let display = formulas.iter().filter(|f| f.display).count();
    let labelled = formulas.iter().filter(|f| f.label.is_some()).count();
    let census = symbols(formulas);
    let top: Vec<String> = census.iter().take(14).map(|(s, n)| format!("{s}×{n}")).collect();
    let mut out = format!(
        "{} formula(s): {display} displayed ({labelled} labelled), {} inline\nsymbols: {}\n",
        formulas.len(),
        formulas.len() - display,
        top.join(", ")
    );
    out.push_str("labelled formulas:\n");
    for f in formulas.iter().filter(|f| f.label.is_some()).take(12) {
        out.push_str(&format!("  {}\n", one_line(f, paper, 56)));
    }
    out.push_str("Search with an expression to find formulas of the same shape.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relations_powers_and_fractions() {
        assert_eq!(parse("a = b").exact(), "=(a,b)");
        assert_eq!(parse("x^2 + y^2 = z^2").exact(), "=(+(sup(x,2),sup(y,2)),sup(z,2))");
        assert_eq!(parse("\\frac{a}{b}").exact(), "frac(a,b)");
        assert_eq!(parse("a/b").exact(), "frac(a,b)");
        // \partial a / \partial s is a fraction of two products.
        assert_eq!(parse("\\partial a / \\partial s").exact(), "frac(*(∂,a),*(∂,s))");
        assert_eq!(parse("-B^{+}").exact(), "neg(sup(B,+))");
        assert_eq!(parse("f(x)").exact(), "apply(f,x)");
        assert_eq!(parse("\\sum_{i=1}^{n} x_i").exact(), "sum(=(i,1),n,sub(x,i))");
    }

    #[test]
    fn shape_ignores_the_letters_but_not_the_structure() {
        assert_eq!(parse("a^2 + b^2").shape(), parse("p^2 + q^2").shape());
        assert_ne!(parse("a^2 + b^2").shape(), parse("a^2 - b^2").shape());
        assert_ne!(parse("a + b").shape(), parse("a * b").shape());
        // Wrappers and spacing do not change anything.
        assert_eq!(parse("\\mathbf{x} = \\left( y \\right)").exact(), parse("x = (y)").exact());
    }

    fn formula(source: &str) -> Formula {
        Formula { source: source.into(), label: None, file: "main.tex".into(), line: 1, section: None, display: true }
    }

    #[test]
    fn finds_the_same_shape_with_other_symbols_and_prefers_exact() {
        let fs = vec![
            formula("a^{*}(s) = -B^{+}(s - s^{*})"),
            formula("u^{*}(x) = -K^{+}(x - x^{*})"),
            formula("E = mc^2"),
            formula("\\partial a / \\partial s = -B^{+}"),
        ];
        let hits = search(&fs, "a^{*}(s) = -B^{+}(s - s^{*})", 5);
        assert_eq!(hits.len(), 2, "the two with this shape, not the others: {hits:?}");
        assert_eq!(hits[0].formula, 0);
        assert!(hits[0].exact, "the identical formula first");
        assert_eq!(hits[1].formula, 1);
        assert!(!hits[1].exact, "same shape, different letters");

        // A subexpression matches inside a larger formula. `-B^{+}` is a subtree of formula 3 only.
        // In formula 0 the minus negates the whole application, so `B^{+}` is what sits inside it.
        let negated = search(&fs, "-B^{+}", 5);
        assert_eq!(negated.iter().filter(|h| h.exact).map(|h| h.formula).collect::<Vec<_>>(), vec![3]);
        let power = search(&fs, "B^{+}", 5);
        assert_eq!(power.iter().filter(|h| h.exact).map(|h| h.formula).collect::<Vec<_>>(), vec![0, 3]);
        // Too small to generalise: `K^{+}` in formula 1 is not offered as "the same shape", or every
        // superscript in the paper would be.
        assert!(power.iter().all(|h| h.exact), "{power:?}");

        // A lone symbol matches only where that symbol appears.
        let single = search(&fs, "m", 5);
        assert_eq!(single.iter().map(|h| h.formula).collect::<Vec<_>>(), vec![2]);

        // A small query stays literal: `B^{+}` must not match every superscript in the paper.
        let small = search(&fs, "B^{+}", 5);
        assert!(small.iter().all(|h| h.exact), "{small:?}");
        assert!(!small.iter().any(|h| h.formula == 2), "E = mc^2 is not a pseudoinverse");
    }

    #[test]
    fn extracts_display_and_inline_maths_with_labels() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tex"),
            "\\section{One}\nText with $x + y$ inline.\n\\begin{equation}\\label{eq:key}a = b\\end{equation}\n% \\begin{equation} commented \\end{equation}\n\\[ c = d \\]\n",
        )
        .unwrap();
        let paper = crate::parse::scan(dir.path(), "main.tex");
        let fs = formulas(dir.path(), &paper);
        let sources: Vec<&str> = fs.iter().map(|f| f.source.as_str()).collect();
        assert!(sources.contains(&"a = b"), "{sources:?}");
        assert!(sources.contains(&"c = d"), "{sources:?}");
        assert!(sources.contains(&"x + y"), "{sources:?}");
        assert_eq!(fs.iter().filter(|f| f.label.as_deref() == Some("eq:key")).count(), 1);
        assert!(!sources.iter().any(|s| s.contains("commented")), "comments are not maths");
        assert!(fs.iter().all(|f| f.section == Some(0)));
        let inline = fs.iter().find(|f| f.source == "x + y").unwrap();
        assert!(!inline.display);
    }

    #[test]
    fn symbol_census_counts_what_the_paper_uses() {
        let fs = vec![formula("a = b + a"), formula("\\partial a / \\partial s")];
        let census = symbols(&fs);
        assert_eq!(census[0], ("a".to_string(), 3));
        assert!(census.iter().any(|(s, n)| s == "∂" && *n == 2));
        // Punctuation left over from `^{*}` or `\|` is not a symbol.
        let noisy = vec![formula("\\| a^{*} \\|")];
        assert_eq!(symbols(&noisy).iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), vec!["a"]);
    }
}
