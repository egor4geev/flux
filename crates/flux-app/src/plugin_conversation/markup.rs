//! The field's Markdown as it is typed (API 0.5, part 5): the looks of what the field's formatting
//! writes — `**bold**`, `__italic__`, `~~strikethrough~~`, `||spoiler||`, `` `code` ``,
//! ```` ``` ```` blocks, `[text](address)` — for [`PromptInput::set_styling`]. The rules are
//! those of the Markdown the plugin parses the sent text with (FluxGram: TDLib's `parseMarkdown`,
//! and the fake world's copy of it), so what the user sees while typing is what is sent: code
//! first — a run of one or three backticks to the next run of as many, a block's language on its
//! first line, nothing inside is markup; then the links (`[text](address)`, the text on one line,
//! the address without spaces) and the double marks, each from a mark to the next same mark. A
//! mark without its pair is plain text, shown as typed. The marks of a span are hidden — the
//! field shows the formatted text alone, as the messengers' fields do: typing the second `**`
//! turns the word bold and the asterisks vanish. A block's fences hide with its language; their
//! lines stay, empty, above and below the block's code (where the caret types before or after
//! the block).
//!
//! [`PromptInput::set_styling`]: crate::prompt_input::PromptInput::set_styling

use std::ops::Range;

use crate::prompt_input::{Look, Markup, Span};

/// The double marks, in the order they are tried: bold, italic, strikethrough, spoiler.
const MARKS: [char; 4] = ['*', '_', '~', '|'];

/// What a span of the field's Markdown makes of its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bold,
    Italic,
    Strikethrough,
    Spoiler,
    Code,
    Link,
}

impl Kind {
    /// The look has it.
    pub fn of(self, look: Look) -> bool {
        match self {
            Kind::Bold => look.bold,
            Kind::Italic => look.italic,
            Kind::Strikethrough => look.strikethrough,
            Kind::Spoiler => look.spoiler,
            Kind::Code => look.code,
            Kind::Link => look.link,
        }
    }
}

/// The field's Markdown read: each character's look, and the spans with their kinds, in the
/// order they are found.
pub struct Parsed {
    pub each: Vec<Look>,
    pub spans: Vec<(Kind, Span)>,
}

/// The field's markup of `text` ([`PromptInput::set_styling`]): the stretches of equal looks
/// (character ranges in order, not overlapping) and the spans, whose marks the field hides.
///
/// [`PromptInput::set_styling`]: crate::prompt_input::PromptInput::set_styling
pub fn markup(text: &str) -> Markup {
    let Parsed { each, spans } = parse(text);
    let mut looks: Vec<(Range<usize>, Look)> = Vec::new();
    for (at, look) in each.into_iter().enumerate() {
        if look == Look::default() {
            continue;
        }
        match looks.last_mut() {
            Some((range, last)) if range.end == at && *last == look => range.end = at + 1,
            _ => looks.push((at..at + 1, look)),
        }
    }
    Markup {
        looks,
        spans: spans.into_iter().map(|(_, span)| span).collect(),
    }
}

/// `text` as the field shows it: the marks of its spans left out (the editing bar quotes the
/// message so).
pub fn shown(text: &str) -> String {
    let mut hidden = vec![false; text.chars().count()];
    for (_, span) in parse(text).spans {
        for at in span.open.chain(span.close) {
            hidden[at] = true;
        }
    }
    text.chars()
        .zip(hidden)
        .filter_map(|(c, hidden)| (!hidden).then_some(c))
        .collect()
}

/// Reads `text`'s Markdown.
pub fn parse(text: &str) -> Parsed {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut each = vec![Look::default(); n];
    let mut spans = Vec::new();
    // Code and its fences, a link's brackets and address: no marks there.
    let mut plain = vec![false; n];

    // Code first: nothing inside it is markup.
    let mut i = 0;
    while i < n {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let run = run_of(&chars, i);
        let close = (run == 1 || run == 3)
            .then(|| next_run(&chars, i + run, run))
            .flatten();
        let Some(close) = close else {
            i += run;
            continue;
        };
        let mut body = i + run;
        if run == 3 {
            // A block's language: a word alone on its first line, hidden with the fence.
            let word = chars[body..close]
                .iter()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '+' | '-'))
                .count();
            if word > 0 && chars.get(body + word) == Some(&'\n') {
                body += word;
            }
        }
        spans.push((
            Kind::Code,
            Span {
                open: i..body,
                close: close..close + run,
            },
        ));
        each[body..close]
            .iter_mut()
            .for_each(|look| look.code = true);
        plain[i..close + run].iter_mut().for_each(|at| *at = true);
        i = close + run;
    }

    // The links: `[` and `](address)` hidden, the text a link (its marks are parsed).
    let mut i = 0;
    while i < n {
        if chars[i] != '[' || plain[i] {
            i += 1;
            continue;
        }
        let Some((bracket, end)) = link_at(&chars, &plain, i) else {
            i += 1;
            continue;
        };
        spans.push((
            Kind::Link,
            Span {
                open: i..i + 1,
                close: bracket..end + 1,
            },
        ));
        each[i + 1..bracket]
            .iter_mut()
            .for_each(|look| look.link = true);
        plain[i] = true;
        plain[bracket..=end].iter_mut().for_each(|at| *at = true);
        i = end + 1;
    }

    // The double marks: from a mark to the next same one.
    let mut open: [Option<usize>; 4] = [None; 4];
    let mark_at = |at: usize| {
        (at + 1 < n && !plain[at] && !plain[at + 1] && chars[at] == chars[at + 1])
            .then(|| MARKS.iter().position(|&mark| mark == chars[at]))
            .flatten()
    };
    // Where each kind's last mark starts: a mark before it has a pair to come.
    let mut last = [None; 4];
    for at in 0..n {
        if let Some(kind) = mark_at(at) {
            last[kind] = Some(at);
        }
    }
    let mut i = 0;
    while i < n {
        let Some(kind) = mark_at(i) else {
            i += 1;
            continue;
        };
        match open[kind] {
            Some(start) => {
                let kind_of = [Kind::Bold, Kind::Italic, Kind::Strikethrough, Kind::Spoiler];
                spans.push((
                    kind_of[kind],
                    Span {
                        open: start..start + 2,
                        close: i..i + 2,
                    },
                ));
                for look in &mut each[start + 2..i] {
                    match kind {
                        0 => look.bold = true,
                        1 => look.italic = true,
                        2 => look.strikethrough = true,
                        _ => look.spoiler = true,
                    }
                }
                open[kind] = None;
            }
            // It opens only when the same mark comes later.
            None if last[kind].is_some_and(|at| at >= i + 2) => open[kind] = Some(i),
            None => {}
        }
        i += 2;
    }

    // A mark's own characters aren't its span's text (nested marks are hidden).
    for (_, span) in &spans {
        for at in span.open.clone().chain(span.close.clone()) {
            each[at] = Look::default();
        }
    }
    Parsed { each, spans }
}

/// The length of the run of backticks at `at`.
fn run_of(chars: &[char], at: usize) -> usize {
    chars[at..].iter().take_while(|&&c| c == '`').count()
}

/// The next run of exactly `len` backticks from `from`.
fn next_run(chars: &[char], from: usize, len: usize) -> Option<usize> {
    let mut at = from;
    while at < chars.len() {
        if chars[at] != '`' {
            at += 1;
            continue;
        }
        let run = run_of(chars, at);
        if run == len {
            return Some(at);
        }
        at += run;
    }
    None
}

/// A link at `at` (its `[`): where its `]` is and its `)`. The text is on one line and isn't
/// empty, the address has no spaces; neither is in code.
fn link_at(chars: &[char], plain: &[bool], at: usize) -> Option<(usize, usize)> {
    let free = |i: usize| !plain[i];
    let mut bracket = at + 1;
    while bracket < chars.len() && free(bracket) && !matches!(chars[bracket], ']' | '\n') {
        bracket += 1;
    }
    if bracket == at + 1 || chars.get(bracket) != Some(&']') || chars.get(bracket + 1) != Some(&'(')
    {
        return None;
    }
    let mut end = bracket + 2;
    while end < chars.len() && free(end) && chars[end] != ')' && !chars[end].is_whitespace() {
        end += 1;
    }
    (end > bracket + 2 && chars.get(end) == Some(&')') && free(end)).then_some((bracket, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formatted stretches as (the text, what it looks like): "b" bold, "i" italic,
    /// "s" struck, "p" spoiler, "c" code, "l" link.
    fn shown(text: &str) -> Vec<(String, String)> {
        let chars: Vec<char> = text.chars().collect();
        markup(text)
            .looks
            .into_iter()
            .map(|(range, look)| {
                let mut kind = String::new();
                for (on, letter) in [
                    (look.bold, 'b'),
                    (look.italic, 'i'),
                    (look.strikethrough, 's'),
                    (look.spoiler, 'p'),
                    (look.code, 'c'),
                    (look.link, 'l'),
                ] {
                    if on {
                        kind.push(letter);
                    }
                }
                (chars[range].iter().collect(), kind)
            })
            .collect()
    }

    /// The spans' hidden marks as (the opening, the closing), in the order they are found.
    fn marks(text: &str) -> Vec<(String, String)> {
        let chars: Vec<char> = text.chars().collect();
        markup(text)
            .spans
            .into_iter()
            .map(|span| {
                (
                    chars[span.open].iter().collect(),
                    chars[span.close].iter().collect(),
                )
            })
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(text, kind)| (text.to_string(), kind.to_string()))
            .collect()
    }

    #[test]
    fn each_mark_styles_its_text_and_hides() {
        let text = "a **жирный** и __курсив__";
        assert_eq!(shown(text), pairs(&[("жирный", "b"), ("курсив", "i")]));
        assert_eq!(marks(text), pairs(&[("**", "**"), ("__", "__")]));
        assert_eq!(
            shown("~~old~~ ||secret 🤫||"),
            pairs(&[("old", "s"), ("secret 🤫", "p")])
        );
        let text = "see [the docs](https://a.b/c__d__e) now";
        assert_eq!(shown(text), pairs(&[("the docs", "l")]));
        assert_eq!(marks(text), pairs(&[("[", "](https://a.b/c__d__e)")]));
        assert_eq!(super::shown(text), "see the docs now");
        assert_eq!(super::shown("**a** and **b"), "a and **b");
    }

    #[test]
    fn marks_nest_and_cross_lines() {
        let text = "**bold __both__**";
        assert_eq!(shown(text), pairs(&[("bold ", "b"), ("both", "bi")]));
        assert_eq!(marks(text), pairs(&[("__", "__"), ("**", "**")]));
        assert_eq!(shown("**one\ntwo**"), pairs(&[("one\ntwo", "b")]));
        let text = "[**Anna**](tg://user?id=42) ";
        assert_eq!(shown(text), pairs(&[("Anna", "bl")]));
        assert_eq!(
            marks(text),
            pairs(&[("[", "](tg://user?id=42)"), ("**", "**")])
        );
    }

    #[test]
    fn unclosed_marks_stay_plain() {
        for text in ["**bold", "a __b ~~c ||d `e [f](g h)", "[](x) [a]() [a] (b)"] {
            assert!(shown(text).is_empty() && marks(text).is_empty(), "{text}");
        }
        // Only the pair's marks: the third one waits for a fourth.
        assert_eq!(shown("**a** **b"), pairs(&[("a", "b")]));
        assert_eq!(marks("**a** **b"), pairs(&[("**", "**")]));
        // A pair with nothing between (what ⌘B puts at the caret) is a span too.
        assert_eq!(marks("x ****"), pairs(&[("**", "**")]));
    }

    #[test]
    fn code_holds_no_markup() {
        let text = "run `cargo **fmt**` now";
        assert_eq!(shown(text), pairs(&[("cargo **fmt**", "c")]));
        assert_eq!(marks(text), pairs(&[("`", "`")]));
        // A block's fences hide with its language; its lines are code.
        let text = "```rust\nfn main() {}\n```";
        assert_eq!(shown(text), pairs(&[("\nfn main() {}\n", "c")]));
        assert_eq!(marks(text), pairs(&[("```rust", "```")]));
        // Two backticks aren't code; a run of three closes only at three.
        assert!(shown("a `` b").is_empty());
        assert_eq!(shown("```a ` b```"), pairs(&[("a ` b", "c")]));
        // A mark whose pair is in code doesn't open.
        assert_eq!(shown("**a `**`"), pairs(&[("**", "c")]));
        assert_eq!(marks("**a `**`"), pairs(&[("`", "`")]));
    }
}
