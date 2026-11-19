//! A poll in a message (API 0.5, FluxGram part 5): the `poll` part — the question in bold, the
//! dim line of its kind ("Anonymous poll", "Quiz") under it, the options, the footer ("12
//! votes"). While it isn't answered each option is a row with a radio (a checkbox when several may
//! be chosen): a click on a row votes, or — several — checks it, and Vote by the footer sends the
//! checked ones; a vote comes as `poll-voted(message, part, options)` and the plugin puts the poll
//! again, answered. Answered (or closed), each row has its share on the left and a thin bar of it
//! under the text in the accent; the user's choice is marked ✓; a quiz's right answer is in the
//! success color, the user's wrong one in the error color; a quiz's explanation opens under the
//! options by the 💡 by the footer.
//!
//! What the user checked before Vote and the explanations opened are kept here by (message,
//! part); either changes the bubble's height, so the message is laid out again. Nothing asks for
//! frames.

use std::collections::{BTreeSet, HashMap, HashSet};

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::{PollOption, PollSpec, RichSpan};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, MouseButton, SharedString, StyledText, div,
    prelude::*, px, relative,
};

use super::{Conversation, LINE_HEIGHT, TEXT_SIZE, rich_runs};
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::theme::{self, UiColors};
use crate::ui;

/// A poll's width in its bubble (up to the bubble's own): its rows and bars read best not too
/// wide, as in the messengers.
pub(super) const WIDTH: f32 = 300.;
/// The radio or the checkbox before an option.
const MARK: f32 = 18.;
/// The share on an answered option's left: "100%".
const SHARE: f32 = 38.;
/// The bar under an answered option's text.
const BAR: f32 = 3.;

/// What the user does with the polls of a conversation before the plugin hears of it.
#[derive(Debug, Default)]
pub(super) struct Polls {
    /// The options checked in a poll of several choices, by (message, part), until Vote.
    checked: HashMap<(String, usize), BTreeSet<u32>>,
    /// The quizzes whose explanation is open, by (message, part).
    explained: HashSet<(String, usize)>,
}

impl Polls {
    /// The message is gone or came again answered: what was checked in it goes.
    pub(super) fn forget(&mut self, message: &str) {
        self.checked.retain(|(owner, _), _| owner != message);
    }
}

/// How an option of an answered poll is marked.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verdict {
    /// Neither chosen nor a quiz's answer: the accent bar, no mark.
    Plain,
    /// The user's choice: ✓ and the accent.
    Chosen,
    /// A quiz's right answer: ✓ in the success color.
    Right,
    /// The user's wrong answer in a quiz: × in the error color.
    Wrong,
}

fn verdict(option: &PollOption, quiz: bool) -> Verdict {
    match (option.correct, option.chosen) {
        (true, _) => Verdict::Right,
        (false, true) if quiz => Verdict::Wrong,
        (false, true) => Verdict::Chosen,
        (false, false) => Verdict::Plain,
    }
}

impl Verdict {
    fn color(self, ui: UiColors) -> Hsla {
        match self {
            Verdict::Plain | Verdict::Chosen => ui.accent,
            Verdict::Right => ui.success,
            Verdict::Wrong => ui.error,
        }
    }
}

/// The options to send for a click on `option`: a single choice votes at once; several — the
/// checked ones change, and none goes yet.
fn click(multiple: bool, checked: &mut BTreeSet<u32>, option: u32) -> Option<Vec<u32>> {
    if !multiple {
        return Some(vec![option]);
    }
    if !checked.remove(&option) {
        checked.insert(option);
    }
    None
}

impl Conversation {
    /// The poll `spec`, the part `part` of the message `message`.
    pub(super) fn render_poll(
        &self,
        message: &str,
        part: usize,
        spec: &PollSpec,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (message.to_string(), part);
        let voting = !spec.answered && !spec.closed;
        let quiz = spec.options.iter().any(|option| option.correct);
        let checked = self.polls.checked.get(&key).cloned().unwrap_or_default();
        let options = spec
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                if voting {
                    let on = checked.contains(&(index as u32));
                    self.render_choice(message, part, index, option, spec.multiple, on, ui, cx)
                } else {
                    render_result(index, option, quiz, ui)
                }
            })
            .collect::<Vec<_>>();
        let vote = (voting && spec.multiple && !checked.is_empty()).then(|| {
            let message = message.to_string();
            div()
                .id(("fg-poll-vote", part))
                .flex_none()
                .px(px(10.))
                .py(px(2.))
                .rounded(px(ui::RADIUS_SM))
                .bg(ui.accent)
                .text_color(ui.on_accent)
                .text_size(px(theme::TEXT_SM))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(tr("Vote"))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let key = (message.clone(), part);
                    let Some(chosen) = this.polls.checked.remove(&key) else {
                        return;
                    };
                    this.relayout(&message);
                    let chosen = chosen.into_iter().collect();
                    this.emit(
                        UiEvent::PollVoted((message.clone(), part as u32, chosen)),
                        cx,
                    );
                    cx.notify();
                }))
        });
        let explained = self.polls.explained.contains(&key);
        let bulb = spec
            .explanation
            .as_ref()
            .filter(|_| spec.answered)
            .map(|_| {
                let message = message.to_string();
                ui::toggle_button(("fg-poll-bulb", part), IconName::Bulb, explained, ui)
                    .tooltip(ui::tooltip(tr("Explanation"), None))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let key = (message.clone(), part);
                        if !this.polls.explained.remove(&key) {
                            this.polls.explained.insert(key);
                        }
                        this.relayout(&message);
                        cx.notify();
                    }))
            });
        let explanation = spec
            .explanation
            .as_ref()
            .filter(|_| spec.answered && explained)
            .map(|spans| {
                div()
                    .pl(px(8.))
                    .border_l_2()
                    .border_color(ui.warning)
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(styled(spans, false, ui))
            });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .text_size(px(TEXT_SIZE))
                    .line_height(px(LINE_HEIGHT))
                    .child(styled(&spec.question, true, ui)),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(spec.kind.clone()),
            )
            .child(
                div()
                    .mt(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(if voting { 0. } else { 6. }))
                    .children(options),
            )
            .child(
                div()
                    .mt(px(4.))
                    .min_h(px(24.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(spec.footer.clone()),
                    )
                    .children(vote)
                    .children(bulb),
            )
            .children(explanation)
            .into_any_element()
    }

    /// An option to vote for: the radio or the checkbox, the text; a click votes or checks it.
    #[allow(clippy::too_many_arguments)]
    fn render_choice(
        &self,
        message: &str,
        part: usize,
        index: usize,
        option: &PollOption,
        multiple: bool,
        on: bool,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message = message.to_string();
        let mark = div()
            .flex_none()
            .size(px(MARK))
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .map(|mark| {
                if multiple {
                    mark.rounded(px(4.))
                } else {
                    mark.rounded(px(MARK / 2.))
                }
            })
            .map(|mark| {
                if on {
                    mark.bg(ui.accent)
                        .border_color(ui.accent)
                        .child(icon(IconName::Check, ui.on_accent).size(px(12.)))
                } else {
                    mark.border_color(ui.dim)
                }
            });
        div()
            .id(SharedString::from(format!("fg-poll-{part}-{index}")))
            .flex()
            .items_center()
            .gap(px(10.))
            .mx(px(-6.))
            .px(px(6.))
            .py(px(5.))
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .hover(|style| style.bg(ui.hover))
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(TEXT_SIZE))
                    .line_height(px(LINE_HEIGHT))
                    .child(styled(&option.text, false, ui)),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let key = (message.clone(), part);
                let checked = this.polls.checked.entry(key.clone()).or_default();
                let was_empty = checked.is_empty();
                match click(multiple, checked, index as u32) {
                    Some(chosen) => {
                        this.polls.checked.remove(&key);
                        this.emit(
                            UiEvent::PollVoted((message.clone(), part as u32, chosen)),
                            cx,
                        );
                    }
                    // Vote comes and goes with the first check and the last.
                    None => {
                        let now_empty = this.polls.checked.get(&key).is_none_or(BTreeSet::is_empty);
                        if was_empty != now_empty {
                            this.relayout(&message);
                        }
                    }
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// The message's bubble changed its height: it is laid out again.
    fn relayout(&self, message: &str) {
        if let Some(ix) = self.position(message) {
            self.remeasure(ix);
        }
    }
}

/// An option of an answered poll: its share, the text and its mark, the bar under the text.
fn render_result(index: usize, option: &PollOption, quiz: bool, ui: UiColors) -> AnyElement {
    let verdict = verdict(option, quiz);
    let mark = match verdict {
        Verdict::Plain => None,
        Verdict::Chosen | Verdict::Right => Some(icon(IconName::Check, verdict.color(ui))),
        Verdict::Wrong => Some(icon(IconName::Close, verdict.color(ui))),
    };
    div()
        .id(("fg-poll-result", index))
        .flex()
        .gap(px(6.))
        .child(
            div()
                .flex_none()
                .w(px(SHARE))
                .text_size(px(theme::TEXT_SM))
                .line_height(px(LINE_HEIGHT))
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("{}%", option.percent.min(100))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(TEXT_SIZE))
                                .line_height(px(LINE_HEIGHT))
                                .child(styled(&option.text, false, ui)),
                        )
                        .children(mark.map(|mark| mark.size(px(14.)).flex_none())),
                )
                .child(
                    div().w_full().h(px(BAR)).child(
                        div()
                            .h_full()
                            .min_w(px(BAR))
                            .w(relative(f32::from(option.percent.min(100)) / 100.))
                            .rounded(px(BAR / 2.))
                            .bg(verdict.color(ui)),
                    ),
                ),
        )
        .into_any_element()
}

/// Rich spans as one styled text (bold all through for the question).
fn styled(spans: &[RichSpan], bold: bool, ui: UiColors) -> StyledText {
    let (text, mut runs, ..) = rich_runs(spans, true, ui);
    if bold {
        for run in &mut runs {
            run.font.weight = FontWeight::BOLD;
        }
    }
    StyledText::new(text).with_runs(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(chosen: bool, correct: bool) -> PollOption {
        PollOption {
            text: Vec::new(),
            percent: 50,
            chosen,
            correct,
        }
    }

    #[test]
    fn a_single_choice_votes_at_once_several_wait_for_vote() {
        let mut checked = BTreeSet::new();
        assert_eq!(click(false, &mut checked, 2), Some(vec![2]));
        assert!(checked.is_empty());
        assert_eq!(click(true, &mut checked, 2), None);
        assert_eq!(click(true, &mut checked, 0), None);
        assert_eq!(checked.iter().copied().collect::<Vec<_>>(), [0, 2]);
        // A second click clears it.
        assert_eq!(click(true, &mut checked, 2), None);
        assert_eq!(checked.iter().copied().collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn answered_options_are_marked_as_the_poll_kind_says() {
        assert_eq!(verdict(&option(false, false), false), Verdict::Plain);
        assert_eq!(verdict(&option(true, false), false), Verdict::Chosen);
        // A quiz: the right answer, chosen or not; the user's wrong one.
        assert_eq!(verdict(&option(true, true), true), Verdict::Right);
        assert_eq!(verdict(&option(false, true), true), Verdict::Right);
        assert_eq!(verdict(&option(true, false), true), Verdict::Wrong);
        assert_eq!(verdict(&option(false, false), true), Verdict::Plain);
        let ui = crate::theme::Theme::flux_night().ui;
        assert_eq!(Verdict::Right.color(ui), ui.success);
        assert_eq!(Verdict::Wrong.color(ui), ui.error);
        assert_eq!(Verdict::Chosen.color(ui), ui.accent);
    }

    #[test]
    fn forgetting_a_message_drops_what_was_checked_in_it() {
        let mut polls = Polls::default();
        polls
            .checked
            .insert(("m1".into(), 0), BTreeSet::from([1u32]));
        polls
            .checked
            .insert(("m2".into(), 0), BTreeSet::from([0u32]));
        polls.forget("m1");
        assert_eq!(polls.checked.len(), 1);
        assert!(polls.checked.contains_key(&("m2".to_string(), 0)));
    }
}
