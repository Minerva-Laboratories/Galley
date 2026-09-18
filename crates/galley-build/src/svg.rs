//! SVG figures without shell escape. The `svg` package converts with Inkscape, which needs
//! `\write18`. With shell escape off, the package uses the files that are already converted into
//! `svg-inkscape/`. Galley makes that conversion itself with svg2pdf, in process and without
//! Inkscape, into `.galley/svg/svg-inkscape/`. Galley puts `.galley/svg` on the compile search
//! path, so `\includesvg` works without a change.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};
use svg2pdf::usvg;

use crate::log::{Diagnostic, Level};

/// The search-path root, relative to the project. Output lands in `<ROOT>/svg-inkscape/`.
pub const ROOT: &str = ".galley/svg";
const OUT: &str = "svg-inkscape";
const MANIFEST: &str = "manifest.json";
/// Do not parse a file larger than this. A figure is much smaller.
const MAX_SVG: u64 = 20 * 1024 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    /// A map from the output stem, the SVG file name without the extension, to the source path and
    /// the content hash.
    outputs: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    source: String,
    hash: String,
}

/// Fonts for the text inside an SVG. Galley uses the host fonts and maps the common generic names
/// to the fonts that are usually installed. The database loads once, because the scan of the system
/// fonts is slow.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    for (serif, sans, mono) in [
        ("DejaVu Serif", "DejaVu Sans", "DejaVu Sans Mono"),
        ("Liberation Serif", "Liberation Sans", "Liberation Mono"),
    ] {
        if db.faces().any(|f| f.families.iter().any(|(n, _)| n == serif)) {
            db.set_serif_family(serif);
            db.set_sans_serif_family(sans);
            db.set_monospace_family(mono);
            break;
        }
    }
    Arc::new(db)
});

/// Convert every changed `.svg` in the project. Returns the problems to show with the build.
pub fn prepare(project_dir: &Path) -> Vec<Diagnostic> {
    let root = project_dir.join(ROOT);
    let out_dir = root.join(OUT);
    let mut svgs = Vec::new();
    collect(project_dir, project_dir, &mut svgs);
    svgs.sort();
    if svgs.is_empty() {
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        return Vec::new();
    }
    if std::fs::create_dir_all(&out_dir).is_err() {
        return Vec::new();
    }
    let manifest_path = root.join(MANIFEST);
    let old: Manifest = std::fs::read(&manifest_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut new = Manifest::default();
    let mut problems = Vec::new();

    for rel in &svgs {
        let stem = Path::new(rel).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if stem.is_empty() {
            continue;
        }
        if let Some(first) = new.outputs.get(&stem) {
            // The package names outputs by file name alone, so two figures with the same name in
            // different folders would overwrite each other, as they do with Inkscape.
            problems.push(warn(rel, format!("{rel} has the same file name as {}; \\includesvg can only use one of them", first.source), "Rename one of the two SVG files."));
            continue;
        }
        let full = project_dir.join(rel);
        let Ok(bytes) = std::fs::metadata(&full).and_then(|m| {
            if m.len() > MAX_SVG {
                Err(std::io::Error::other("too large"))
            } else {
                std::fs::read(&full)
            }
        }) else {
            problems.push(warn(rel, format!("{rel} could not be read, or is larger than 20 MB"), "Simplify the drawing or export it as PDF."));
            continue;
        };
        let entry = Entry { source: rel.clone(), hash: hash(&bytes) };
        let files = [format!("{stem}_svg-tex.pdf"), format!("{stem}_svg-tex.pdf_tex"), format!("{stem}_svg-raw.pdf")];
        let current = old.outputs.get(&stem) == Some(&entry) && files.iter().all(|f| out_dir.join(f).is_file());
        if !current {
            match convert(&bytes) {
                Ok((pdf, w, h)) => {
                    let written = std::fs::write(out_dir.join(&files[0]), &pdf)
                        .and_then(|_| std::fs::write(out_dir.join(&files[1]), overlay(&files[0], w, h)))
                        .and_then(|_| std::fs::write(out_dir.join(&files[2]), &pdf));
                    if written.is_err() {
                        continue;
                    }
                }
                Err(why) => {
                    for f in &files {
                        let _ = std::fs::remove_file(out_dir.join(f));
                    }
                    problems.push(warn(rel, format!("{rel} could not be converted: {why}"), "Check that it opens in a browser, or export it as PDF and use \\includegraphics."));
                    continue;
                }
            }
        }
        new.outputs.insert(stem, entry);
    }

    // Remove the outputs whose SVG is gone. If they stay, a deleted figure still compiles.
    for stem in old.outputs.keys().filter(|s| !new.outputs.contains_key(*s)) {
        for suffix in ["_svg-tex.pdf", "_svg-tex.pdf_tex", "_svg-raw.pdf"] {
            let _ = std::fs::remove_file(out_dir.join(format!("{stem}{suffix}")));
        }
    }
    if let Ok(json) = serde_json::to_vec_pretty(&new) {
        let _ = std::fs::write(manifest_path, json);
    }
    problems
}

/// Convert SVG bytes into a one-page vector PDF and give its size in big points. The converter
/// never reads an external file. It uses only the images that are embedded as data URLs.
pub fn convert(bytes: &[u8]) -> Result<(Vec<u8>, f32, f32), String> {
    let mut opt = usvg::Options {
        fontdb: Arc::clone(&FONTS),
        ..usvg::Options::default()
    };
    opt.image_href_resolver = usvg::ImageHrefResolver {
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(|_, _| None),
    };
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| e.to_string())?;
    // The Inkscape convention is 96 user units to the inch.
    let page = svg2pdf::PageOptions { dpi: 96.0 };
    let pdf = svg2pdf::to_pdf(&tree, svg2pdf::ConversionOptions::default(), page).map_err(|e| e.to_string())?;
    let size = tree.size();
    Ok((pdf, size.width() * 72.0 / 96.0, size.height() * 72.0 / 96.0))
}

/// The `_tex` companion file that Inkscape writes. The text is already in the PDF, so this file
/// only places the page. The patches of the package supply the path and the requested width.
fn overlay(pdf_name: &str, width_bp: f32, height_bp: f32) -> String {
    let ratio = if width_bp > 0.0 { height_bp / width_bp } else { 1.0 };
    format!(
        "%% Written by Galley for the svg package (converted without Inkscape).\n\
\\begingroup%\n\
  \\makeatletter%\n\
  \\ifx\\svgwidth\\undefined%\n\
    \\setlength{{\\unitlength}}{{{width_bp:.4}bp}}%\n\
    \\ifx\\svgscale\\undefined%\n\
      \\relax%\n\
    \\else%\n\
      \\setlength{{\\unitlength}}{{\\unitlength * \\real{{\\svgscale}}}}%\n\
    \\fi%\n\
  \\else%\n\
    \\setlength{{\\unitlength}}{{\\svgwidth}}%\n\
  \\fi%\n\
  \\global\\let\\svgwidth\\undefined%\n\
  \\global\\let\\svgscale\\undefined%\n\
  \\makeatother%\n\
  \\begin{{picture}}(1,{ratio:.6})%\n\
    \\put(0,0){{\\includegraphics[width=\\unitlength,page=1]{{{pdf_name}}}}}%\n\
  \\end{{picture}}%\n\
\\endgroup%\n"
    )
}

fn warn(file: &str, message: String, hint: &str) -> Diagnostic {
    Diagnostic {
        level: Level::Warning,
        file: Some(file.to_string()),
        line: None,
        code: "svg-conversion".into(),
        message,
        hint: Some(hint.to_string()),
        explain: None,
        fix: None,
        raw: String::new(),
    }
}

fn hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}:{}", bytes.len())
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || name == ".galley" || name == OUT {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if name.to_ascii_lowercase().ends_with(".svg") {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="192" height="96" viewBox="0 0 192 96"><rect x="8" y="8" width="176" height="80" fill="teal"/></svg>"#;

    #[test]
    fn converts_and_sizes_at_96_dpi() {
        let (pdf, w, h) = convert(RECT.as_bytes()).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert!((w - 144.0).abs() < 0.01 && (h - 72.0).abs() < 0.01, "{w}x{h}");
        assert!(convert(b"not svg").is_err());
    }

    #[test]
    fn external_files_are_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.png");
        // A valid 1x1 PNG, so a resolver that read it would embed it.
        std::fs::write(&secret, [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 15, 4, 0, 9, 251, 3, 253, 227, 85, 242, 156, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130]).unwrap();
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{}"/></svg>"#, secret.display());
        let tree = {
            let mut opt = usvg::Options::default();
            opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
            usvg::Tree::from_data(svg.as_bytes(), &opt).unwrap()
        };
        assert!(!tree.root().has_children(), "the restricted resolver drops the image");
        let (pdf, _, _) = convert(svg.as_bytes()).unwrap();
        assert!(!pdf.windows(4).any(|w| w == b"/Image"), "no image embedded from disk");
    }

    #[test]
    fn prepare_writes_package_files_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("figs")).unwrap();
        std::fs::write(dir.path().join("figs/arch.svg"), RECT).unwrap();
        std::fs::write(dir.path().join("arch.svg"), RECT).unwrap();
        std::fs::write(dir.path().join("broken.svg"), "<svg").unwrap();
        let problems = prepare(dir.path());
        let out = dir.path().join(ROOT).join(OUT);
        for f in ["arch_svg-tex.pdf", "arch_svg-tex.pdf_tex", "arch_svg-raw.pdf"] {
            assert!(out.join(f).is_file(), "{f}");
        }
        let tex = std::fs::read_to_string(out.join("arch_svg-tex.pdf_tex")).unwrap();
        assert!(tex.contains("\\begin{picture}(1,0.5"), "{tex}");
        let codes: Vec<&str> = problems.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(problems.len(), 2, "{codes:?}");
        assert!(codes.iter().any(|m| m.contains("same file name")));
        assert!(codes.iter().any(|m| m.contains("broken.svg could not be converted")));

        std::fs::remove_file(dir.path().join("arch.svg")).unwrap();
        std::fs::remove_file(dir.path().join("figs/arch.svg")).unwrap();
        std::fs::write(dir.path().join("other.svg"), RECT).unwrap();
        prepare(dir.path());
        assert!(!out.join("arch_svg-tex.pdf").exists(), "outputs of deleted SVGs go");
        assert!(out.join("other_svg-raw.pdf").exists());
        std::fs::remove_file(dir.path().join("other.svg")).unwrap();
        std::fs::remove_file(dir.path().join("broken.svg")).unwrap();
        prepare(dir.path());
        assert!(!dir.path().join(ROOT).exists(), "no SVGs, no directory");
    }
}
