//! The marks the field hides (part 5). The parent's markup may name the characters of its spans'
//! marks — a plugin conversation's `**`, `__`, `[` and `](address)` — and the field shows the
//! formatted text alone, as the messengers' fields do, while its document stays the Markdown the
//! parent sends. [`Concealed`] maps the document's positions to the *places* of the shown text: a
//! hidden mark takes no room, the motions work on the shown text and the caret never stops between
//! marks ([`Concealed::rest`]), deleting what is shown keeps every mark's pair
//! ([`Concealed::delete`]), and a copy carries the Markdown that keeps its formatting
//! ([`Concealed::markdown`]). Made again only when the text changes.

use std::ops::Range;

use flux_core::Rope;
use flux_core::transaction::Change;

/// A span of the parent's markup: the characters of its opening and its closing marks, which the
/// field hides (`**` and `**`; `[` and `](address)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub open: Range<usize>,
    pub close: Range<usize>,
}

/// The text as it is shown: the spans' marks hidden. Positions are the document's characters; a
/// place is a position in the shown text. The positions around hidden marks are one place, and
/// the caret rests at one of them.
#[derive(Debug, Default)]
pub(super) struct Concealed {
    /// The hidden characters in runs: in order, apart.
    runs: Vec<Range<usize>>,
    /// The place of each run (the shown characters before it).
    at: Vec<usize>,
    spans: Vec<Span>,
    /// The shown text: the field's motions work on it.
    pub(super) shown: Rope,
}

impl Concealed {
    pub(super) fn new(text: &Rope, mut spans: Vec<Span>) -> Self {
        let len = text.len_chars();
        spans.retain(|span| {
            span.open.start <= span.open.end
                && span.open.end <= span.close.start
                && span.close.start <= span.close.end
                && span.close.end <= len
        });
        let mut marks: Vec<Range<usize>> = spans
            .iter()
            .flat_map(|span| [span.open.clone(), span.close.clone()])
            .filter(|mark| !mark.is_empty())
            .collect();
        marks.sort_by_key(|mark| mark.start);
        let mut runs: Vec<Range<usize>> = Vec::new();
        for mark in marks {
            match runs.last_mut() {
                Some(run) if mark.start <= run.end => run.end = run.end.max(mark.end),
                _ => runs.push(mark),
            }
        }
        let mut at = Vec::with_capacity(runs.len());
        let mut shown = String::with_capacity(text.len_bytes());
        let (mut hidden, mut from) = (0, 0);
        for run in &runs {
            at.push(run.start - hidden);
            hidden += run.len();
            shown.extend(text.slice(from..run.start).chars());
            from = run.end;
        }
        shown.extend(text.slice(from..).chars());
        Concealed {
            runs,
            at,
            spans,
            shown: Rope::from_str(&shown),
        }
    }

    /// Something is hidden: without it the places are the positions.
    pub(super) fn hides(&self) -> bool {
        !self.runs.is_empty()
    }

    /// The character at `pos` is a hidden mark.
    pub(super) fn is_hidden(&self, pos: usize) -> bool {
        let next = self.runs.partition_point(|run| run.end <= pos);
        self.runs.get(next).is_some_and(|run| run.start <= pos)
    }

    /// The place of position `pos`.
    pub(super) fn place(&self, pos: usize) -> usize {
        let next = self.runs.partition_point(|run| run.start < pos);
        match next.checked_sub(1) {
            Some(last) => self.at[last] + pos - pos.min(self.runs[last].end),
            None => pos,
        }
    }

    /// The first and the last positions of `place`: after the shown character before it and
    /// before the one at it — the hidden marks between them are one place.
    pub(super) fn positions(&self, place: usize) -> (usize, usize) {
        let next = self.at.partition_point(|&at| at < place);
        let hidden = next
            .checked_sub(1)
            .map_or(0, |last| self.runs[last].end - self.at[last]);
        let first = place + hidden;
        match self.at.get(next) {
            Some(&at) if at == place => (first, self.runs[next].end),
            _ => (first, first),
        }
    }

    /// Where the caret rests at `place`: after the closing marks there and before the opening
    /// ones — outside the spans, so what is typed there is plain — or between the marks of a span
    /// with nothing shown in it, so what is typed goes in it.
    pub(super) fn rest(&self, place: usize) -> usize {
        let (first, last) = self.positions(place);
        if first == last {
            return first;
        }
        // The marks of the place in order; an opening one with its span.
        let mut marks: Vec<(Range<usize>, Option<&Span>)> = Vec::new();
        for span in &self.spans {
            let within =
                |mark: &Range<usize>| !mark.is_empty() && first <= mark.start && mark.end <= last;
            if within(&span.open) {
                marks.push((span.open.clone(), Some(span)));
            }
            if within(&span.close) {
                marks.push((span.close.clone(), None));
            }
        }
        marks.sort_by_key(|(mark, _)| mark.start);
        let mut rest = first;
        for (mark, opens) in marks {
            match opens {
                None => rest = mark.end,
                Some(span) => {
                    if self.is_blank(span) {
                        rest = mark.end;
                    }
                    break;
                }
            }
        }
        rest
    }

    /// Where the caret rests at the place of `pos`.
    pub(super) fn snap(&self, pos: usize) -> usize {
        self.rest(self.place(pos))
    }

    /// `motion` made on the shown text: the selection's head goes to the place it gives and rests
    /// there; its anchor keeps its position while its place stays.
    pub(super) fn moved(
        &self,
        range: flux_core::Range,
        motion: impl Fn(&Rope, flux_core::Range) -> flux_core::Range,
    ) -> flux_core::Range {
        let anchor = self.place(range.anchor);
        let moved = motion(
            &self.shown,
            flux_core::Range::new(anchor, self.place(range.head)),
        );
        let head = self.rest(moved.head);
        let anchor = if moved.is_empty() {
            head
        } else if moved.anchor == anchor {
            range.anchor
        } else {
            self.rest(moved.anchor)
        };
        flux_core::Range::new(anchor, head)
    }

    /// The position of the shown character `index`.
    pub(super) fn position_of(&self, index: usize) -> usize {
        self.positions(index).1
    }

    /// Nothing of the span's text is shown.
    fn is_blank(&self, span: &Span) -> bool {
        self.place(span.close.start) == self.place(span.open.end)
    }

    /// Spans with nothing shown in them whose marks don't hold `caret`: what ⌘B with nothing
    /// selected put at the caret, left without typing. Their marks, as changes that delete them.
    pub(super) fn blank_spans(&self, caret: usize) -> Vec<Change> {
        let mut cuts: Vec<Range<usize>> = Vec::new();
        for span in &self.spans {
            if !self.is_blank(span) || (span.open.end..=span.close.start).contains(&caret) {
                continue;
            }
            cuts.extend([span.open.clone(), span.close.clone()]);
        }
        merged(cuts)
            .into_iter()
            .map(|cut| (cut.start, cut.end, None))
            .collect()
    }

    /// The changes that delete the shown characters of `from..to` and put `insert` at `from`. The
    /// hidden marks there stay unless their span is left with nothing shown — then both its marks
    /// go — so no mark loses its pair. With them, where `caret` is after them (after the inserted
    /// text when there is one).
    pub(super) fn delete(
        &self,
        from: usize,
        to: usize,
        insert: &str,
        caret: usize,
    ) -> (Vec<Change>, usize) {
        let gone_spans: Vec<&Span> = if from < to {
            self.spans
                .iter()
                .filter(|span| {
                    let (start, end) = (span.open.end, span.close.start);
                    let all = self.place(end) - self.place(start);
                    let (a, b) = (start.max(from), end.min(to));
                    let inside = if a < b {
                        self.place(b) - self.place(a)
                    } else {
                        0
                    };
                    inside == all
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut cuts: Vec<Range<usize>> = Vec::new();
        for pos in from..to {
            if !self.is_hidden(pos) {
                cuts.push(pos..pos + 1);
            }
        }
        for span in gone_spans {
            cuts.extend([span.open.clone(), span.close.clone()]);
        }
        let cuts = merged(cuts);
        let removed_before = |pos: usize| -> usize {
            cuts.iter()
                .map(|cut| cut.end.min(pos).saturating_sub(cut.start))
                .sum()
        };
        let mut changes: Vec<Change> = cuts.iter().map(|cut| (cut.start, cut.end, None)).collect();
        if insert.is_empty() {
            return (changes, caret - removed_before(caret));
        }
        // The insertion joins the cut at `from`, or is a change of its own.
        let at = match cuts
            .iter()
            .position(|cut| cut.start <= from && from <= cut.end)
        {
            Some(index) => {
                changes[index].2 = Some(insert.to_string());
                cuts[index].start
            }
            None => {
                let index = changes.partition_point(|change| change.0 < from);
                changes.insert(index, (from, from, Some(insert.to_string())));
                from
            }
        };
        let caret = at - removed_before(at) + insert.chars().count();
        (changes, caret)
    }

    /// The shown text of `from..to`: what a copy puts on the clipboard as plain text.
    pub(super) fn shown_text(&self, from: usize, to: usize) -> String {
        self.shown
            .slice(self.place(from)..self.place(to))
            .to_string()
    }

    /// The Markdown of `from..to` that keeps its formatting on its own: the marks of the spans it
    /// starts or ends inside put around it.
    pub(super) fn markdown(&self, text: &Rope, from: usize, to: usize) -> String {
        let crossing: Vec<&Span> = self
            .spans
            .iter()
            .filter(|span| span.open.end < to && from < span.close.start)
            .collect();
        let mut opens: Vec<&Span> = crossing
            .iter()
            .copied()
            .filter(|span| span.open.start < from)
            .collect();
        opens.sort_by_key(|span| span.open.start);
        let mut closes: Vec<&Span> = crossing
            .into_iter()
            .filter(|span| span.close.end > to)
            .collect();
        closes.sort_by_key(|span| span.close.start);
        let mut markdown = String::new();
        for span in opens {
            markdown.extend(text.slice(span.open.clone()).chars());
        }
        markdown.extend(text.slice(from..to).chars());
        for span in closes {
            markdown.extend(text.slice(span.close.clone()).chars());
        }
        markdown
    }
}

/// `ranges` sorted and joined where they touch or overlap.
fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|range| !range.is_empty());
    ranges.sort_by_key(|range| range.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(open: Range<usize>, close: Range<usize>) -> Span {
        Span { open, close }
    }

    /// "a **b** c": the bold span's marks at 2..4 and 5..7.
    fn bold() -> (Rope, Concealed) {
        let text = Rope::from_str("a **b** c");
        let concealed = Concealed::new(&text, vec![span(2..4, 5..7)]);
        (text, concealed)
    }

    /// The text after `changes`.
    fn applied(text: &str, changes: Vec<Change>) -> String {
        let mut text = text.to_string();
        for (from, to, insert) in changes.into_iter().rev() {
            let chars: Vec<char> = text.chars().collect();
            let mut out: String = chars[..from].iter().collect();
            out.push_str(insert.as_deref().unwrap_or(""));
            out.extend(&chars[to..]);
            text = out;
        }
        text
    }

    #[test]
    fn hidden_marks_take_no_place() {
        let (_, c) = bold();
        assert_eq!(c.shown.to_string(), "a b c");
        let places: Vec<usize> = (0..=9).map(|pos| c.place(pos)).collect();
        assert_eq!(places, vec![0, 1, 2, 2, 2, 3, 3, 3, 4, 5]);
        assert_eq!(c.positions(2), (2, 4));
        assert_eq!(c.positions(3), (5, 7));
        assert_eq!(c.positions(4), (8, 8));
        assert_eq!(c.position_of(2), 4);
        assert!(c.is_hidden(3) && c.is_hidden(5) && !c.is_hidden(4) && !c.is_hidden(7));
        assert_eq!(c.shown_text(0, 9), "a b c");
        assert_eq!(c.shown_text(3, 6), "b");
    }

    #[test]
    fn the_caret_rests_outside_spans_and_inside_blank_ones() {
        let (_, c) = bold();
        // Before "b": before the opening marks; after it: after the closing ones.
        assert_eq!(c.rest(2), 2);
        assert_eq!(c.rest(3), 7);
        assert_eq!(c.snap(3), 2);
        assert_eq!(c.snap(6), 7);
        // A closing mark then an opening one: between them. "**a**__b__"
        let text = Rope::from_str("**a**__b__");
        let c = Concealed::new(&text, vec![span(0..2, 3..5), span(5..7, 8..10)]);
        assert_eq!(c.rest(1), 5);
        // A pair with nothing in it: between its marks. "x****"
        let text = Rope::from_str("x****");
        let c = Concealed::new(&text, vec![span(1..3, 3..5)]);
        assert_eq!(c.rest(1), 3);
        assert!(c.blank_spans(3).is_empty());
        assert_eq!(c.blank_spans(0), vec![(1, 5, None)]);
        // Sending takes every blank pair, wherever the caret is (`PromptInput::text_to_send`).
        assert_eq!(c.blank_spans(usize::MAX), vec![(1, 5, None)]);
    }

    #[test]
    fn arrows_step_over_hidden_marks() {
        use flux_core::movement::{self, Direction};
        let (_, c) = bold();
        let step = |range: flux_core::Range, dir, extend| {
            c.moved(range, |text, range| {
                movement::move_horizontally(text, range, dir, extend)
            })
        };
        let point = flux_core::Range::point;
        // One press = one shown character: past "b" and its closing marks at once, then back
        // before the opening ones.
        assert_eq!(step(point(2), Direction::Forward, false).head, 7);
        assert_eq!(step(point(7), Direction::Backward, false).head, 2);
        assert_eq!(step(point(1), Direction::Forward, false).head, 2);
        // A caret typing at the span's end: the next shown character; at the text's end it
        // steps out of the span in place.
        assert_eq!(step(point(5), Direction::Forward, false).head, 8);
        let end = Concealed::new(&Rope::from_str("a **b**"), vec![span(2..4, 5..7)]);
        let out = end.moved(point(5), |text, range| {
            movement::move_horizontally(text, range, Direction::Forward, false)
        });
        assert_eq!(out.head, 7);
        // Extending keeps the anchor; the word with its marks.
        let range = step(point(1), Direction::Forward, true);
        assert_eq!((range.anchor, range.head), (1, 2));
        let range = step(range, Direction::Forward, true);
        assert_eq!((range.anchor, range.head), (1, 7));
        // ⌥→ by the shown words.
        let word = c.moved(point(1), |text, range| {
            movement::move_word(text, range, Direction::Forward, false)
        });
        assert_eq!(word.head, 7);
    }

    #[test]
    fn deleting_keeps_the_marks_balanced() {
        let (_, c) = bold();
        // ⌫ after the span: its last shown character, the caret stays after the marks.
        let (changes, caret) = c.delete(4, 5, "", 7);
        assert_eq!(applied("a **b** c", changes), "a  c");
        assert_eq!(caret, 2);
        // "a **bc** d": ⌫ of "c" leaves "b" bold.
        let text = "a **bc** d";
        let c = Concealed::new(&Rope::from_str(text), vec![span(2..4, 6..8)]);
        let (changes, caret) = c.delete(5, 6, "", 8);
        assert_eq!(applied(text, changes), "a **b** d");
        assert_eq!(caret, 7);
        // A selection from before the span into it: the opening marks stay.
        let (changes, caret) = c.delete(0, 5, "", 0);
        assert_eq!(applied(text, changes), "**c** d");
        assert_eq!(caret, 0);
        // Typing over the whole span: plain.
        let (changes, caret) = c.delete(2, 8, "x", 8);
        assert_eq!(applied(text, changes), "a x d");
        assert_eq!(caret, 3);
        // Typing over a part keeps the rest bold.
        let (changes, caret) = c.delete(4, 5, "x", 5);
        assert_eq!(applied(text, changes), "a **xc** d");
        assert_eq!(caret, 5);
    }

    #[test]
    fn nested_spans_left_empty_lose_their_marks() {
        // "**__a__**": deleting "a" takes both pairs.
        let text = "**__a__**";
        let c = Concealed::new(
            &Rope::from_str(text),
            vec![span(0..2, 7..9), span(2..4, 5..7)],
        );
        let (changes, caret) = c.delete(4, 5, "", 9);
        assert_eq!(applied(text, changes), "");
        assert_eq!(caret, 0);
    }

    #[test]
    fn a_copy_carries_balanced_markdown() {
        let text = Rope::from_str("a **bold** [link](https://x.y) c");
        let c = Concealed::new(&text, vec![span(2..4, 8..10), span(11..12, 16..30)]);
        assert_eq!(c.shown.to_string(), "a bold link c");
        // "ol" of the bold word.
        assert_eq!(c.markdown(&text, 5, 7), "**ol**");
        assert_eq!(c.shown_text(5, 7), "ol");
        // From inside the bold word into the link.
        assert_eq!(c.markdown(&text, 6, 14), "**ld** [li](https://x.y)");
        assert_eq!(c.shown_text(6, 14), "ld li");
        // A whole span: as it is.
        assert_eq!(c.markdown(&text, 2, 10), "**bold**");
    }
}
