//! The tour (five slides): a way of working in Flux, its keys, and a picture Flux draws in the
//! chosen theme ([`super::scenes`]). Each slide has a live task — "Try it: press ⌘P": the keys are
//! bound in the guide's own context to [`super::TryTask`], and pressing them plays the picture's
//! little show (the search opens in it) and marks the task done. The picture also plays once by
//! itself a moment after the slide shows.
//!
//! The keys a slide lists and the keys of its task are read from the keymap — the bindings of the
//! actions themselves — so the tour teaches the keys Flux answers to.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, App, BoxShadow, Context, Div, FontWeight, KeyBinding, SharedString, Window,
    div, point, prelude::*, px,
};

use super::{CONTEXT, Tour, TryTask, WelcomeGuide, heading, reduce_motion, scenes};
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::theme::{Theme, UiColors};
use crate::{
    claude, code_actions, command_palette, file_finder, file_tree, git, navigation,
    project_search, quick_switch, terminal_panel, ui,
};

/// How long after a tour slide shows its picture plays by itself.
const AUTOPLAY_DELAY: Duration = Duration::from_millis(800);

/// How long a picture's show runs: long enough to read what it types and shows — the commit's
/// message and Claude's steps take longer than a popup.
pub fn show_duration(tour: Tour) -> Duration {
    Duration::from_millis(match tour {
        Tour::Search | Tour::Windows => 1600,
        Tour::Code => 1400,
        Tour::Git | Tour::Claude => 2400,
    })
}

/// The left column: the heading, the keys, the task.
const TEXT_WIDTH: f32 = 330.;
/// The keycaps' column of a key row: the keys end at its right edge, the labels line up after it.
const KEYS_WIDTH: f32 = 86.;
const KEY_ROW_HEIGHT: f32 = 36.;

/// The live tasks: which are done, and when each picture's show started.
#[derive(Default)]
pub struct Tasks {
    /// Tasks done (the user pressed the keys), with when.
    pub done: HashMap<Tour, Instant>,
    /// When each picture's show starts: by itself a moment after its slide shows (a moment in the
    /// future then), or at once by the task's keys.
    pub played: HashMap<Tour, Instant>,
}

impl Tasks {
    /// The slide of `tour` shows: its picture goes back to the start and plays by itself a moment
    /// later.
    pub fn shown(&mut self, tour: Tour) {
        self.played.insert(tour, Instant::now() + AUTOPLAY_DELAY);
    }

    /// The task's keys were pressed: the task is done, and the picture plays from the start.
    pub fn try_task(&mut self, tour: Tour) {
        let now = Instant::now();
        self.done.insert(tour, now);
        self.played.insert(tour, now);
    }

    /// How far the picture's show is: 0 — before, 1 — over.
    pub fn progress(&self, tour: Tour) -> f32 {
        progress_at(
            self.played.get(&tour).copied(),
            show_duration(tour),
            Instant::now(),
            reduce_motion(),
        )
    }

    /// A show is about to start or is running: the window asks for the next frame.
    pub fn animating(&self) -> bool {
        let now = Instant::now();
        let reduce = reduce_motion();
        self.played.iter().any(|(tour, start)| {
            let length = if reduce { Duration::ZERO } else { show_duration(*tour) };
            // A frame past the end, so the last one is drawn at 1.
            now <= *start + length + Duration::from_millis(50)
        })
    }
}

/// The progress at `now` of a show `length` long starting at `start`; with "Reduce motion" it
/// jumps from 0 to 1 when it starts.
fn progress_at(start: Option<Instant>, length: Duration, now: Instant, reduce_motion: bool) -> f32 {
    let Some(elapsed) = start.and_then(|start| now.checked_duration_since(start)) else {
        return 0.;
    };
    if reduce_motion {
        return 1.;
    }
    (elapsed.as_secs_f32() / length.as_secs_f32()).min(1.)
}

/// What a slide teaches: the action of its task (its keys play the picture).
fn task_action(tour: Tour) -> Box<dyn Action> {
    match tour {
        Tour::Search => Box::new(file_finder::Toggle),
        Tour::Code => Box::new(code_actions::ShowContextActions),
        Tour::Git => Box::new(git::Commit),
        Tour::Claude => Box::new(claude::ToggleClaude),
        Tour::Windows => Box::new(file_tree::ToggleOpen),
    }
}

/// Binds each task's keys (the keys of the action the slide teaches) to [`super::TryTask`] in the
/// guide's key context. Runs after the other parts bound their keys (`welcome::init` comes last).
pub fn bind_keys(cx: &mut App) {
    let bindings: Vec<KeyBinding> = Tour::ALL
        .into_iter()
        .filter_map(|tour| {
            let binding = ui::action_binding(task_action(tour).as_ref(), cx)?;
            let keys = binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.unparse())
                .collect::<Vec<_>>()
                .join(" ");
            Some(KeyBinding::new(&keys, TryTask(tour), Some(CONTEXT)))
        })
        .collect();
    cx.bind_keys(bindings);
}

/// A slide's words: its title, what it says, the keys it lists (an action and what it does) and
/// what the task's success says (`{0}` — the task's keys).
struct Copy {
    title: &'static str,
    lead: SharedString,
    keys: Vec<(Box<dyn Action>, &'static str)>,
    done: &'static str,
}

fn copy(tour: Tour, cx: &App) -> Copy {
    match tour {
        Tour::Search => Copy {
            title: tr("Everything from the Keyboard"),
            lead: tr(
                "A file, a line in the project, a command, a theme — all found from the keyboard. Every action that has keys shows them.",
            )
            .into(),
            keys: vec![
                (Box::new(file_finder::Toggle), tr("Find File")),
                (Box::new(project_search::Toggle), tr("Find in Files")),
                (Box::new(command_palette::Toggle), tr("Command Palette")),
                (Box::new(quick_switch::Toggle), tr("Theme, Look and Language")),
            ],
            done: tr("{0} opens the file search in any Flux window."),
        },
        Tour::Code => Copy {
            title: tr("Flux Understands Your Code"),
            lead: tr(
                "Flux installs and runs the language servers itself: problems as you type, completion, going to a definition, renaming across the project.",
            )
            .into(),
            keys: vec![
                (Box::new(navigation::GoToDefinition), tr("Go To Definition")),
                (Box::new(navigation::FindUsages), tr("Find Usages")),
                (Box::new(navigation::RenameSymbol), tr("Rename")),
                (Box::new(code_actions::ShowContextActions), tr("Fixes and Actions")),
            ],
            done: tr("{0} shows the fixes and actions at the caret."),
        },
        Tour::Git => Copy {
            title: tr("Version Control in Windows"),
            lead: tr(
                "Committing in parts, branches, the log with its graph, stash, rebase, conflicts and annotations — in Flux's windows, without a terminal.",
            )
            .into(),
            keys: vec![
                (Box::new(git::Commit), tr("Commit")),
                (Box::new(git::Push), tr("Push")),
                (Box::new(git::UpdateProject), tr("Update Project")),
                (Box::new(git::Branches), tr("Branches")),
            ],
            done: tr("{0} opens the commit window with the focus in the message."),
        },
        Tour::Claude => Copy {
            title: tr("Claude Code Beside Your Code"),
            lead: tr(
                "Sessions in tabs, edits as cards with their diff, and at the end of a turn — a summary with review and rollback. Subagents are seen live.",
            )
            .into(),
            keys: vec![
                (Box::new(claude::ToggleClaude), tr("Open the Chat")),
                (Box::new(claude::AddSelectionToClaude), tr("Selection to the Chat")),
                (Box::new(code_actions::ShowContextActions), tr("Fix with Claude")),
            ],
            done: tr("{0} opens the chat, and takes you back to the editor."),
        },
        Tour::Windows => Copy {
            title: tr("Tool Windows"),
            lead: match ui::shortcut_for(&terminal_panel::HidePanel, cx) {
                Some(hide) => trf(
                    "On the left is the strip of tool windows: the project, the commit, search, the terminal, version control. Each opens and hides with its keys; {0} hides the one you're in.",
                    &[&hide],
                )
                .into(),
                None => tr(
                    "On the left is the strip of tool windows: the project, the commit, search, the terminal, version control. Each opens and hides with its keys.",
                )
                .into(),
            },
            keys: vec![
                (Box::new(file_tree::ToggleOpen), tr("Project")),
                (Box::new(git::ToggleCommitWindow), tr("Commit")),
                (Box::new(terminal_panel::TogglePanel), tr("Terminal")),
                (Box::new(git::ToggleGitWindow), tr("Git")),
            ],
            done: tr("{0} opens and hides the project tree."),
        },
    }
}

pub fn render(
    tour: Tour,
    guide: &mut WelcomeGuide,
    _window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let copy = copy(tour, cx);
    let number = Tour::ALL.iter().position(|t| *t == tour).unwrap_or(0) + 1;
    let eyebrow = trf("Tour · {0} of {1}", &[&number, &Tour::ALL.len()]);
    let rows = copy.keys.iter().filter_map(|(action, label)| {
        let keys = ui::shortcut_for(action.as_ref(), cx)?;
        Some(key_row(&keys, label, ui))
    });
    let done = guide.tasks.done.contains_key(&tour);
    let text = div()
        .w(px(TEXT_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .child(heading(eyebrow, copy.title, copy.lead, ui))
        .child(div().mt(px(22.)).ml(px(-14.)).flex().flex_col().children(rows))
        .children(task(tour, copy.done, done, ui, cx));
    let picture = div()
        .flex_1()
        .min_w_0()
        .pt(px(12.))
        .flex()
        .justify_center()
        .child(scenes::scene(tour, guide.tasks.progress(tour), cx));
    div()
        .size_full()
        .relative()
        .child(glow(ui))
        .child(
            div()
                .size_full()
                .pt(px(34.))
                .pl(px(36.))
                .pr(px(20.))
                .flex()
                .items_start()
                .gap(px(14.))
                .child(text)
                .child(picture),
        )
        .into_any_element()
}

/// A soft light of the accent behind the picture: the shadow of an empty circle, blurred wide (gpui
/// has no radial gradients).
fn glow(ui: UiColors) -> Div {
    const SIZE: f32 = 320.;
    div()
        .absolute()
        .top(px(256. - SIZE / 2.))
        .right(px(300. - SIZE / 2.))
        .size(px(SIZE))
        .rounded(px(SIZE / 2.))
        .shadow(vec![BoxShadow {
            color: UiColors::tint(ui.accent, 0.26),
            offset: point(px(0.), px(0.)),
            blur_radius: px(170.),
            spread_radius: px(50.),
        }])
}

/// A key row: the keycaps at the right edge of their column, what they do after them.
fn key_row(keys: &str, label: &str, ui: UiColors) -> Div {
    div()
        .h(px(KEY_ROW_HEIGHT))
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(KEYS_WIDTH))
                .flex_none()
                .flex()
                .justify_end()
                .child(ui::keys(keys, ui)),
        )
        .child(div().text_size(px(13.5)).child(label.to_string()))
}

/// The live task: "Try it yourself: press ⌘P", and once the keys are pressed, what they did.
/// Nothing when the action has no keys.
fn task(tour: Tour, done_text: &str, done: bool, ui: UiColors, cx: &App) -> Option<Div> {
    let keys = ui::shortcut_for(task_action(tour).as_ref(), cx)?;
    let card = div()
        .mt(px(18.))
        .px(px(12.))
        .py(px(10.))
        .flex()
        .gap(px(10.))
        .rounded(px(10.))
        .border_1()
        .text_size(px(12.5))
        .line_height(px(18.));
    Some(if done {
        card.bg(UiColors::tint(ui.success, 0.12))
            .border_color(UiColors::tint(ui.success, 0.35))
            .child(icon(IconName::CheckCircle, ui.success).size(px(16.)).mt(px(1.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("You got it.")),
                    )
                    .child(
                        div()
                            .text_color(ui.text_muted)
                            .child(trf(done_text, &[&keys])),
                    ),
            )
    } else {
        card.items_center()
            .bg(ui.accent_soft)
            .border_color(UiColors::tint(ui.accent, 0.45))
            .child(icon(IconName::Sparkle, ui.accent_text).size(px(15.)))
            .child(tr("Try it yourself: press"))
            .child(ui::keys(&keys, ui))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_show_starts_after_its_moment_and_runs_to_the_end() {
        let start = Instant::now();
        let show = show_duration(Tour::Search);
        assert_eq!(progress_at(None, show, start, false), 0.);
        // Waiting for the show that plays by itself.
        assert_eq!(progress_at(Some(start + AUTOPLAY_DELAY), show, start, false), 0.);
        let half = progress_at(Some(start), show, start + show / 2, false);
        assert!((half - 0.5).abs() < 0.01);
        assert_eq!(progress_at(Some(start), show, start + show * 2, false), 1.);
        // "Reduce motion": no in-between.
        assert_eq!(progress_at(Some(start), show, start + show / 4, true), 1.);
        assert_eq!(progress_at(Some(start + AUTOPLAY_DELAY), show, start, true), 0.);
        // The pictures that type and step through more get more time.
        assert!(show_duration(Tour::Git) > show_duration(Tour::Search));
        assert!(show_duration(Tour::Claude) >= Duration::from_millis(2200));
    }

    #[test]
    fn pressing_the_keys_marks_the_task_and_replays_the_picture() {
        let mut tasks = Tasks::default();
        tasks.shown(Tour::Git);
        assert!(tasks.animating());
        assert_eq!(tasks.progress(Tour::Git), 0.);
        assert!(!tasks.done.contains_key(&Tour::Git));
        tasks.try_task(Tour::Git);
        assert!(tasks.done.contains_key(&Tour::Git));
        assert!(tasks.played[&Tour::Git] <= Instant::now());
    }
}
