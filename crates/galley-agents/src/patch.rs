//! Turn "before" and "after" texts into the `{find, replace}` edits `propose_patch` accepts. Each
//! `find` must occur exactly once in the base text, so changed line ranges grow by whole lines of
//! context until they are unique, and ranges that grow into each other merge.

use similar::{DiffTag, TextDiff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub find: String,
    pub replace: String,
}

#[derive(Clone, Copy)]
struct Region {
    old: (usize, usize),
    new: (usize, usize),
}

pub fn edits(base: &str, new: &str) -> Vec<Edit> {
    if base == new {
        return Vec::new();
    }
    let old_lines: Vec<&str> = base.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let diff = TextDiff::from_lines(base, new);

    let mut regions: Vec<Region> = Vec::new();
    for op in diff.ops() {
        if op.tag() == DiffTag::Equal {
            continue;
        }
        let (o, n) = (op.old_range(), op.new_range());
        match regions.last_mut() {
            Some(last) if last.old.1 == o.start && last.new.1 == n.start => {
                last.old.1 = o.end;
                last.new.1 = n.end;
            }
            _ => regions.push(Region { old: (o.start, o.end), new: (n.start, n.end) }),
        }
    }

    let text_of = |lines: &[&str], r: (usize, usize)| lines[r.0..r.1].concat();
    let unique = |r: &Region| {
        let f = text_of(&old_lines, r.old);
        !f.is_empty() && base.matches(&f).count() == 1
    };

    loop {
        // Equal lines flank every region on both sides, so stepping out by one line in the old
        // text is the same line in the new text.
        for r in regions.iter_mut() {
            while !unique(r) {
                let mut grew = false;
                if r.old.0 > 0 {
                    r.old.0 -= 1;
                    r.new.0 -= 1;
                    grew = true;
                }
                if r.old.1 < old_lines.len() {
                    r.old.1 += 1;
                    r.new.1 += 1;
                    grew = true;
                }
                if !grew {
                    break;
                }
            }
        }
        let mut merged: Vec<Region> = Vec::new();
        let mut did_merge = false;
        for r in regions.iter().copied() {
            match merged.last_mut() {
                Some(last) if r.old.0 < last.old.1 => {
                    last.old.1 = last.old.1.max(r.old.1);
                    last.new.1 = last.new.1.max(r.new.1);
                    did_merge = true;
                }
                _ => merged.push(r),
            }
        }
        regions = merged;
        if !did_merge {
            break;
        }
    }

    regions
        .into_iter()
        .map(|r| Edit { find: text_of(&old_lines, r.old), replace: text_of(&new_lines, r.new) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(base: &str, edits: &[Edit]) -> String {
        let mut s = base.to_string();
        for e in edits {
            assert_eq!(s.matches(&e.find).count(), 1, "find must be unique: {:?}", e.find);
            s = s.replacen(&e.find, &e.replace, 1);
        }
        s
    }

    #[test]
    fn single_line_change_is_one_edit() {
        let base = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let e = edits(base, new);
        assert_eq!(e, vec![Edit { find: "b\n".into(), replace: "B\n".into() }]);
        assert_eq!(apply(base, &e), new);
    }

    #[test]
    fn insertion_borrows_context_lines() {
        let base = "\\documentclass{article}\n\\begin{document}\nHi\n\\end{document}\n";
        let new = "\\documentclass{article}\n\\usepackage{natbib}\n\\begin{document}\nHi\n\\end{document}\n";
        let e = edits(base, new);
        assert_eq!(e.len(), 1);
        assert!(e[0].find.contains("\\documentclass") || e[0].find.contains("\\begin{document}"));
        assert_eq!(apply(base, &e), new);
    }

    #[test]
    fn repeated_lines_grow_until_unique_and_merge() {
        let base = "x\ny\nx\ny\nx\ny\n";
        let new = "x\nY1\nx\ny\nx\nY3\n";
        let e = edits(base, new);
        assert_eq!(apply(base, &e), new);
    }

    #[test]
    fn far_apart_changes_stay_separate() {
        let base: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let new = base.replace("line 3\n", "LINE 3\n").replace("line 30\n", "LINE 30\n");
        let e = edits(&base, &new);
        assert_eq!(e.len(), 2);
        assert_eq!(apply(&base, &e), new);
    }

    #[test]
    fn deletion_and_no_trailing_newline() {
        let base = "one\ntwo\nthree";
        let new = "one\nthree";
        let e = edits(base, new);
        assert_eq!(apply(base, &e), new);
        assert!(edits(base, base).is_empty());
    }
}
