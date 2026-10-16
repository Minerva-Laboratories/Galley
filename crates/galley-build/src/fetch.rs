//! Filling Tectonic's package cache without running the author's document online. A compile runs
//! offline. When Tectonic reports files its cache lacks, Galley writes a small document of its own
//! that loads just those files, compiles that one with network, and compiles the author's document
//! offline again. Only names that look like plain file names reach that small document, so nothing
//! the author wrote is executed while the network is on.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Instant;

use regex::Regex;

use crate::Result;
use crate::engine::CompileSpec;
use crate::log::{Level, RawDiag};
use crate::runner::{Builder, ResolvedEngine, RunOutput, cache_cold, needs_fetch};

/// Fetch rounds one compile may trigger. Each round can reveal the files the last one loaded.
const ROUNDS: usize = 3;
/// Names one fetch document may ask for, so a document listing hundreds of fake packages cannot
/// keep a worker busy.
const MAX_NAMES: usize = 64;

static NOT_FOUND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"File `([^']+)' not found").unwrap());
static TIKZ_LIBRARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"did not find the tikz library '([^']+)'").unwrap());
static FONT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"=([A-Za-z0-9._-]+)(?: at [0-9.]+pt)? not loadable").unwrap());
/// xdvipdfmx, writing the PDF, needs a font's outline file as well as its metrics. A cache that
/// has only the metrics fails here, after TeX itself succeeded.
static PDF_FONT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"Could not locate a virtual/physical font for TFM "([A-Za-z0-9._-]+)""#).unwrap());
static CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\documentclass\s*(?:\[[^\]]*\])?\s*\{([^}]+)\}").unwrap());
static PACKAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{([^}]+)\}").unwrap()
});
static USE_TIKZ_LIBRARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\usetikzlibrary\s*\{([^}]+)\}").unwrap());

/// A file name that can go into the fetch document as it is: letters, digits, `-`, `_` and `.`,
/// starting with a letter or digit. No braces, backslashes or spaces, so no TeX can ride along.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What a fetch document asks Tectonic to load.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FetchList {
    class: Option<String>,
    packages: Vec<String>,
    tikz: Vec<String>,
    fonts: Vec<String>,
    files: Vec<String>,
}

impl FetchList {
    /// The class and packages a document names. Used when the project has no cache yet, so the
    /// first offline compile already finds most of what it needs.
    pub(crate) fn from_source(text: &str) -> FetchList {
        let mut list = FetchList {
            class: CLASS.captures(text).map(|c| c[1].trim().to_string()).filter(|n| plain(n)),
            ..FetchList::default()
        };
        for c in PACKAGE.captures_iter(text) {
            for name in c[1].split(',') {
                list.add_package(name.trim());
            }
        }
        for c in USE_TIKZ_LIBRARY.captures_iter(text) {
            for name in c[1].split(',') {
                list.add(name.trim(), |l| &mut l.tikz);
            }
        }
        list
    }

    /// The files a failed offline compile reported missing from the cache. A file the project
    /// holds itself is not fetched.
    pub(crate) fn from_diagnostics(raw: &[RawDiag], project_dir: &Path) -> FetchList {
        let mut list = FetchList::default();
        for d in raw.iter().filter(|d| d.level == Level::Error) {
            for text in [&d.message, &d.raw] {
                if let Some(c) = NOT_FOUND.captures(text) {
                    let name = c[1].trim();
                    if !project_dir.join(name).exists() {
                        match name.strip_suffix(".sty") {
                            Some(package) => list.add_package(package),
                            None => list.add(name, |l| &mut l.files),
                        }
                    }
                }
                if let Some(c) = TIKZ_LIBRARY.captures(text) {
                    list.add(c[1].trim(), |l| &mut l.tikz);
                }
                if let Some(c) = FONT.captures(text) {
                    list.add(c[1].trim(), |l| &mut l.fonts);
                }
            }
        }
        list
    }

    /// The fonts xdvipdfmx could not find while writing the PDF. They show only in Tectonic's
    /// output, not in the log.
    pub(crate) fn from_stderr(stderr: &str) -> FetchList {
        let mut list = FetchList::default();
        for c in PDF_FONT.captures_iter(stderr) {
            list.add(c[1].trim(), |l| &mut l.fonts);
        }
        list
    }

    /// Both lists, without repeats.
    fn merge(mut self, other: FetchList) -> FetchList {
        if self.class.is_none() {
            self.class = other.class;
        }
        for n in other.packages {
            self.add(&n, |l| &mut l.packages);
        }
        for n in other.tikz {
            self.add(&n, |l| &mut l.tikz);
        }
        for n in other.fonts {
            self.add(&n, |l| &mut l.fonts);
        }
        for n in other.files {
            self.add(&n, |l| &mut l.files);
        }
        self
    }

    fn add_package(&mut self, name: &str) {
        self.add(name.strip_suffix(".sty").unwrap_or(name), |l| &mut l.packages);
    }

    fn add(&mut self, name: &str, field: impl Fn(&mut FetchList) -> &mut Vec<String>) {
        if plain(name) && self.len() < MAX_NAMES && !field(self).iter().any(|n| n == name) {
            field(self).push(name.to_string());
        }
    }

    fn len(&self) -> usize {
        usize::from(self.class.is_some()) + self.packages.len() + self.tikz.len() + self.fonts.len() + self.files.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every name in the list, tagged by kind so the same word in two kinds stays distinct.
    fn names(&self) -> Vec<String> {
        let tag = |kind: &str, v: &[String]| v.iter().map(|n| format!("{kind}:{n}")).collect::<Vec<_>>();
        let mut out: Vec<String> = self.class.iter().map(|c| format!("class:{c}")).collect();
        out.extend(tag("package", &self.packages));
        out.extend(tag("tikz", &self.tikz));
        out.extend(tag("font", &self.fonts));
        out.extend(tag("file", &self.files));
        out
    }

    /// Drop what an earlier round already asked for, so a round never repeats itself.
    fn without(mut self, asked: &BTreeSet<String>) -> FetchList {
        if self.class.as_ref().is_some_and(|c| asked.contains(&format!("class:{c}"))) {
            self.class = None;
        }
        self.packages.retain(|n| !asked.contains(&format!("package:{n}")));
        self.tikz.retain(|n| !asked.contains(&format!("tikz:{n}")));
        self.fonts.retain(|n| !asked.contains(&format!("font:{n}")));
        self.files.retain(|n| !asked.contains(&format!("file:{n}")));
        self
    }

    /// The fetch document. Loading a package also loads what it needs, so one round usually brings
    /// a package with its dependencies.
    pub(crate) fn source(&self) -> String {
        let mut tex = format!("\\documentclass{{{}}}\n", self.class.as_deref().unwrap_or("article"));
        for p in &self.packages {
            tex.push_str(&format!("\\usepackage{{{p}}}\n"));
        }
        if !self.tikz.is_empty() {
            tex.push_str(&format!("\\usepackage{{tikz}}\n\\usetikzlibrary{{{}}}\n", self.tikz.join(",")));
        }
        for f in &self.files {
            tex.push_str(&format!("\\IfFileExists{{{f}}}{{}}{{}}\n"));
        }
        tex.push_str("\\begin{document}\nx\n");
        // A font is typeset as well as loaded, so the PDF step fetches its outline file too.
        // Every slot is tried; the ones a font lacks are skipped by TeX.
        for f in &self.fonts {
            tex.push_str(&format!(
                "{{\\font\\galleyfetch={f}\\relax\\galleyfetch\\count255=0 \\loop\\char\\count255 \\advance\\count255 1 \\ifnum\\count255<128 \\repeat}}\n"
            ));
        }
        tex.push_str("\\end{document}\n");
        tex
    }
}

impl Builder {
    /// Compile offline with `make`. When Tectonic's cache lacks files, fetch them with a document
    /// of Galley's own and compile offline again. `first` is an offline run that already happened,
    /// if any. Returns the last run and whether anything was fetched.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn compile_fetching(
        &self,
        engine: &ResolvedEngine,
        project_dir: &Path,
        main_text: &str,
        make: &(dyn Fn() -> Result<CompileSpec> + Send + Sync),
        first: Option<RunOutput>,
        deadline: Option<Instant>,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(RunOutput, bool)> {
        // Fetching never stretches a compile past twice its own limit.
        let deadline = deadline.unwrap_or_else(|| Instant::now() + self.timeout * 2);
        let limit = || deadline.saturating_duration_since(Instant::now());
        let ResolvedEngine::Tectonic(tectonic) = engine else {
            let run = match first {
                Some(run) => run,
                None => self.run_spec_limited(make()?, limit()).await?,
            };
            return Ok((run, false));
        };
        let mut asked = BTreeSet::new();
        let mut fetched = false;
        let mut run = match first {
            Some(run) => run,
            None => {
                if cache_cold(tectonic, project_dir) {
                    let list = FetchList::from_source(main_text);
                    asked.extend(list.names());
                    self.fetch(tectonic, project_dir, &list, limit()).await?;
                    fetched = true;
                }
                self.run_spec_limited(make()?, limit()).await?
            }
        };
        for _ in 0..ROUNDS {
            let pdf_fonts = if run.exit_ok { FetchList::default() } else { FetchList::from_stderr(&run.stderr) };
            if run.timed_out || (!needs_fetch(&run.raw, project_dir) && pdf_fonts.is_empty()) {
                break;
            }
            let list = FetchList::from_diagnostics(&run.raw, project_dir).merge(pdf_fonts).without(&asked);
            if list.is_empty() {
                break;
            }
            asked.extend(list.names());
            progress("Fetching packages…".into());
            self.fetch(tectonic, project_dir, &list, limit()).await?;
            fetched = true;
            run = self.run_spec_limited(make()?, limit()).await?;
        }
        Ok((run, fetched))
    }

    async fn fetch(
        &self,
        tectonic: &crate::engine::Tectonic,
        project_dir: &Path,
        list: &FetchList,
        limit: std::time::Duration,
    ) -> Result<()> {
        let spec = tectonic.fetch(&self.sandbox, project_dir, &list.source())?;
        let run = self.run_spec_limited(spec, limit).await?;
        tracing::debug!(names = list.len(), ok = run.exit_ok, timed_out = run.timed_out, "package fetch");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(message: &str) -> RawDiag {
        RawDiag {
            level: Level::Error,
            file: None,
            line: None,
            message: message.into(),
            context: String::new(),
            raw: String::new(),
        }
    }

    #[test]
    fn names_from_a_failed_compile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("local.sty"), "").unwrap();
        let list = FetchList::from_diagnostics(
            &[
                error("LaTeX Error: File `tikz.sty' not found"),
                error("LaTeX Error: File `size11.clo' not found"),
                error("LaTeX Error: File `local.sty' not found"),
                error("Package tikz Error: I did not find the tikz library 'calc'."),
                error("Font \\OT1/cmr/m/it/10.95=cmti10 at 10.95pt not loadable: Metric (TFM) file not found."),
            ],
            dir.path(),
        );
        assert_eq!(list.packages, vec!["tikz"]);
        assert_eq!(list.files, vec!["size11.clo"]);
        assert_eq!(list.tikz, vec!["calc"]);
        assert_eq!(list.fonts, vec!["cmti10"]);
    }

    #[test]
    fn fonts_missing_when_writing_the_pdf_are_fetched_and_typeset() {
        let stderr = "warning: Could not locate a virtual/physical font for TFM \"lasy6\".\n\
                      warning: Could not locate a virtual/physical font for TFM \"x}\\input{y\".\n";
        let list = FetchList::from_stderr(stderr);
        assert_eq!(list.fonts, vec!["lasy6"]);
        let tex = list.source();
        let body = tex.split("\\begin{document}").nth(1).unwrap();
        assert!(body.contains("\\font\\galleyfetch=lasy6\\relax\\galleyfetch"), "{tex}");
        assert!(!tex.contains("\\input"), "{tex}");
    }

    #[test]
    fn crafted_names_never_reach_the_fetch_document() {
        let list = FetchList::from_diagnostics(
            &[
                error("LaTeX Error: File `x}\\input{/etc/passwd' not found"),
                error("LaTeX Error: File `a b.sty' not found"),
                error("LaTeX Error: File `../up.sty' not found"),
                error("Package tikz Error: I did not find the tikz library 'x}\\def'."),
            ],
            Path::new("/nonexistent"),
        );
        assert!(list.is_empty(), "{list:?}");
        let list = FetchList::from_source("\\documentclass{x y}\\input{y}\n\\usepackage{ok,\\bad,no space}");
        assert_eq!(list.class, None);
        assert_eq!(list.packages, vec!["ok"]);
        let source = list.source();
        assert!(!source.contains("\\input") && !source.contains("\\bad"), "{source}");
    }

    #[test]
    fn the_fetch_document_loads_each_kind() {
        let list = FetchList::from_source(
            "\\documentclass[11pt]{article}\n\\usepackage[utf8]{inputenc}\n\\usepackage{amsmath, graphicx}\n\\usetikzlibrary{calc,arrows.meta}\n",
        );
        let tex = list.source();
        assert!(tex.starts_with("\\documentclass{article}\n"));
        assert!(tex.contains("\\usepackage{inputenc}\n\\usepackage{amsmath}\n\\usepackage{graphicx}\n"));
        assert!(tex.contains("\\usetikzlibrary{calc,arrows.meta}"));
        assert!(tex.ends_with("\\end{document}\n"));
    }

    #[test]
    fn a_long_list_is_capped_and_rounds_do_not_repeat() {
        let text: String = (0..500).map(|i| format!("\\usepackage{{fake{i}}}\n")).collect();
        let list = FetchList::from_source(&text);
        assert_eq!(list.len(), MAX_NAMES);
        let asked: BTreeSet<String> = list.names().into_iter().collect();
        assert!(FetchList::from_source(&text).without(&asked).is_empty());
    }
}
