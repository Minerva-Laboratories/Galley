//! Bundled project templates (SPEC §9.4). A template is a name, a description, and a set of files.
//! Galley writes and commits the files when it creates the project. The templates are compiled
//! into the binary, so a fresh install has them offline and there is no directory to keep in sync.
//!
//! The templates use only free classes that Tectonic can fetch. A venue distributes its own class
//! files, such as `neurips.sty`. The paper templates therefore give the structure and say where to
//! swap the class in.

#[derive(Debug, Clone, serde::Serialize)]
pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub main_file: &'static str,
    /// Venue preset to apply to the new project, when the template implies one.
    pub venue: Option<&'static str>,
    #[serde(skip)]
    pub files: &'static [(&'static str, &'static str)],
}

macro_rules! file {
    ($dir:literal, $path:literal) => {
        ($path, include_str!(concat!("../../../templates/", $dir, "/", $path)))
    };
}

pub const BUNDLED: &[Template] = &[
    Template {
        id: "blank-article",
        name: "Blank article",
        description: "A main.tex with the article class, an abstract, and a bibliography file.",
        main_file: "main.tex",
        venue: None,
        files: &[file!("blank-article", "main.tex"), file!("blank-article", "refs.bib")],
    },
    Template {
        id: "conference-paper",
        name: "Conference paper",
        description: "Two columns, numbered sections, a results table and cross-references. Swap in the venue's class when you have it.",
        main_file: "main.tex",
        venue: None,
        files: &[file!("conference-paper", "main.tex"), file!("conference-paper", "refs.bib")],
    },
    Template {
        id: "preprint",
        name: "Preprint",
        description: "Single column with theorem environments and an appendix, the shape arXiv readers expect.",
        main_file: "main.tex",
        venue: Some("arxiv"),
        files: &[file!("preprint", "main.tex"), file!("preprint", "refs.bib")],
    },
    Template {
        id: "talk",
        name: "Talk",
        description: "Beamer slides at 16:9, one idea per frame.",
        main_file: "main.tex",
        venue: None,
        files: &[file!("talk", "main.tex")],
    },
    Template {
        id: "thesis",
        name: "Thesis or report",
        description: "Chapters in their own files, so two people can write at once without touching the same document.",
        main_file: "main.tex",
        venue: None,
        files: &[
            file!("thesis", "main.tex"),
            file!("thesis", "refs.bib"),
            file!("thesis", "chapters/introduction.tex"),
            file!("thesis", "chapters/background.tex"),
            file!("thesis", "chapters/conclusion.tex"),
        ],
    },
    Template {
        id: "rebuttal",
        name: "Response to reviewers",
        description: "Quote each point, answer it, and mark what changed in the paper.",
        main_file: "main.tex",
        venue: None,
        files: &[file!("rebuttal", "main.tex")],
    },
];

pub fn find(id: &str) -> Option<&'static Template> {
    BUNDLED.iter().find(|t| t.id == id)
}

/// The template used when a request does not name one.
pub fn default() -> &'static Template {
    &BUNDLED[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_has_its_main_file_and_a_document() {
        for t in BUNDLED {
            let main = t.files.iter().find(|(p, _)| *p == t.main_file);
            let (_, text) = main.unwrap_or_else(|| panic!("{} has no {}", t.id, t.main_file));
            assert!(text.contains("\\documentclass"), "{} is not a document", t.id);
            assert!(text.contains("\\end{document}"), "{} is unterminated", t.id);
            assert!(t.files.iter().all(|(p, _)| !p.starts_with('/') && !p.contains("..")), "{} escapes the project", t.id);
        }
    }

    #[test]
    fn a_templates_venue_is_one_we_bundle() {
        for t in BUNDLED.iter().filter_map(|t| t.venue) {
            assert!(crate::venues::find(t).is_some(), "unknown venue {t}");
        }
    }

    #[test]
    fn unknown_ids_fall_back_to_nothing_and_the_default_is_blank() {
        assert!(find("no-such-template").is_none());
        assert_eq!(default().id, "blank-article");
        assert_eq!(find("thesis").unwrap().files.len(), 5);
    }
}
