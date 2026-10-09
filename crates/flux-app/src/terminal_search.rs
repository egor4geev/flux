//! Search in a terminal's output (⌘F): a bar at the top of the pane, like the find bar in a file;
//! the terminal element draws the matches, the current one brighter.
//!
//! The window's find bar actions are reused: ⌘F (`find_bar::Deploy`) and ⌘G / ⇧⌘G handled by the
//! terminal come here instead of the file's find bar (⌘R as well: a terminal has nothing to
//! replace), and the bar has the `FindBar` key context, so ↵, ⇧↵, Esc, and the ⌥⌘C/W/R toggles
//! work as in a file.
//!
//! The newest output is at the bottom, so the search runs upward, as in other terminals: the first
//! match is the nearest one at or above the bottom of the view; ↵ and ⌘G go to the match above,
//! ⇧↵ and ⇧⌘G to the one below; past the oldest match the search wraps around to the newest. While
//! the bar is open, the output is searched again as it arrives, throttled to the cost of a search;
//! the current match keeps its number counted from the top of the scrollback, which new output at
//! the bottom doesn't change.

use std::time::{Duration, Instant};

use flux_term::{GridPoint, SearchMatch, SearchOptions};
use gpui::{
    Action, AnyElement, App, AppContext, ClickEvent, Context, Div, Entity, FocusHandle, Focusable,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};

use crate::find_bar;
use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::terminal_view::TerminalView;
use crate::theme::{self, Theme};
use crate::ui::{self, GAP};

/// Width limits of the query field: in a narrow split pane the buttons still fit.
const INPUT_MIN_WIDTH: f32 = 120.;
const INPUT_MAX_WIDTH: f32 = 380.;
/// A query error message is no wider than this.
const COUNTER_MAX_WIDTH: f32 = 320.;
/// A search after new output waits at least this long, and ten times as long as the last search
/// took, up to the maximum: a flood of output takes a bounded share of the UI thread.
const REFRESH_MIN: Duration = Duration::from_millis(150);
const REFRESH_MAX: Duration = Duration::from_millis(500);

pub fn init(_cx: &mut App) {}

/// The outcome of the last search, for the counter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Status {
    /// The query is empty.
    #[default]
    Idle,
    /// `truncated`: there are more matches than the terminal collects.
    Found { truncated: bool },
    /// The regular expression doesn't compile.
    Error(SharedString),
}

/// The search in one terminal: the bar while it is open, and what it found. The element draws
/// `matches` (in grid coordinates, kept fresh as output arrives) and the `active` one brighter.
#[derive(Default)]
pub struct SearchState {
    open: bool,
    /// The bar has just opened: the first search waits until the grid has made room for it.
    opening: bool,
    /// The query field, created when the bar first opens; it keeps the query between openings.
    query: Option<Entity<TextInput>>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    status: Status,
    /// The query and options of the last search: `Changed` with the same text doesn't search again.
    searched: Option<(String, SearchOptions)>,
    /// From the top of the scrollback to the bottom of the screen.
    pub(crate) matches: Vec<SearchMatch>,
    pub(crate) active: Option<usize>,
    /// The search after new output, waiting out its delay.
    refresh: Option<Task<()>>,
    /// How long the last search took: the delay of the next one after output follows it.
    last_duration: Duration,
    _query_subscription: Option<Subscription>,
}

impl SearchState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn options(&self) -> SearchOptions {
        SearchOptions {
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
        }
    }

    fn active_match(&self) -> Option<SearchMatch> {
        self.matches.get(self.active?).copied()
    }

    fn clear_results(&mut self) {
        self.status = Status::Idle;
        self.matches.clear();
        self.active = None;
    }
}

/// What to do with the current match after a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The bar opened, or the query or a toggle changed: the current match is the nearest one at or
    /// above the previous current match (or the bottom of the view), and it is scrolled into view.
    Find,
    /// New output: the current match keeps its place and nothing scrolls.
    Refresh,
}

/// ⌘F in the terminal (⌘R too): opens the bar with the selected text as the query, or, if focus
/// is already in the bar, selects the query.
pub fn deploy(view: &mut TerminalView, window: &mut Window, cx: &mut Context<TerminalView>) {
    let query = query_input(view, window, cx);
    let was_open = view.search.open;
    let focused_here = was_open && query.focus_handle(cx).is_focused(window);
    view.search.open = true;
    if !was_open {
        search_after_layout(view, window, cx);
    }
    let seed = if focused_here { None } else { seed(view) };
    let current = query.read(cx).text();
    if let Some(seed) = seed.filter(|seed| *seed != current) {
        // `Changed` starts the search as you type (once the bar has room, if it is opening).
        query.update(cx, |query, cx| query.set_text(&seed, cx));
    }
    query.update(cx, |query, cx| query.select_all(cx));
    window.focus(&query.focus_handle(cx));
    cx.notify();
}

/// New output while the bar is open: the matches are searched again a little later, since lines
/// move as the output scrolls. One search covers all the output that arrived meanwhile.
pub fn output_changed(view: &mut TerminalView, cx: &mut Context<TerminalView>) {
    if !view.search.open || view.search.opening || view.search.refresh.is_some() {
        return;
    }
    if view
        .search
        .searched
        .as_ref()
        .is_none_or(|(text, _)| text.is_empty())
    {
        return;
    }
    let delay = (view.search.last_duration * 10).clamp(REFRESH_MIN, REFRESH_MAX);
    view.search.refresh = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(delay).await;
        this.update(cx, |view, cx| {
            view.search.refresh = None;
            if view.search.open {
                search(view, Mode::Refresh, cx);
            }
        })
        .ok();
    }));
}

/// Search actions handled by the terminal (⌘F, ⌘R, ⌘G, ⇧⌘G).
pub fn actions(root: Div, cx: &mut Context<TerminalView>) -> Div {
    root.on_action(cx.listener(|view, _: &find_bar::Deploy, window, cx| deploy(view, window, cx)))
        .on_action(
            cx.listener(|view, _: &find_bar::DeployReplace, window, cx| deploy(view, window, cx)),
        )
        .on_action(
            cx.listener(|view, _: &find_bar::FindNext, window, cx| {
                find_next(view, true, window, cx)
            }),
        )
        .on_action(cx.listener(|view, _: &find_bar::FindPrevious, window, cx| {
            find_next(view, false, window, cx)
        }))
}

/// The query field, created on first use.
fn query_input(
    view: &mut TerminalView,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) -> Entity<TextInput> {
    if let Some(query) = &view.search.query {
        return query.clone();
    }
    let query = cx.new(|cx| TextInput::new(tr("Find"), cx).code().icon(IconName::Search));
    let subscription = cx.subscribe_in(&query, window, query_changed);
    view.search.query = Some(query.clone());
    view.search._query_subscription = Some(subscription);
    query
}

/// The query was edited: search as you type (unless it is the text just searched).
fn query_changed(
    view: &mut TerminalView,
    _: &Entity<TextInput>,
    event: &InputEvent,
    _: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    match event {
        InputEvent::Changed => {
            if !view.search.opening && view.search.searched != current_query(view, cx) {
                search(view, Mode::Find, cx);
            }
        }
    }
}

fn current_query(view: &TerminalView, cx: &App) -> Option<(String, SearchOptions)> {
    let query = view.search.query.as_ref()?;
    Some((query.read(cx).text(), view.search.options()))
}

/// The query taken from the selection, if it is on one line; in regex mode, escaped so that it is
/// searched literally.
fn seed(view: &TerminalView) -> Option<String> {
    let text = view.terminal.selection_text()?;
    let text = text.trim_end();
    if text.is_empty() || text.contains(['\n', '\r']) {
        return None;
    }
    Some(if view.search.regex {
        find_bar::escape_regex(text)
    } else {
        text.to_string()
    })
}

/// Searches the scrollback and the screen for the query.
fn search(view: &mut TerminalView, mode: Mode, cx: &mut Context<TerminalView>) {
    let Some((text, options)) = current_query(view, cx) else {
        return;
    };
    view.search.refresh = None;
    view.search.searched = Some((text.clone(), options));
    if text.is_empty() {
        view.search.clear_results();
        return cx.notify();
    }
    let bottom = viewport_bottom(view);
    let origin = view
        .search
        .active_match()
        .map_or(bottom, |found| found.start);
    let previous = view.search.active;

    let started = Instant::now();
    let results = view.terminal.search(&text, options);
    view.search.last_duration = started.elapsed();
    match results {
        // The regex engine's message ("error building NFA") says little; as in Find in Files.
        Err(_) => {
            view.search.clear_results();
            view.search.status = Status::Error(tr("Invalid regular expression").into());
        }
        Ok(results) => {
            view.search.status = Status::Found {
                truncated: results.truncated,
            };
            let matches = results.matches;
            view.search.active = match mode {
                Mode::Find => match_at_or_above(&matches, origin),
                Mode::Refresh => previous
                    .filter(|_| !matches.is_empty())
                    .map(|index| index.min(matches.len() - 1))
                    .or_else(|| match_at_or_above(&matches, bottom)),
            };
            view.search.matches = matches;
            if mode == Mode::Find {
                reveal_active(view);
            }
        }
    }
    cx.notify();
}

/// ⌘G / ⇧⌘G: the match above (`up`) or below. With the bar closed, it opens without taking
/// focus, the query taken from the selection if there is none yet.
fn find_next(
    view: &mut TerminalView,
    up: bool,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    if view.search.open {
        return step(view, up, cx);
    }
    let query = query_input(view, window, cx);
    if query.read(cx).is_empty() {
        let Some(seed) = seed(view) else {
            return;
        };
        query.update(cx, |query, cx| query.set_text(&seed, cx));
    }
    view.search.open = true;
    search_after_layout(view, window, cx);
    cx.notify();
}

/// The match above (`up`) or below the current one, wrapping around; it is scrolled into view.
fn step(view: &mut TerminalView, up: bool, cx: &mut Context<TerminalView>) {
    let len = view.search.matches.len();
    let index = match view.search.active {
        Some(active) => step_index(active, len, up),
        None => match_at_or_above(&view.search.matches, viewport_bottom(view)),
    };
    if index.is_none() {
        return;
    }
    view.search.active = index;
    reveal_active(view);
    cx.notify();
}

/// Scrolls the scrollback so that the current match is visible: its end first, then its start, so
/// that a match taller than the view shows its beginning.
fn reveal_active(view: &mut TerminalView) {
    if let Some(found) = view.search.active_match() {
        view.terminal.scroll_to(found.end);
        view.terminal.scroll_to(found.start);
    }
}

/// The bar has just opened: it takes rows from the grid when it is first drawn, and a view scrolled
/// back then keeps its top line and loses rows at the bottom, while the lines of the screen may move
/// into the scrollback. So the first search waits until the grid has its new size: the first match
/// is the nearest one at or above the bottom of what is actually visible, and the matches are where
/// the element draws them. Next-frame callbacks run before the frame is drawn, hence two of them.
fn search_after_layout(
    view: &mut TerminalView,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    view.search.opening = true;
    cx.on_next_frame(window, |_, window, cx| {
        cx.on_next_frame(window, |view, _, cx| {
            view.search.opening = false;
            if view.search.open {
                search(view, Mode::Find, cx);
            }
        });
    });
}

/// The last cell of the view as the user saw it (the last frame): the search starts from there,
/// upward.
fn viewport_bottom(view: &TerminalView) -> GridPoint {
    match view.layout {
        Some(layout) => GridPoint::new(
            layout.rows as i32 - 1 - layout.display_offset as i32,
            layout.columns.saturating_sub(1),
        ),
        None => GridPoint::new(i32::MAX, usize::MAX),
    }
}

/// Esc, ×: the bar closes, the highlighting goes away, focus returns to the terminal. The query
/// stays for the next ⌘F.
fn close(view: &mut TerminalView, window: &mut Window, cx: &mut Context<TerminalView>) {
    let search = &mut view.search;
    search.open = false;
    search.opening = false;
    search.clear_results();
    search.refresh = None;
    search.searched = None;
    window.focus(&view.focus_handle);
    cx.notify();
}

fn toggle(
    view: &mut TerminalView,
    option: fn(&mut SearchState) -> &mut bool,
    cx: &mut Context<TerminalView>,
) {
    let flag = option(&mut view.search);
    *flag = !*flag;
    search(view, Mode::Find, cx);
}

/// The right part of the bar, after the toggles.
enum Counter {
    None,
    /// «3 of 12».
    Found(SharedString),
    /// "No results" or a regular expression error.
    Error(SharedString),
}

fn counter(search: &SearchState) -> Counter {
    match &search.status {
        Status::Idle => Counter::None,
        Status::Error(message) => Counter::Error(message.clone()),
        Status::Found { truncated } => {
            let count = search.matches.len();
            let label = find_bar::counter_label(count, search.active, *truncated).into();
            if count == 0 {
                Counter::Error(label)
            } else {
                Counter::Found(label)
            }
        }
    }
}

/// The bar above the terminal text, while it is open.
pub fn render_bar(
    view: &mut TerminalView,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) -> Option<AnyElement> {
    if !view.search.is_open() {
        return None;
    }
    let query = view.search.query.clone()?;
    let ui = Theme::ui(cx);
    // Tooltips show the keys as if focus were in the field: they are right in the terminal, too.
    let focus = query.focus_handle(cx);
    let tip = |label: &'static str, action: &dyn Action, focus: &FocusHandle| {
        ui::tooltip(label, ui::shortcut_in(action, focus, window))
    };
    let counter = match counter(&view.search) {
        Counter::None => None,
        Counter::Found(label) => Some(ui::badge(label, ui.accent_text).into_any_element()),
        Counter::Error(message) => Some(
            div()
                .flex_none()
                .max_w(px(COUNTER_MAX_WIDTH))
                .flex()
                .items_center()
                .gap_1()
                .text_color(ui.error)
                .child(icon(IconName::Warning, ui.error).size(px(13.)))
                .child(div().min_w_0().truncate().child(message))
                .into_any_element(),
        ),
    };
    let search = &view.search;
    let row = div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .flex_1()
                .min_w(px(INPUT_MIN_WIDTH))
                .max_w(px(INPUT_MAX_WIDTH))
                .child(query),
        )
        .child(
            ui::toggle_button(
                "terminal-search-case",
                IconName::CaseSensitive,
                search.case_sensitive,
                ui,
            )
            .tooltip(tip(
                tr("Match Case"),
                &find_bar::ToggleCaseSensitive,
                &focus,
            ))
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                toggle(view, |search| &mut search.case_sensitive, cx)
            })),
        )
        .child(
            ui::toggle_button(
                "terminal-search-word",
                IconName::WholeWord,
                search.whole_word,
                ui,
            )
            .tooltip(tip(tr("Whole Word"), &find_bar::ToggleWholeWord, &focus))
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                toggle(view, |search| &mut search.whole_word, cx)
            })),
        )
        .child(
            ui::toggle_button("terminal-search-regex", IconName::Regex, search.regex, ui)
                .tooltip(tip(
                    tr("Regular Expression"),
                    &find_bar::ToggleRegex,
                    &focus,
                ))
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    toggle(view, |search| &mut search.regex, cx)
                })),
        )
        .child(div().flex_none().w(px(2.)))
        .children(counter)
        .child(
            ui::icon_button("terminal-search-above", IconName::ArrowUp, ui)
                .tooltip(tip(tr("Match Above"), &find_bar::SelectNextMatch, &focus))
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| step(view, true, cx))),
        )
        .child(
            ui::icon_button("terminal-search-below", IconName::ArrowDown, ui)
                .tooltip(tip(
                    tr("Match Below"),
                    &find_bar::SelectPreviousMatch,
                    &focus,
                ))
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| step(view, false, cx))),
        )
        .child(div().flex_1())
        .child(
            ui::icon_button("terminal-search-close", IconName::Close, ui)
                .tooltip(tip(tr("Close"), &find_bar::Dismiss, &focus))
                .on_click(cx.listener(|view, _: &ClickEvent, window, cx| close(view, window, cx))),
        );

    Some(
        div()
            .key_context("FindBar")
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .px_2()
            .py_2()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.foreground)
            // ↵ goes up, to older output: see the module notes.
            .on_action(
                cx.listener(|view, _: &find_bar::SelectNextMatch, _, cx| step(view, true, cx)),
            )
            .on_action(
                cx.listener(|view, _: &find_bar::SelectPreviousMatch, _, cx| step(view, false, cx)),
            )
            .on_action(
                cx.listener(|view, _: &find_bar::Dismiss, window, cx| close(view, window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &find_bar::ToggleCaseSensitive, _, cx| {
                    toggle(view, |search| &mut search.case_sensitive, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &find_bar::ToggleWholeWord, _, cx| {
                toggle(view, |search| &mut search.whole_word, cx)
            }))
            .on_action(cx.listener(|view, _: &find_bar::ToggleRegex, _, cx| {
                toggle(view, |search| &mut search.regex, cx)
            }))
            // ⌥↵ (select all matches) and ⇥ (next field) have nothing to do in a one-field bar on
            // a terminal; caught here, they don't reach the program.
            .on_action(cx.listener(|_, _: &find_bar::SelectAllMatches, _, _| {}))
            .on_action(cx.listener(|_, _: &find_bar::FocusNextField, _, _| {}))
            .child(row)
            // The divider under the bar runs from edge to edge of the pane, inset like the file's.
            .child(
                div()
                    .absolute()
                    .left(px(GAP))
                    .right(px(GAP))
                    .bottom_0()
                    .h(px(1.))
                    .bg(ui.divider),
            )
            .into_any_element(),
    )
}

// --- Choosing a match: matches are in reading order, from the top of the scrollback ---

/// The last match that starts at `origin` or before it; with none there, the last match of all —
/// the upward search wraps around from the oldest output to the newest.
fn match_at_or_above(matches: &[SearchMatch], origin: GridPoint) -> Option<usize> {
    let last = matches.len().checked_sub(1)?;
    let after = matches.partition_point(|found| found.start <= origin);
    Some(after.checked_sub(1).unwrap_or(last))
}

/// The match above (`up`: the previous one in reading order) or below the current one, wrapping
/// around.
fn step_index(active: usize, len: usize, up: bool) -> Option<usize> {
    if len == 0 {
        None
    } else if up {
        Some((active.min(len - 1) + len - 1) % len)
    } else {
        Some((active + 1) % len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(line: i32, column: usize, len: usize) -> SearchMatch {
        SearchMatch {
            start: GridPoint::new(line, column),
            end: GridPoint::new(line, column + len - 1),
        }
    }

    /// Matches in the scrollback (-5), on the screen (0, 3), the newest at line 10.
    fn matches() -> Vec<SearchMatch> {
        vec![
            found(-5, 2, 3),
            found(0, 0, 3),
            found(3, 7, 3),
            found(10, 1, 3),
        ]
    }

    #[test]
    fn the_first_match_is_the_nearest_at_or_above_the_origin() {
        let matches = matches();
        // From the bottom of a view that shows lines 0..=12: the newest match.
        assert_eq!(match_at_or_above(&matches, GridPoint::new(12, 79)), Some(3));
        // A view scrolled back to the scrollback: the match above its bottom.
        assert_eq!(match_at_or_above(&matches, GridPoint::new(2, 79)), Some(1));
        // A match that starts at the origin is chosen itself (the query is being refined).
        assert_eq!(match_at_or_above(&matches, GridPoint::new(3, 7)), Some(2));
        // Nothing above the origin: wraps around to the newest.
        assert_eq!(match_at_or_above(&matches, GridPoint::new(-6, 0)), Some(3));
        assert_eq!(match_at_or_above(&[], GridPoint::new(0, 0)), None);
    }

    #[test]
    fn steps_go_up_and_down_and_wrap() {
        assert_eq!(step_index(2, 4, true), Some(1));
        assert_eq!(step_index(2, 4, false), Some(3));
        // Above the oldest: the newest; below the newest: the oldest.
        assert_eq!(step_index(0, 4, true), Some(3));
        assert_eq!(step_index(3, 4, false), Some(0));
        // A stale current match past the end (the output shrank) steps from the last one.
        assert_eq!(step_index(9, 4, true), Some(2));
        assert_eq!(step_index(0, 0, true), None);
    }
}
