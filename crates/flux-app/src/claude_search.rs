//! The search in a Claude chat (⌘F, part 10.2), as in JetBrains' AI Chat: a bar at the top of the
//! chat; the matches in what the conversation shows — the user's messages, Claude's text, commands
//! and their output, the rows of Claude's steps, subagents' answers — marked, the current one
//! brighter. As in the terminal's search, the newest match comes first and ↵ goes to the one above
//! it (older), ⇧↵ to the one below; ⌘G / ⇧⌘G the same. A folded group of steps or a clipped output
//! opens when the current match is inside it. In a subagent's view it searches the subagent's
//! conversation. Case-insensitive, no options: a chat isn't code.
//!
//! The transcript draws the marks ([`SearchMarks`] in its state, `markdown::Marks` per entry); what
//! is counted here ([`entry_texts`]) is what it marks, in the same order.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use flux_claude::{Entry, EntryId, EntryKind, ShellEntry, TaskItem, ToolCall, ToolEntry};
use gpui::{
    Action, AnyElement, AppContext, ClickEvent, Context, Entity, FocusHandle, Focusable,
    Subscription, Window, canvas, div, prelude::*, px,
};

use crate::claude_chat::ClaudeChat;
use crate::claude_transcript;
use crate::find_bar;
use crate::i18n::tr;
use crate::icons::IconName;
use crate::input::{InputEvent, TextInput};
use crate::markdown;
use crate::motion;
use crate::theme::{self, Theme};
use crate::ui;

/// The search of one chat while its bar is open.
pub struct ChatSearch {
    query: Entity<TextInput>,
    /// The query of the last search, lowercased.
    needle: Vec<char>,
    /// The entries with matches, in the conversation's order, and how many each has.
    found: Vec<(EntryId, usize)>,
    total: usize,
    /// The current match, counted from the top of the conversation.
    current: Option<usize>,
    /// The conversation changed: search again before the next frame.
    stale: bool,
    _subscription: Subscription,
}

/// The bar sliding down from the top of the chat as it opens and back up as it closes, as the
/// file's find bar does (`find_bar`): how much of it shows, from and to, since when; the bar's
/// height as last laid out; a search that closed, drawn without input until its bar has slid away
/// (its marks and the focus have gone at once).
pub struct SearchSlide {
    reveal: (f32, f32, Instant),
    height: Rc<Cell<f32>>,
    closed: Option<ChatSearch>,
}

impl Default for SearchSlide {
    fn default() -> Self {
        Self {
            reveal: (0., 0., Instant::now()),
            height: Rc::default(),
            closed: None,
        }
    }
}

impl SearchSlide {
    /// How far the slide has come (0 → 1) and how much of the bar shows.
    fn revealed(&self) -> (f32, f32) {
        let (from, to, since) = self.reveal;
        let (t, _) = motion::SMOOTH.at(since.elapsed());
        (t, motion::lerp(from, to, t))
    }

    /// The bar opens or closes: it slides from where it is.
    fn slide(&mut self, open: bool) {
        let (_, shown) = self.revealed();
        self.reveal = (shown, if open { 1. } else { 0. }, Instant::now());
    }
}

/// What the transcript marks: the query and the current match (its entry and its number among the
/// entry's own matches).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMarks {
    pub needle: Vec<char>,
    pub current: Option<(EntryId, usize)>,
}

impl ClaudeChat {
    /// ⌘F: opens the bar (or takes the keyboard back to it) with the query selected.
    pub fn deploy_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            // A bar still sliding away gives way to the new one, which slides from where it is.
            self.search_slide.closed = None;
            self.search_slide.slide(true);
            let query = cx.new(|cx| TextInput::new(tr("Find in Chat"), cx).icon(IconName::Search));
            let subscription = cx.subscribe_in(&query, window, Self::search_query_changed);
            self.search = Some(ChatSearch {
                query,
                needle: Vec::new(),
                found: Vec::new(),
                total: 0,
                current: None,
                stale: false,
                _subscription: subscription,
            });
        }
        if let Some(search) = &self.search {
            let query = search.query.clone();
            query.update(cx, |query, cx| query.select_all(cx));
            window.focus(&query.focus_handle(cx));
        }
        cx.notify();
    }

    /// Esc in the bar: closes it, the marks go, the keyboard returns to the message field (or to the
    /// subagent's view).
    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.take() else {
            return;
        };
        // The bar slides away, taking no input.
        self.search_slide.slide(false);
        self.search_slide.closed = motion::enabled().then_some(search);
        self.set_search_marks(None);
        if self.agents.is_empty() {
            self.focus_composer(window, cx);
        } else {
            window.focus(&self.focus_handle);
        }
        cx.notify();
    }

    pub fn search_is_open(&self) -> bool {
        self.search.is_some()
    }

    /// The conversation changed (or the view switched to a subagent and back): the matches are
    /// found again before the next frame.
    pub(crate) fn search_stale(&mut self) {
        if let Some(search) = &mut self.search {
            search.stale = true;
        }
    }

    fn search_query_changed(
        &mut self,
        _: &Entity<TextInput>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                self.run_search(true, cx);
                self.reveal_current(false, cx);
            }
        }
    }

    /// Searches the conversation in sight. `fresh` — a new query: the newest match becomes the
    /// current one; otherwise the current match keeps its place where it can.
    fn run_search(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let Some(search) = &self.search else {
            return;
        };
        let needle: Vec<char> = search
            .query
            .read(cx)
            .text()
            .to_lowercase()
            .chars()
            .collect();
        let previous = (!fresh).then(|| current_entry(search)).flatten();
        let found = {
            let session = self.session().read(cx);
            let model = session.model();
            let scope = self.shown_transcript().scope;
            let entries = claude_transcript::scope_entries(model, scope);
            find(entries, &model.info.cwd, &model.tasks, &needle)
        };
        let total: usize = found.iter().map(|(_, count)| count).sum();
        let current = if total == 0 {
            None
        } else if let Some((entry, local)) = previous {
            // The same match, if its entry still has it; else the nearest one before it.
            let mut index = 0;
            let mut kept = None;
            for (id, count) in &found {
                if *id == entry {
                    kept = Some(index + local.min(count - 1));
                    break;
                }
                index += count;
            }
            Some(kept.unwrap_or(total - 1))
        } else {
            Some(total - 1)
        };
        if let Some(search) = &mut self.search {
            search.needle = needle;
            search.found = found;
            search.total = total;
            search.current = current;
            search.stale = false;
        }
        let marks = self.search.as_ref().map(|search| SearchMarks {
            needle: search.needle.clone(),
            current: current_entry(search),
        });
        self.set_search_marks(marks);
        cx.notify();
    }

    /// Searches again if the conversation changed since the last search.
    pub(crate) fn refresh_search(&mut self, cx: &mut Context<Self>) {
        if self.search.as_ref().is_some_and(|search| search.stale) {
            self.run_search(false, cx);
        }
    }

    /// The match above the current one (`up`, older) or below it; around at the ends.
    pub fn step_search(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.total == 0 {
            return;
        }
        let current = search.current.unwrap_or(search.total - 1);
        search.current = Some(if up {
            current.checked_sub(1).unwrap_or(search.total - 1)
        } else if current + 1 >= search.total {
            0
        } else {
            current + 1
        });
        let marks = SearchMarks {
            needle: search.needle.clone(),
            current: current_entry(search),
        };
        self.set_search_marks(Some(marks));
        self.reveal_current(true, cx);
        cx.notify();
    }

    /// Brings the current match into view: gliding there (`glide`, a step to the next match), or at
    /// once — the first match of a query being typed follows the keys, as in the file's find bar.
    fn reveal_current(&mut self, glide: bool, cx: &mut Context<Self>) {
        let Some(entry) = self
            .search
            .as_ref()
            .and_then(current_entry)
            .map(|(entry, _)| entry)
        else {
            return;
        };
        let transcript = self.shown_transcript_mut();
        if glide {
            transcript.reveal(entry);
        } else {
            transcript.reveal_at_once(entry);
        }
        cx.notify();
    }

    /// The marks go to the transcript in sight; the others lose theirs.
    fn set_search_marks(&mut self, marks: Option<SearchMarks>) {
        let shown = self.agents.len();
        for (index, transcript) in self.transcripts_mut().enumerate() {
            let wanted = if index == shown { marks.clone() } else { None };
            if transcript.search != wanted {
                transcript.search = wanted;
                transcript.invalidate();
            }
        }
    }
}

/// The current match as its entry and its number among the entry's matches.
fn current_entry(search: &ChatSearch) -> Option<(EntryId, usize)> {
    let mut current = search.current?;
    for (id, count) in &search.found {
        if current < *count {
            return Some((*id, current));
        }
        current -= count;
    }
    None
}

/// The entries of the conversation that have matches of `needle`, with their counts.
pub(crate) fn find(
    entries: &[Entry],
    root: &Path,
    tasks: &[TaskItem],
    needle: &[char],
) -> Vec<(EntryId, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .filter_map(|entry| {
            let count: usize = entry_texts(entry, root, tasks)
                .iter()
                .map(|text| markdown::occurrences(text, needle).len())
                .sum();
            (count > 0).then_some((entry.id, count))
        })
        .collect()
}

/// The texts of an entry the transcript marks, in the order it draws them. Keep in step with the
/// transcript's marks (`Renderer::marks`).
pub(crate) fn entry_texts(entry: &Entry, root: &Path, tasks: &[TaskItem]) -> Vec<String> {
    match &entry.kind {
        EntryKind::User(user) => {
            let text = claude_transcript::bubble_text(&user.text);
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![text.to_string()]
            }
        }
        EntryKind::Text { text, .. } => markdown::block_texts(&markdown::parse(text)),
        EntryKind::Tool(tool) => tool_texts(tool, root, tasks),
        EntryKind::Shell(shell) => shell_texts(shell),
        _ => Vec::new(),
    }
}

fn tool_texts(tool: &ToolEntry, root: &Path, tasks: &[TaskItem]) -> Vec<String> {
    match &tool.call {
        // The card makes its body before its header.
        ToolCall::Bash { command, .. } => {
            let mut texts = claude_transcript::bash_body_texts(tool, command);
            texts.push(claude_transcript::first_line(command));
            texts
        }
        ToolCall::Agent { .. } => claude_transcript::agent_card_answer(tool)
            .into_iter()
            .collect(),
        ToolCall::Edit { .. } | ToolCall::Write { .. } | ToolCall::NotebookEdit { .. } => {
            Vec::new()
        }
        _ => {
            let detail = claude_transcript::tool_summary(tool, root, tasks).detail;
            if detail.is_empty() {
                Vec::new()
            } else {
                vec![detail]
            }
        }
    }
}

fn shell_texts(shell: &ShellEntry) -> Vec<String> {
    let mut texts = vec![claude_transcript::first_line(&shell.command)];
    let output = shell.output.trim_end();
    if !output.is_empty() {
        texts.push(output.to_string());
    }
    texts
}

// --- The bar ---

/// ⌘F, ⌘G and ⇧⌘G on the chat (the window's find bar actions, caught before the window's).
pub fn actions(root: gpui::Div, cx: &mut Context<ClaudeChat>) -> gpui::Div {
    root.on_action(
        cx.listener(|chat, _: &find_bar::Deploy, window, cx| chat.deploy_search(window, cx)),
    )
    .on_action(
        cx.listener(|chat, _: &find_bar::DeployReplace, window, cx| chat.deploy_search(window, cx)),
    )
    .on_action(cx.listener(|chat, _: &find_bar::FindNext, window, cx| {
        if chat.search_is_open() {
            chat.step_search(true, cx)
        } else {
            chat.deploy_search(window, cx)
        }
    }))
    .on_action(cx.listener(|chat, _: &find_bar::FindPrevious, window, cx| {
        if chat.search_is_open() {
            chat.step_search(false, cx)
        } else {
            chat.deploy_search(window, cx)
        }
    }))
}

/// The bar at the top of the chat while the search is open: the query, "3 / 12", the arrows,
/// close. It slides down as it opens and back up after it closed, taking no input then.
pub fn render_bar(
    chat: &mut ClaudeChat,
    window: &mut Window,
    cx: &mut Context<ClaudeChat>,
) -> Option<AnyElement> {
    let open = chat.search.is_some();
    let (t, shown) = chat.search_slide.revealed();
    if !open && (t >= 1. || shown <= 0.) {
        chat.search_slide.closed = None;
    }
    let search = chat.search.as_ref().or(chat.search_slide.closed.as_ref())?;
    let ui = Theme::ui(cx);
    let query = search.query.clone();
    let focus = query.focus_handle(cx);
    let tip = |label: &'static str, action: &dyn Action, focus: &FocusHandle| {
        ui::tooltip(tr(label), ui::shortcut_in(action, focus, window, cx))
    };
    // As the file's find bar counts: "3 of 12", "No results".
    let counter = (!search.needle.is_empty()).then(|| {
        let label = find_bar::counter_label(search.total, search.current, false);
        if search.total == 0 {
            div()
                .flex_none()
                .text_color(ui.error)
                .child(label)
                .into_any_element()
        } else {
            ui::badge(label, ui.accent_text).into_any_element()
        }
    });
    let has_matches = search.total > 0;
    let row = div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().flex_1().min_w(px(120.)).max_w(px(380.)).child(query))
        .children(counter)
        .child(
            ui::icon_button("claude-search-above", IconName::ArrowUp, ui)
                .when(!has_matches, |button| button.opacity(0.5))
                .tooltip(tip("Match Above", &find_bar::SelectNextMatch, &focus))
                .on_click(cx.listener(|chat, _: &ClickEvent, _, cx| chat.step_search(true, cx))),
        )
        .child(
            ui::icon_button("claude-search-below", IconName::ArrowDown, ui)
                .when(!has_matches, |button| button.opacity(0.5))
                .tooltip(tip("Match Below", &find_bar::SelectPreviousMatch, &focus))
                .on_click(cx.listener(|chat, _: &ClickEvent, _, cx| chat.step_search(false, cx))),
        )
        .child(div().flex_1())
        .child(
            ui::icon_button("claude-search-close", IconName::Close, ui)
                .tooltip(tip("Close", &find_bar::Dismiss, &focus))
                .on_click(
                    cx.listener(|chat, _: &ClickEvent, window, cx| chat.close_search(window, cx)),
                ),
        );
    let bar =
        claude_transcript::column(
            div()
                .key_context("FindBar")
                .py(px(8.))
                .font_family(theme::UI_FONT)
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.foreground)
                .on_action(cx.listener(|chat, _: &find_bar::SelectNextMatch, _, cx| {
                    chat.step_search(true, cx)
                }))
                .on_action(
                    cx.listener(|chat, _: &find_bar::SelectPreviousMatch, _, cx| {
                        chat.step_search(false, cx)
                    }),
                )
                .on_action(cx.listener(|chat, _: &find_bar::Dismiss, window, cx| {
                    chat.close_search(window, cx)
                }))
                // ⌥↵ and ⇥ have nothing to do in a one-field bar.
                .on_action(cx.listener(|_, _: &find_bar::SelectAllMatches, _, _| {}))
                .on_action(cx.listener(|_, _: &find_bar::FocusNextField, _, _| {}))
                .child(row),
        )
        .flex_none()
        .relative()
        .border_b_1()
        .border_color(ui.divider)
        // Its height, for the slide (the first slide starts from the frame that measured it).
        .child({
            let height = chat.search_slide.height.clone();
            canvas(
                move |bounds, window, _| {
                    let measured = f32::from(bounds.size.height);
                    if height.replace(measured) != measured {
                        window.request_animation_frame();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        });
    // Sliding down from the top of the chat: the room it takes grows, the bar comes down within
    // it, fading in over the first half; closing, it slides back up. At rest, it lays out as is.
    if t >= 1. {
        return Some(bar.into_any_element());
    }
    window.request_animation_frame();
    let height = chat.search_slide.height.get();
    let bar = div()
        .flex_none()
        .flex()
        .flex_col()
        .overflow_hidden()
        .h(px(height * shown))
        .child(
            bar.opacity((shown / 0.5).clamp(0., 1.))
                .transform_translate(px(0.), px(-height * (1. - shown))),
        );
    Some(if open {
        bar.into_any_element()
    } else {
        motion::inert(bar).into_any_element()
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use flux_claude::session::UserEntry;
    use flux_claude::{Entry, EntryKind, ShellEntry, ShellState};

    use super::{entry_texts, find};

    fn entry(id: u64, kind: EntryKind) -> Entry {
        Entry { id, kind }
    }

    fn user(text: &str) -> EntryKind {
        EntryKind::User(UserEntry {
            uuid: None,
            text: text.to_string(),
            images: Vec::new(),
            queued: false,
            cancelled: false,
            at: None,
        })
    }

    fn needle(query: &str) -> Vec<char> {
        query.to_lowercase().chars().collect()
    }

    #[test]
    fn matches_are_counted_by_entry_case_insensitively() {
        let entries = vec![
            entry(1, user("Why does the Config fail?")),
            entry(
                2,
                EntryKind::Text {
                    text:
                        "The **config** reads `CONFIG_PATH` first.\n\n```rust\nlet config = 1;\n```"
                            .to_string(),
                    streaming: false,
                },
            ),
            entry(3, user("thanks")),
        ];
        let found = find(&entries, Path::new("/p"), &[], &needle("config"));
        assert_eq!(found, vec![(1, 1), (2, 3)]);
        assert!(find(&entries, Path::new("/p"), &[], &needle("")).is_empty());
    }

    #[test]
    fn a_message_is_searched_without_its_edge_mentions() {
        let entries = vec![entry(1, user("@src/config.rs#L1-2 explain the config"))];
        assert_eq!(
            entry_texts(&entries[0], Path::new("/p"), &[]),
            vec!["explain the config".to_string()]
        );
        assert_eq!(
            find(&entries, Path::new("/p"), &[], &needle("config")),
            vec![(1, 1)]
        );
    }

    #[test]
    fn a_command_and_its_output_are_searched() {
        let shell = EntryKind::Shell(ShellEntry {
            command: "cargo test".to_string(),
            output: "test a ... ok\ntest b ... FAILED\n".to_string(),
            state: ShellState::Done,
            exit_code: Some(101),
            duration: None,
            started: None,
            delivered: true,
        });
        let entries = vec![entry(7, shell)];
        assert_eq!(
            find(&entries, Path::new("/p"), &[], &needle("test")),
            vec![(7, 3)]
        );
    }
}
