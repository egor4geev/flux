//! Three-way merge of texts, without git: the merge tool shows a conflicted file as its three
//! versions — the common base, ours and theirs — and builds the result from them.
//!
//! Both sides are diffed against the base (the same line diff as the gutter, [`diff_lines`]); a
//! change of one side that doesn't touch a change of the other goes in as it is, changes that
//! overlap or touch (adjacent lines, as git decides) form one region: a conflict, unless both sides
//! made the same change there.
//!
//! [`resolve_simple`] — the merge tool's magic wand — merges one conflict again at the level of
//! words: two edits of the same line that change different words come together.

use std::ops::Range;

use crate::diff::{Hunk, diff_lines, diff_tokens, line_ranges, words};

/// A changed region: lines of the base, ours and theirs (zero-based, half-open). An empty base
/// range is an insertion before that base line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub base: Range<u32>,
    pub ours: Range<u32>,
    pub theirs: Range<u32>,
    pub kind: ChunkKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Only ours changed these lines.
    Ours,
    /// Only theirs changed them.
    Theirs,
    /// Both made the same change.
    Both,
    /// Both changed them, differently.
    Conflict,
}

/// The changed regions of a three-way merge, in order. Lines include their line breaks (compare
/// texts without `\r`, as the diff viewer does).
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Vec<Chunk> {
    let ours_lines = line_ranges(ours);
    let theirs_lines = line_ranges(theirs);
    regions(&diff_lines(base, ours), &diff_lines(base, theirs))
        .into_iter()
        .map(|region| {
            let kind = match (region.ours_changed, region.theirs_changed) {
                (true, false) => ChunkKind::Ours,
                (false, true) => ChunkKind::Theirs,
                _ if lines(ours, &ours_lines, &region.ours)
                    == lines(theirs, &theirs_lines, &region.theirs) =>
                {
                    ChunkKind::Both
                }
                _ => ChunkKind::Conflict,
            };
            Chunk {
                base: region.base,
                ours: region.ours,
                theirs: region.theirs,
                kind,
            }
        })
        .collect()
}

/// A changed region of a three-way merge, in units (lines or tokens) of each version, and which
/// sides changed it.
struct Region {
    base: Range<u32>,
    ours: Range<u32>,
    theirs: Range<u32>,
    ours_changed: bool,
    theirs_changed: bool,
}

/// Joins the hunks of two diffs against the same base (`a`: base → ours, `b`: base → theirs) into
/// regions: hunks that overlap or touch, from either side, are one region.
fn regions(a: &[Hunk], b: &[Hunk]) -> Vec<Region> {
    let mut regions = Vec::new();
    let (mut i, mut j) = (0, 0);
    // How many units each side gained before the current region (base unit + delta = side unit).
    let (mut delta_a, mut delta_b) = (0i64, 0i64);
    while i < a.len() || j < b.len() {
        let first_is_a = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => x.old.start <= y.old.start,
            (Some(_), None) => true,
            _ => false,
        };
        let first = if first_is_a { &a[i] } else { &b[j] };
        let (start, mut end) = (first.old.start, first.old.end);
        let (from_a, from_b) = (i, j);
        if first_is_a {
            i += 1;
        } else {
            j += 1;
        }
        // Grow the region by every hunk of either side that overlaps or touches it.
        loop {
            if let Some(next) = a.get(i).filter(|hunk| hunk.old.start <= end) {
                end = end.max(next.old.end);
                i += 1;
            } else if let Some(next) = b.get(j).filter(|hunk| hunk.old.start <= end) {
                end = end.max(next.old.end);
                j += 1;
            } else {
                break;
            }
        }
        let gain = |hunks: &[Hunk]| -> i64 {
            hunks
                .iter()
                .map(|hunk| hunk.new.len() as i64 - hunk.old.len() as i64)
                .sum()
        };
        let in_a = &a[from_a..i];
        let in_b = &b[from_b..j];
        let side = |delta: i64, hunks: &[Hunk]| -> Range<u32> {
            let from = (start as i64 + delta) as u32;
            let to = (end as i64 + delta + gain(hunks)) as u32;
            from..to
        };
        regions.push(Region {
            base: start..end,
            ours: side(delta_a, in_a),
            theirs: side(delta_b, in_b),
            ours_changed: !in_a.is_empty(),
            theirs_changed: !in_b.is_empty(),
        });
        delta_a += gain(in_a);
        delta_b += gain(in_b);
    }
    regions
}

/// The magic wand of the merge tool: merges a conflict's three texts word by word. `Some` — the
/// sides changed different words (or made the same change), and this is the merged text; `None` —
/// their changes touch, the conflict stays for the user.
pub fn resolve_simple(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (b, o, t) = (word_slices(base), word_slices(ours), word_slices(theirs));
    let join = |tokens: &[&str], range: &Range<u32>| -> String {
        tokens[range.start as usize..range.end as usize].concat()
    };
    let mut merged = String::with_capacity(base.len().max(ours.len()).max(theirs.len()));
    let mut at = 0u32;
    for region in regions(&diff_tokens(&b, &o), &diff_tokens(&b, &t)) {
        merged.push_str(&join(&b, &(at..region.base.start)));
        let taken = match (region.ours_changed, region.theirs_changed) {
            (true, false) => join(&o, &region.ours),
            (false, true) => join(&t, &region.theirs),
            _ => {
                let (ours, theirs) = (join(&o, &region.ours), join(&t, &region.theirs));
                if ours != theirs {
                    return None;
                }
                ours
            }
        };
        merged.push_str(&taken);
        at = region.base.end;
    }
    merged.push_str(&join(&b, &(at..b.len() as u32)));
    Some(merged)
}

/// A text cut into its word tokens (they cover the whole text).
fn word_slices(text: &str) -> Vec<&str> {
    words(text).into_iter().map(|range| &text[range]).collect()
}

/// The merge as the merge tool opens it: every non-conflicting change applied (ours, theirs, the
/// same on both sides), the base lines left in each conflict. Returns the text and each chunk's
/// line range in it.
pub fn initial_result(
    base: &str,
    ours: &str,
    theirs: &str,
    chunks: &[Chunk],
) -> (String, Vec<Range<u32>>) {
    let base_lines = line_ranges(base);
    let ours_lines = line_ranges(ours);
    let theirs_lines = line_ranges(theirs);
    let mut text = String::with_capacity(base.len().max(ours.len()).max(theirs.len()));
    let mut ranges = Vec::with_capacity(chunks.len());
    let mut at = 0u32;
    let mut line = 0u32;
    for chunk in chunks {
        let same = at..chunk.base.start;
        text.push_str(lines(base, &base_lines, &same));
        line += same.len() as u32;
        let (source, source_lines, range) = match chunk.kind {
            ChunkKind::Ours | ChunkKind::Both => (ours, &ours_lines, &chunk.ours),
            ChunkKind::Theirs => (theirs, &theirs_lines, &chunk.theirs),
            ChunkKind::Conflict => (base, &base_lines, &chunk.base),
        };
        text.push_str(lines(source, source_lines, range));
        ranges.push(line..line + range.len() as u32);
        line += range.len() as u32;
        at = chunk.base.end;
    }
    text.push_str(lines(base, &base_lines, &(at..base_lines.len() as u32)));
    (text, ranges)
}

/// The text of lines `range` (with their line breaks); lines past the end are empty.
pub fn lines<'a>(text: &'a str, ranges: &[Range<usize>], range: &Range<u32>) -> &'a str {
    let start = range.start as usize;
    let end = (range.end as usize).min(ranges.len());
    if start >= end {
        return "";
    }
    &text[ranges[start].start..ranges[end - 1].end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(chunks: &[Chunk]) -> Vec<ChunkKind> {
        chunks.iter().map(|chunk| chunk.kind).collect()
    }

    #[test]
    fn separate_changes_merge_cleanly() {
        let base = "a\nb\nc\nd\ne\n";
        let ours = "A\nb\nc\nd\ne\n";
        let theirs = "a\nb\nc\nd\nE\n";
        let chunks = merge3(base, ours, theirs);
        assert_eq!(kinds(&chunks), vec![ChunkKind::Ours, ChunkKind::Theirs]);
        let (text, ranges) = initial_result(base, ours, theirs, &chunks);
        assert_eq!(text, "A\nb\nc\nd\nE\n");
        assert_eq!(ranges, vec![0..1, 4..5]);
    }

    #[test]
    fn overlapping_changes_conflict_and_keep_the_base() {
        let base = "a\nb\nc\n";
        let ours = "a\nours\nc\n";
        let theirs = "a\ntheirs\nc\n";
        let chunks = merge3(base, ours, theirs);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].kind, ChunkKind::Conflict);
        assert_eq!(
            (&chunks[0].base, &chunks[0].ours, &chunks[0].theirs),
            (&(1..2), &(1..2), &(1..2))
        );
        let (text, ranges) = initial_result(base, ours, theirs, &chunks);
        assert_eq!(text, base);
        assert_eq!(ranges, vec![1..2]);
    }

    #[test]
    fn adjacent_changes_conflict_as_in_git() {
        let base = "a\nb\nc\nd\n";
        let ours = "a\nB\nc\nd\n";
        let theirs = "a\nb\nC\nd\n";
        let chunks = merge3(base, ours, theirs);
        assert_eq!(kinds(&chunks), vec![ChunkKind::Conflict]);
        assert_eq!(chunks[0].base, 1..3);
        assert_eq!(chunks[0].ours, 1..3);
        assert_eq!(chunks[0].theirs, 1..3);
    }

    #[test]
    fn the_same_change_on_both_sides_is_not_a_conflict() {
        let base = "a\nb\nc\n";
        let both = "a\nx\ny\nc\n";
        let chunks = merge3(base, both, both);
        assert_eq!(kinds(&chunks), vec![ChunkKind::Both]);
        let (text, ranges) = initial_result(base, both, both, &chunks);
        assert_eq!(text, both);
        assert_eq!(ranges, vec![1..3]);
    }

    #[test]
    fn ranges_follow_lines_gained_and_lost_before_them() {
        // Ours adds two lines at the top; theirs deletes a line in the middle; both change the end.
        let base = "1\n2\n3\n4\n5\n6\n";
        let ours = "0\n0\n1\n2\n3\n4\n5\nours\n";
        let theirs = "1\n2\n4\n5\ntheirs\n";
        let chunks = merge3(base, ours, theirs);
        assert_eq!(
            kinds(&chunks),
            vec![ChunkKind::Ours, ChunkKind::Theirs, ChunkKind::Conflict]
        );
        assert_eq!(chunks[0].ours, 0..2);
        assert_eq!(chunks[0].theirs, 0..0);
        assert_eq!(chunks[1].base, 2..3);
        assert_eq!(chunks[1].ours, 4..5);
        assert_eq!(chunks[1].theirs, 2..2);
        assert_eq!(chunks[2].base, 5..6);
        assert_eq!(chunks[2].ours, 7..8);
        assert_eq!(chunks[2].theirs, 4..5);
        let (text, ranges) = initial_result(base, ours, theirs, &chunks);
        assert_eq!(text, "0\n0\n1\n2\n4\n5\n6\n");
        assert_eq!(ranges, vec![0..2, 4..4, 6..7]);
    }

    #[test]
    fn files_added_on_both_sides_conflict_as_a_whole() {
        let chunks = merge3("", "fn a() {}\n", "fn b() {}\n");
        assert_eq!(kinds(&chunks), vec![ChunkKind::Conflict]);
        assert_eq!(chunks[0].base, 0..0);
        assert_eq!(chunks[0].ours, 0..1);
        assert_eq!(chunks[0].theirs, 0..1);
        let (text, ranges) = initial_result("", "fn a() {}\n", "fn b() {}\n", &chunks);
        assert_eq!(text, "");
        assert_eq!(ranges, vec![0..0]);
    }

    #[test]
    fn simple_conflicts_merge_word_by_word() {
        // The same line: one side renamed the variable, the other changed the value.
        assert_eq!(
            resolve_simple("let x = 1;\n", "let y = 1;\n", "let x = 2;\n").as_deref(),
            Some("let y = 2;\n")
        );
        // Both changed the same word differently: no.
        assert_eq!(
            resolve_simple("let x = 1;\n", "let y = 1;\n", "let z = 1;\n"),
            None
        );
        // The same change on both sides, and changes in different lines of one block.
        assert_eq!(
            resolve_simple("a b\nc d\n", "a B\nc d\n", "a b\nC d\n").as_deref(),
            Some("a B\nC d\n")
        );
        // Cyrillic words are words.
        assert_eq!(
            resolve_simple("привет мир\n", "здравствуй мир\n", "привет всем\n").as_deref(),
            Some("здравствуй всем\n")
        );
        // A deletion next to an insertion touches it.
        assert_eq!(resolve_simple("a b c", "a c", "a b x c"), None);
        assert_eq!(resolve_simple("", "", "").as_deref(), Some(""));
    }

    #[test]
    fn no_changes_no_chunks() {
        assert!(merge3("a\n", "a\n", "a\n").is_empty());
        let (text, ranges) = initial_result("a\nb", "a\nb", "a\nb", &[]);
        assert_eq!(text, "a\nb");
        assert!(ranges.is_empty());
    }
}
