//! SyncTeX in both directions. It maps a source line to a page position, and a page position to a
//! source line. It parses the `.synctex.gz` file that the engine writes. The coordinates are in PDF
//! points from the top-left corner.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use serde::Serialize;

use crate::{Error, Result};

const SP_PER_PT: f64 = 65536.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Record {
    page: u32,
    file: u32,
    line: u32,
    /// Position and size in sp. `h` and `v` are the reference point, the baseline left for a box.
    h: i64,
    v: i64,
    width: i64,
    height: i64,
    depth: i64,
    /// A box has an extent. A point record such as a kern, glue or the current position has none.
    is_box: bool,
    /// An opening `[` or `(`: a box that encloses other material. TeX can attribute such a box to a
    /// line that typeset nothing itself, such as `\\begin{document}`, and place it on any page.
    container: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PdfLocation {
    pub page: u32,
    /// Points from the page's left edge.
    pub x: f64,
    /// Points from the page's top edge.
    pub y: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
}

pub struct SyncTex {
    inputs: HashMap<u32, String>,
    records: Vec<Record>,
    scale: f64,
}

impl SyncTex {
    pub fn load(path: &Path, project_dir: &Path) -> Result<SyncTex> {
        let bytes = std::fs::read(path)?;
        let text = if path.extension().is_some_and(|e| e == "gz") {
            let mut s = String::new();
            flate2::read::GzDecoder::new(&bytes[..]).read_to_string(&mut s)?;
            s
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        SyncTex::parse(&text, project_dir)
    }

    /// The parser strips `project_dir` and the sandbox path `/work` from the input paths, so the
    /// caller sees project-relative files.
    pub fn parse(text: &str, project_dir: &Path) -> Result<SyncTex> {
        let mut inputs = HashMap::new();
        let mut records = Vec::new();
        let mut unit = 1.0f64;
        let mut magnification = 1000.0f64;
        let mut page = 0u32;
        let prefixes = [project_dir.to_string_lossy().into_owned(), "/work".to_string()];

        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("Input:") {
                let mut parts = rest.splitn(2, ':');
                let id: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| Error::SyncTex(line.into()))?;
                let raw = parts.next().unwrap_or("").trim();
                if raw.is_empty() {
                    continue;
                }
                let mut name = raw.to_string();
                for p in &prefixes {
                    if let Some(stripped) = name.strip_prefix(p) {
                        name = stripped.trim_start_matches('/').to_string();
                        break;
                    }
                }
                inputs.insert(id, name.trim_start_matches("./").to_string());
                continue;
            }
            if let Some(v) = line.strip_prefix("Unit:") {
                unit = v.trim().parse().unwrap_or(1.0);
                continue;
            }
            if let Some(v) = line.strip_prefix("Magnification:") {
                magnification = v.trim().parse().unwrap_or(1000.0);
                continue;
            }
            let Some(kind) = line.chars().next() else { continue };
            match kind {
                '{' => page = line[1..].trim().parse().unwrap_or(page + 1),
                '}' | ']' | ')' | '!' => {}
                '[' | '(' | 'h' | 'v' | '$' | 'r' | 'x' | 'k' | 'g' | 'f' => {
                    if let Some(r) = parse_record(&line[1..], page, matches!(kind, '[' | '(' | 'h' | 'v' | '$' | 'r'), matches!(kind, '[' | '(')) {
                        records.push(r);
                    }
                }
                _ => {}
            }
        }
        Ok(SyncTex {
            inputs,
            records,
            scale: unit * magnification / 1000.0 / SP_PER_PT,
        })
    }

    pub fn inputs(&self) -> impl Iterator<Item = &str> {
        self.inputs.values().map(String::as_str)
    }

    /// Where a source line ended up. Prefers the first box on that line.
    pub fn forward(&self, file: &str, line: u32) -> Option<PdfLocation> {
        let file = file.trim_start_matches("./");
        let ids: Vec<u32> = self
            .inputs
            .iter()
            .filter(|(_, name)| *name == file || name.strip_suffix(".tex") == Some(file) || file.strip_suffix(".tex") == Some(name))
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return None;
        }
        // A line counts only if it typeset something itself: glyphs, glue, kerns, math, a rule or an
        // empty box. The preamble typesets nothing, and the nearest following line with any record
        // can be \\begin{document}, whose only record is an enclosing box that TeX may place on the
        // last page. Skipping such lines sends a click on \\usepackage or \\author to the title block.
        let content_lines: std::collections::HashSet<u32> =
            self.records.iter().filter(|r| ids.contains(&r.file) && !r.container).map(|r| r.line).collect();
        // Take the exact line first, then the nearest line after it. A blank line has no record.
        let mut best: Option<&Record> = None;
        for r in &self.records {
            if !ids.contains(&r.file) || r.line < line {
                continue;
            }
            if !content_lines.is_empty() && !content_lines.contains(&r.line) {
                continue;
            }
            let better = match best {
                None => true,
                Some(b) => r.line < b.line || (r.line == b.line && r.is_box && !b.is_box),
            };
            if better {
                best = Some(r);
            }
            if r.line == line && r.is_box {
                break;
            }
        }
        let r = best?;
        let height = if r.is_box { (r.height + r.depth) as f64 * self.scale } else { 12.0 };
        Some(PdfLocation {
            page: r.page,
            x: r.h as f64 * self.scale,
            y: r.v as f64 * self.scale - r.height as f64 * self.scale,
            height,
        })
    }

    /// Which source line produced the content at a point on a page.
    pub fn inverse(&self, page: u32, x: f64, y: f64) -> Option<SourceLocation> {
        let x_sp = x / self.scale;
        let y_sp = y / self.scale;
        // The page, the text frame and the column are boxes too, and TeX attributes them to
        // whatever line closed the document. They contain every click, so ranking by containment
        // alone sends the whole page to \\end{document}. Keep boxes near the height of a line of
        // text, which is what a reader is pointing at, and fall back to everything if that leaves
        // nothing.
        let page_records: Vec<&Record> = self.records.iter().filter(|r| r.page == page).collect();
        let mut heights: Vec<i64> =
            page_records.iter().filter(|r| r.is_box).map(|r| r.height + r.depth).filter(|h| *h > 0).collect();
        heights.sort_unstable();
        let line_height = heights.get(heights.len() / 2).copied().unwrap_or(0);
        let tall = line_height.saturating_mul(4);
        let kept: Vec<&Record> = page_records
            .iter()
            .copied()
            .filter(|r| !r.is_box || tall == 0 || r.height + r.depth <= tall)
            .collect();
        let candidates = if kept.is_empty() { page_records } else { kept };
        let mut best: Option<(&Record, f64)> = None;
        for r in candidates {
            let score = if r.is_box {
                let top = (r.v - r.height) as f64;
                let bottom = (r.v + r.depth) as f64;
                let left = r.h as f64;
                let right = (r.h + r.width) as f64;
                let dy = if y_sp < top { top - y_sp } else if y_sp > bottom { y_sp - bottom } else { 0.0 };
                let dx = if x_sp < left { left - x_sp } else if x_sp > right { x_sp - right } else { 0.0 };
                dy * 4.0 + dx + (r.width as f64 * 1e-7)
            } else {
                (r.v as f64 - y_sp).abs() * 4.0 + (r.h as f64 - x_sp).abs()
            };
            if best.is_none_or(|(_, s)| score < s) {
                best = Some((r, score));
            }
        }
        let (r, _) = best?;
        Some(SourceLocation {
            file: self.inputs.get(&r.file)?.clone(),
            line: r.line,
        })
    }
}

/// `file,line[,col]:h,v[:w,h,d]`
fn parse_record(body: &str, page: u32, is_box: bool, container: bool) -> Option<Record> {
    let mut sections = body.split(':');
    let link = sections.next()?;
    let pos = sections.next()?;
    let size = sections.next();
    let mut link_parts = link.split(',');
    let file: u32 = link_parts.next()?.trim().parse().ok()?;
    let line: u32 = link_parts.next()?.trim().parse().ok()?;
    let mut pos_parts = pos.split(',');
    let h: i64 = pos_parts.next()?.trim().parse().ok()?;
    let v: i64 = pos_parts.next()?.trim().parse().ok()?;
    let (width, height, depth) = match size {
        Some(s) => {
            let mut p = s.split(',').map(|x| x.trim().parse::<i64>().unwrap_or(0));
            (p.next().unwrap_or(0), p.next().unwrap_or(0), p.next().unwrap_or(0))
        }
        None => (0, 0, 0),
    };
    Some(Record {
        page,
        file,
        line,
        h,
        v,
        width,
        height,
        depth,
        is_box,
        container,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/main.synctex");

    #[test]
    fn parses_inputs_and_pages() {
        let st = SyncTex::parse(FIXTURE, Path::new("/nope")).unwrap();
        let inputs: Vec<&str> = st.inputs().collect();
        assert_eq!(inputs, vec!["main.tex"]);
        assert!(st.records.iter().all(|r| r.page == 1));
        assert!(st.records.len() > 20);
    }

    #[test]
    fn forward_and_inverse_agree() {
        let st = SyncTex::parse(FIXTURE, Path::new("/nope")).unwrap();
        // Line 10 is \maketitle in the blank template; it produces the title box.
        let loc = st.forward("main.tex", 10).expect("title location");
        assert_eq!(loc.page, 1);
        assert!(loc.y > 50.0 && loc.y < 400.0, "{loc:?}");
        let back = st.inverse(1, loc.x + 5.0, loc.y + 5.0).expect("inverse");
        assert_eq!(back.file, "main.tex");
        assert!((9..=19).contains(&back.line), "{back:?}");
        assert!(st.forward("other.tex", 1).is_none());
        assert!(st.forward("main", 10).is_some());
    }

    /// The shape of a real report: the preamble (lines 1 to 4) typesets nothing, line 5 is
    /// \\begin{document} with only an enclosing box that TeX put on the last page, and the title
    /// block starts at line 7 on page 1.
    const PREAMBLE_CASE: &str = "SyncTeX Version:1
Input:1:/work/main.tex
Output:pdf
Magnification:1000
Unit:1
X Offset:0
Y Offset:0
Content:
!100
{1
[1,7:4736286,4736286:26673152,40000000,0
(1,7:4736286,6000000:26673152,700000,0
x1,7:4736286,6000000
g1,7:5000000,6000000
)
]
}1
{2
[1,9:4736286,4736286:26673152,40000000,0
(1,9:4736286,9000000:26673152,700000,0
x1,9:4736286,9000000
)
(1,5:4736286,40000000:26673152,700000,0
)
]
}2
Postamble:
";

    #[test]
    fn a_preamble_line_goes_to_the_title_and_not_to_a_stray_box() {
        let st = SyncTex::parse(PREAMBLE_CASE, Path::new("/work")).unwrap();
        for line in [1, 3, 4, 5, 6] {
            let loc = st.forward("main.tex", line).unwrap();
            assert_eq!(loc.page, 1, "line {line} went to {loc:?}");
        }
        assert_eq!(st.forward("main.tex", 9).unwrap().page, 2);
    }
}
