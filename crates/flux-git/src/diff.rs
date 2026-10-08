//! Diffs of two texts, without git: the gutter markers and the diff viewer compare the unsaved text
//! of a document with its HEAD version as you type.
//!
//! Lines are compared by the histogram algorithm with git's indent heuristic (`imara-diff`), so the
//! blocks match what `git diff` shows. Inside a modified block, words are compared by Myers (the
//! histogram algorithm is poor on small alphabets): runs of letters, digits and `_`, runs of
//! whitespace, and single other characters.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

/// A changed block: the `old` lines of the base were replaced by the `new` lines of the current
/// text (zero-based line numbers, half-open). An insertion has an empty `old` — `old.start` is the
/// base line before which the lines were added; a deletion has an empty `new` — `new.start` is the
/// current line before which the lines were removed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hunk {
    pub old: Range<u32>,
    pub new: Range<u32>,
}

/// What a hunk did, for its color: added (green), deleted (gray), modified (blue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    Added,
    Deleted,
    Modified,
}

impl Hunk {
    pub fn kind(&self) -> HunkKind {
        if self.old.is_empty() {
            HunkKind::Added
        } else if self.new.is_empty() {
            HunkKind::Deleted
        } else {
            HunkKind::Modified
        }
    }
}

/// The changed blocks between two texts, in order. Lines include their line breaks: a line that
/// lost its final newline, or switched from `\r\n` to `\n`, is changed.
pub fn diff_lines(old: &str, new: &str) -> Vec<Hunk> {
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.hunks()
        .map(|hunk| Hunk {
            old: hunk.before,
            new: hunk.after,
        })
        .collect()
}

/// The changed parts of two versions of a block: byte ranges in `old` and in `new` (in order,
/// adjacent parts merged).
pub fn diff_words(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_words = words(old);
    let new_words = words(new);
    let mut input = InternedInput::default();
    input.update_before(old_words.iter().map(|range| &old[range.clone()]));
    input.update_after(new_words.iter().map(|range| &new[range.clone()]));
    let mut diff = Diff::compute(Algorithm::Myers, &input);
    diff.postprocess_no_heuristic(&input);
    let span = |words: &[Range<usize>], tokens: Range<u32>| {
        (!tokens.is_empty())
            .then(|| words[tokens.start as usize].start..words[tokens.end as usize - 1].end)
    };
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for hunk in diff.hunks() {
        removed.extend(span(&old_words, hunk.before));
        added.extend(span(&new_words, hunk.after));
    }
    (merge_adjacent(removed), merge_adjacent(added))
}

/// The base text with only some hunks applied: the content of a partial commit (the checked
/// changes of a file), or of a partial rollback (the base with the kept changes). `hunks` must come
/// from `diff_lines(old, new)`.
pub fn apply_hunks(old: &str, new: &str, hunks: &[Hunk], apply: impl Fn(&Hunk) -> bool) -> String {
    let old_lines = line_ranges(old);
    let new_lines = line_ranges(new);
    let lines = |text: &str, ranges: &[Range<usize>], span: &Range<u32>| -> String {
        let span = span.start as usize..(span.end as usize).min(ranges.len());
        match (
            ranges.get(span.start),
            span.end.checked_sub(1).and_then(|i| ranges.get(i)),
        ) {
            (Some(first), Some(last)) if !span.is_empty() => {
                text[first.start..last.end].to_string()
            }
            _ => String::new(),
        }
    };
    let mut result = String::with_capacity(old.len().max(new.len()));
    let mut at = 0u32;
    for hunk in hunks {
        result.push_str(&lines(old, &old_lines, &(at..hunk.old.start)));
        if apply(hunk) {
            result.push_str(&lines(new, &new_lines, &hunk.new));
        } else {
            result.push_str(&lines(old, &old_lines, &hunk.old));
        }
        at = hunk.old.end;
    }
    result.push_str(&lines(old, &old_lines, &(at..old_lines.len() as u32)));
    result
}

/// Byte ranges of the lines, each with its line break.
fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            ranges.push(start..at + 1);
            start = at + 1;
        }
    }
    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}

/// Word tokens: runs of letters, digits and `_`; runs of whitespace; single other characters.
fn words(text: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut tokens: Vec<Range<usize>> = Vec::new();
    let mut previous: Option<Class> = None;
    for (at, c) in text.char_indices() {
        let current = class(c);
        let joins = current != Class::Other && previous.as_ref() == Some(&current);
        match tokens.last_mut() {
            Some(last) if joins => last.end = at + c.len_utf8(),
            _ => tokens.push(at..at + c.len_utf8()),
        }
        previous = Some(current);
    }
    tokens
}

fn merge_adjacent(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => merged.push(range),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old: Range<u32>, new: Range<u32>) -> Hunk {
        Hunk { old, new }
    }

    #[test]
    fn added_modified_and_deleted_blocks() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nc\nx\ny\nd\n";
        let hunks = diff_lines(old, new);
        assert_eq!(
            hunks,
            vec![hunk(1..2, 1..2), hunk(3..3, 3..5), hunk(4..5, 6..6)]
        );
        let kinds: Vec<HunkKind> = hunks.iter().map(Hunk::kind).collect();
        assert_eq!(
            kinds,
            vec![HunkKind::Modified, HunkKind::Added, HunkKind::Deleted]
        );
    }

    #[test]
    fn identical_and_empty_texts() {
        assert!(diff_lines("a\nb\n", "a\nb\n").is_empty());
        assert_eq!(diff_lines("", "a\n"), vec![hunk(0..0, 0..1)]);
        assert_eq!(diff_lines("a\n", ""), vec![hunk(0..1, 0..0)]);
        // A line that lost its final newline is changed.
        assert_eq!(diff_lines("a\nb\n", "a\nb"), vec![hunk(1..2, 1..2)]);
    }

    #[test]
    fn changed_words_inside_a_line() {
        let (removed, added) =
            diff_words("let total = price * count;", "let sum = price * amount;");
        let old = "let total = price * count;";
        let new = "let sum = price * amount;";
        let words = |text: &str, ranges: &[Range<usize>]| -> Vec<String> {
            ranges.iter().map(|r| text[r.clone()].to_string()).collect()
        };
        assert_eq!(words(old, &removed), vec!["total", "count"]);
        assert_eq!(words(new, &added), vec!["sum", "amount"]);
        // Cyrillic words are words too.
        let (removed, added) = diff_words("привет мир", "привет всем");
        assert_eq!(words("привет мир", &removed), vec!["мир"]);
        assert_eq!(words("привет всем", &added), vec!["всем"]);
    }

    #[test]
    fn some_hunks_are_applied() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nc\nx\ny\nd\n";
        let hunks = diff_lines(old, new);
        assert_eq!(apply_hunks(old, new, &hunks, |_| true), new);
        assert_eq!(apply_hunks(old, new, &hunks, |_| false), old);
        // Only the first change (b → B) is committed: the rest stays as in the base.
        let first = hunks[0].clone();
        assert_eq!(
            apply_hunks(old, new, &hunks, |h| *h == first),
            "a\nB\nc\nd\ne\n"
        );
        // Everything but the deletion of "e".
        let deletion = hunks[2].clone();
        assert_eq!(
            apply_hunks(old, new, &hunks, |h| *h != deletion),
            "a\nB\nc\nx\ny\nd\ne\n"
        );
    }

    #[test]
    fn apply_works_without_final_newlines() {
        let old = "a\nb";
        let new = "a\nc";
        let hunks = diff_lines(old, new);
        assert_eq!(apply_hunks(old, new, &hunks, |_| true), new);
        assert_eq!(apply_hunks(old, new, &hunks, |_| false), old);
    }
}
