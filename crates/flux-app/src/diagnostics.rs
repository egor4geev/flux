//! Diagnostics of a document: kept in the editor and shifted by its edits until the server sends
//! fresh ones; drawn as wavy underlines and colored line numbers; F2 / Shift+F2 go to the next /
//! previous one (errors first, as in JetBrains). A document may have several servers (Python:
//! pyright and ruff): each publication replaces only its own server's diagnostics.
//!
//! Plugins publish problems too (part 8.2, a linter): they live in the same list under an owner of
//! their own ([`plugin_owner`]: the high bit, never a server's id), so they are drawn, counted and
//! visited by F2 as the servers' are, and a server's publication doesn't touch them.

use std::cmp::Reverse;
use std::ops::Range;

use flux_core::text::{line_len, line_start};
use flux_core::{Assoc, ChangeSet, Rope};
use flux_lsp::lsp_types::{self, DiagnosticSeverity, NumberOrString};
use gpui::prelude::FluentBuilder;
use std::path::PathBuf;

use gpui::{
    App, Context, Div, Entity, Hsla, InteractiveElement, IntoElement, KeyBinding, ParentElement,
    Pixels, Point, StatefulInteractiveElement, Styled, UnderlineStyle, Window, actions, div, point,
    px,
};

use crate::editor::Editor;
use crate::element::LayoutCache;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::theme::UiColors;
use crate::ui;
use crate::workspace::Workspace;

actions!(diagnostics, [NextProblem, PreviousProblem]);

/// The bit of the owners of plugins' problems: servers' ids count up from 0 and never have it.
const PLUGIN_OWNER: u64 = 1 << 63;

/// The owner of a plugin's problems in the documents: stable for the plugin's id (FNV-1a), with
/// the high bit set.
pub fn plugin_owner(plugin: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in plugin.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    PLUGIN_OWNER | (hash & !PLUGIN_OWNER)
}

/// Whether the problems of `owner` are a plugin's (not a server's).
pub fn is_plugin_owner(owner: u64) -> bool {
    owner & PLUGIN_OWNER != 0
}

/// Wavy underline: the line thickness; the wave is three times as high.
const UNDERLINE_THICKNESS: f32 = 1.;

pub fn init(cx: &mut App) {
    let context = Some("Editor");
    cx.bind_keys([
        // As in JetBrains: Next / Previous Highlighted Error.
        KeyBinding::new("f2", NextProblem, context),
        KeyBinding::new("shift-f2", PreviousProblem, context),
    ]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => tr("Error"),
            Severity::Warning => tr("Warning"),
            Severity::Info => tr("Info"),
            Severity::Hint => tr("Hint"),
        }
    }

    /// The color of the underline and the line number; hints are not drawn.
    pub fn color(self, ui: &UiColors) -> Option<Hsla> {
        match self {
            Severity::Error => Some(ui.error),
            Severity::Warning => Some(ui.warning),
            Severity::Info => Some(ui.info),
            Severity::Hint => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// Character range in the document; may be empty (drawn as one character wide).
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    /// "rustc", "clippy", …
    pub source: Option<String>,
    pub code: Option<String>,
    /// The server that published it (the hub's server id).
    pub owner: u64,
}

impl Diagnostic {
    /// One line for the status bar: "Error: mismatched types (rustc E0308)".
    pub fn summary(&self) -> String {
        let message = self.message.lines().next().unwrap_or_default();
        let origin = [self.source.as_deref(), self.code.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        if origin.is_empty() {
            format!("{}: {message}", self.severity.label())
        } else {
            format!("{}: {message} ({origin})", self.severity.label())
        }
    }
}

/// The document's diagnostics, sorted by start.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Fresh diagnostics from server `owner`: they replace that server's, the others stay.
    pub fn set(&mut self, owner: u64, items: Vec<Diagnostic>) {
        self.items.retain(|d| d.owner != owner);
        self.items
            .extend(items.into_iter().map(|d| Diagnostic { owner, ..d }));
        self.items.sort_by_key(|d| (d.range.start, d.severity));
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The servers' diagnostics go (the document is opened on its servers again: they publish
    /// theirs anew); the plugins' stay.
    pub fn clear_servers(&mut self) {
        self.items.retain(|d| is_plugin_owner(d.owner));
    }

    /// Server `owner` is gone (exited, restarted): its diagnostics are stale.
    pub fn clear_owner(&mut self, owner: u64) {
        self.items.retain(|d| d.owner != owner);
    }

    /// Shifts the diagnostics by an edit. Text typed at the edges of a range stays outside it; a
    /// range whose text is deleted becomes empty at that place (until the server publishes again).
    pub fn map(&mut self, changes: &ChangeSet) {
        if self.items.is_empty() {
            return;
        }
        #[derive(Clone, Copy)]
        enum Edge {
            Start,
            End,
            /// Both ends of an empty range.
            Point,
        }
        // `map_sorted` wants positions in order, and an end (`Before`) before a start (`After`) at
        // the same place.
        let mut edges = Vec::with_capacity(self.items.len() * 2);
        for (i, d) in self.items.iter().enumerate() {
            if d.range.is_empty() {
                edges.push((d.range.start, Assoc::After, i, Edge::Point));
            } else {
                edges.push((d.range.start, Assoc::After, i, Edge::Start));
                edges.push((d.range.end, Assoc::Before, i, Edge::End));
            }
        }
        edges.sort_by_key(|&(pos, assoc, ..)| (pos, assoc == Assoc::After));
        let mapped = changes.map_sorted(edges.iter().map(|&(pos, assoc, ..)| (pos, assoc)));
        for (&(_, _, i, edge), pos) in edges.iter().zip(mapped) {
            let range = &mut self.items[i].range;
            match edge {
                Edge::Start => range.start = pos,
                Edge::End => range.end = pos,
                Edge::Point => *range = pos..pos,
            }
        }
        for d in &mut self.items {
            d.range.end = d.range.end.max(d.range.start);
        }
    }

    /// Diagnostics whose range contains `pos` (an empty range — when it starts at `pos`), most
    /// severe first: for hover.
    pub fn at(&self, pos: usize) -> Vec<&Diagnostic> {
        let mut found: Vec<&Diagnostic> = self
            .items
            .iter()
            .filter(|d| d.range.contains(&pos) || d.range.start == pos)
            .collect();
        found.sort_by_key(|d| d.severity);
        found
    }

    /// Every diagnostic, by start (Flux's tools for Claude, the context of ⌥↵).
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// Diagnostics that touch `range` (an empty one — when it lies in it), in order: Fix with
    /// Claude takes those of the selection.
    pub fn in_range(&self, range: Range<usize>) -> Vec<&Diagnostic> {
        self.items
            .iter()
            .filter(|d| {
                d.range.start < range.end && d.range.end > range.start
                    || d.range.is_empty() && range.contains(&d.range.start)
            })
            .collect()
    }

    /// Errors and warnings, for the status bar.
    pub fn counts(&self) -> (usize, usize) {
        let count = |severity| self.items.iter().filter(|d| d.severity == severity).count();
        (count(Severity::Error), count(Severity::Warning))
    }

    /// Where F2 (`forward`) or Shift+F2 goes from `from`: among the errors if there are any,
    /// otherwise among warnings and infos (hints are skipped); wraps around.
    pub fn next_stop(&self, from: usize, forward: bool) -> Option<&Diagnostic> {
        let errors_only = self.items.iter().any(|d| d.severity == Severity::Error);
        let stops = || {
            self.items.iter().filter(move |d| {
                if errors_only {
                    d.severity == Severity::Error
                } else {
                    d.severity <= Severity::Info
                }
            })
        };
        if forward {
            stops()
                .find(|d| d.range.start > from)
                .or_else(|| stops().next())
        } else {
            // Of several diagnostics at one place, the first (most severe) one.
            let before = stops().rev().find(|d| d.range.start < from);
            let stop = before.or_else(|| stops().next_back())?;
            stops().find(|d| d.range.start == stop.range.start)
        }
    }
}

/// A file Flux knows problems of: an open document (its servers' and plugins' problems, in its
/// text, unsaved changes included), or one no editor shows, with what its servers published as
/// they sent it.
pub(crate) enum KnownFile {
    Open(Entity<Editor>),
    Published(Vec<lsp_types::Diagnostic>),
}

/// The files Flux knows problems of: every open document of the window (with problems or not —
/// the caller looks), then the files only the servers checked (several servers' publications of a
/// file together), in that order. Shared by Flux's tools for Claude and the plugin API.
pub(crate) fn known_files(workspace: &Workspace, cx: &App) -> Vec<(PathBuf, KnownFile)> {
    let mut files: Vec<(PathBuf, KnownFile)> = Vec::new();
    let mut open = Vec::new();
    for editor in workspace.editors(cx) {
        let Some(path) = editor.read(cx).document.path().map(PathBuf::from) else {
            continue;
        };
        open.push(crate::navigation::canonical(&path));
        files.push((path, KnownFile::Open(editor)));
    }
    for (path, _, diagnostics) in workspace.lsp.read(cx).unopened_diagnostics() {
        if open.contains(&crate::navigation::canonical(&path)) {
            continue;
        }
        let known = files.iter_mut().find_map(|(known, file)| match file {
            KnownFile::Published(list) if *known == path => Some(list),
            _ => None,
        });
        match known {
            Some(list) => list.extend(diagnostics),
            None => files.push((path, KnownFile::Published(diagnostics))),
        }
    }
    files
}

/// Server diagnostics → ours, positions in `text` (the document the server reported on).
pub fn from_lsp(text: &Rope, diagnostics: &[lsp_types::Diagnostic]) -> Vec<Diagnostic> {
    let lines = flux_lsp::position::Lines::new(text);
    diagnostics
        .iter()
        .map(|d| Diagnostic {
            range: lines.range_from_lsp(d.range),
            severity: severity_from_lsp(d),
            message: d.message.clone(),
            source: d.source.clone(),
            code: code_from_lsp(d),
            owner: 0,
        })
        .collect()
}

/// A server diagnostic's severity: without one it is up to the client; like VS Code, an error.
pub fn severity_from_lsp(diagnostic: &lsp_types::Diagnostic) -> Severity {
    match diagnostic.severity {
        Some(DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => Severity::Info,
        Some(DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    }
}

/// A server diagnostic's code as text: "E0308", "6133".
pub fn code_from_lsp(diagnostic: &lsp_types::Diagnostic) -> Option<String> {
    diagnostic.code.as_ref().map(|code| match code {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s.clone(),
    })
}

/// Diagnostics of the visible lines, ready to paint.
#[derive(Default)]
pub struct DiagnosticsPaint {
    /// Wavy underlines (origin, width, color), the most severe painted last (on top).
    underlines: Vec<(Point<Pixels>, Pixels, Hsla)>,
    first_line: usize,
    /// For each visible line where a problem starts, the color of the worst one: its line number is
    /// drawn in it (a mark beside the number would not fit the gutter of a long file).
    numbers: Vec<Option<Hsla>>,
}

impl DiagnosticsPaint {
    /// Under the content mask of the text area, after the text: the waves sit under the glyphs.
    pub fn paint_underlines(&mut self, window: &mut Window) {
        for (origin, width, color) in self.underlines.drain(..) {
            window.paint_underline(
                origin,
                width,
                &UnderlineStyle {
                    thickness: px(UNDERLINE_THICKNESS),
                    color: Some(color),
                    wavy: true,
                },
            );
        }
    }

    /// The color of a visible line's number, if a problem starts on the line.
    pub fn number_color(&self, line: usize) -> Option<Hsla> {
        *self.numbers.get(line.checked_sub(self.first_line)?)?
    }
}

/// Lays out the diagnostics over the visible lines `layout.first_line..last_line`; `em` is the width
/// of a character (an empty range is underlined one character wide past the end of a line).
pub fn prepaint(
    diagnostics: &Diagnostics,
    text: &Rope,
    layout: &LayoutCache,
    last_line: usize,
    em: Pixels,
    ui: &UiColors,
) -> DiagnosticsPaint {
    let mut paint = DiagnosticsPaint::default();
    let first_line = layout.first_line;
    if diagnostics.items.is_empty() || last_line <= first_line {
        return paint;
    }
    let line_height = layout.line_height;
    let len = text.len_chars();
    // Character bounds of the visible lines: most diagnostics are outside them, and are skipped
    // by comparing positions, without looking up lines.
    let visible_start = line_start(text, first_line);
    let visible_end = if last_line < text.len_lines() {
        line_start(text, last_line)
    } else {
        len + 1
    };
    let mut worst: Vec<Option<Severity>> = vec![None; last_line - first_line];
    let mut underlines = Vec::new();
    for d in &diagnostics.items {
        if d.range.start >= visible_end {
            break;
        }
        if d.range.end < visible_start {
            continue;
        }
        let start_line = text.char_to_line(d.range.start.min(len));
        let end_line = text.char_to_line(d.range.end.min(len));
        let Some(color) = d.severity.color(ui) else {
            continue;
        };
        if start_line >= first_line {
            let slot = &mut worst[start_line - first_line];
            *slot = Some(slot.map_or(d.severity, |s| s.min(d.severity)));
        }
        for line in start_line.max(first_line)..=end_line.min(last_line - 1) {
            let Some(line_layout) = layout.line(line) else {
                continue;
            };
            let start = line_start(text, line);
            let line_chars = line_len(text, line);
            let x0 = if line == start_line {
                line_layout.x_for_column(d.range.start - start)
            } else {
                px(0.)
            };
            let mut x1 = if line == end_line {
                line_layout.x_for_column(d.range.end - start)
            } else {
                line_layout.shaped.width
            };
            // An empty range, or one that covers only a line break: one character wide.
            if line == start_line && x1 - x0 < px(2.) {
                let column = d.range.start - start;
                x1 = if column < line_chars {
                    line_layout.x_for_column(column + 1)
                } else {
                    x0 + em
                };
            }
            if x1 <= x0 {
                continue;
            }
            let wave = px(UNDERLINE_THICKNESS * 3.);
            let top = layout.line_top(line) + line_height - wave - px(1.);
            underlines.push((d.severity, point(layout.origin.x + x0, top), x1 - x0, color));
        }
    }
    underlines.sort_by_key(|&(severity, ..)| Reverse(severity));
    paint.underlines = underlines
        .into_iter()
        .map(|(_, origin, width, color)| (origin, width, color))
        .collect();
    paint.first_line = first_line;
    paint.numbers = worst
        .into_iter()
        .map(|severity| severity?.color(ui))
        .collect();
    paint
}

/// F2 / Shift+F2: the cursor goes to the start of the next / previous problem; its popup shows at
/// the caret (as JetBrains' error tooltip: Fix with Claude, More actions… ⌥↵) and its message goes
/// to the status bar.
fn go_to_problem(editor: &mut Editor, forward: bool, cx: &mut Context<Editor>) {
    let head = editor.document.selection().primary().head;
    let Some(stop) = editor.diagnostics.next_stop(head, forward) else {
        editor.show_status(tr("No problems").into(), cx);
        return;
    };
    let (start, summary) = (stop.range.start, stop.summary());
    editor.select_range(start..start, cx);
    editor.show_status(summary.into(), cx);
    crate::hover::show_problem(editor, start, cx);
}

/// Registers the editor actions of this module.
pub fn actions(root: Div, cx: &mut Context<Editor>) -> Div {
    root.on_action(cx.listener(|editor, _: &NextProblem, _, cx| go_to_problem(editor, true, cx)))
        .on_action(
            cx.listener(|editor, _: &PreviousProblem, _, cx| go_to_problem(editor, false, cx)),
        )
}

/// Error and warning counts of the active document for the status bar; a click goes to the next
/// problem.
pub fn status_item(editor: &Editor, ui: UiColors) -> Option<impl IntoElement + use<>> {
    let (errors, warnings) = editor.diagnostics.counts();
    if errors + warnings == 0 {
        return None;
    }
    let count = |name: IconName, color: Hsla, count: usize| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(icon(name, color).size(px(13.)))
            .child(count.to_string())
    };
    let tooltip = trf("Errors: {0} · Warnings: {1}", &[&errors, &warnings]);
    Some(
        div()
            .id("diagnostics-status")
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_1p5()
            .h(px(20.))
            .rounded(px(ui::RADIUS_SM))
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .when(errors > 0, |item| {
                item.child(count(IconName::Error, ui.error, errors))
            })
            .when(warnings > 0, |item| {
                item.child(count(IconName::Warning, ui.warning, warnings))
            })
            .tooltip(ui::tooltip(tooltip, Some("F2".into())))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(NextProblem), cx)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(start: usize, end: usize, severity: Severity) -> Diagnostic {
        Diagnostic {
            range: start..end,
            severity,
            message: format!("{start}..{end}"),
            source: None,
            code: None,
            owner: 0,
        }
    }

    fn ranges(diagnostics: &Diagnostics) -> Vec<Range<usize>> {
        diagnostics.items.iter().map(|d| d.range.clone()).collect()
    }

    #[test]
    fn plugins_own_their_problems_apart_from_servers() {
        let owner = plugin_owner("someone.linter");
        assert_eq!(owner, plugin_owner("someone.linter"), "stable");
        assert_ne!(owner, plugin_owner("someone.other"));
        assert!(is_plugin_owner(owner));
        // Servers count from 0: a server's id is never a plugin's owner.
        assert!(!is_plugin_owner(0) && !is_plugin_owner(12_345));
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(3, vec![diagnostic(0, 4, Severity::Error)]);
        diagnostics.set(owner, vec![diagnostic(6, 8, Severity::Warning)]);
        // A server's fresh publication leaves the plugin's problems alone.
        diagnostics.set(3, vec![diagnostic(1, 2, Severity::Error)]);
        assert_eq!(ranges(&diagnostics), vec![1..2, 6..8]);
        // Opened on its servers again: theirs go, the plugin's stay.
        diagnostics.clear_servers();
        assert_eq!(ranges(&diagnostics), vec![6..8]);
        diagnostics.clear_owner(owner);
        assert!(ranges(&diagnostics).is_empty());
    }

    #[test]
    fn a_range_takes_the_diagnostics_that_touch_it() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(
            1,
            vec![
                diagnostic(0, 4, Severity::Error),
                diagnostic(10, 10, Severity::Warning),
                diagnostic(12, 20, Severity::Error),
                diagnostic(30, 35, Severity::Info),
            ],
        );
        let found = |range: Range<usize>| -> Vec<Range<usize>> {
            diagnostics
                .in_range(range)
                .into_iter()
                .map(|d| d.range.clone())
                .collect()
        };
        assert_eq!(found(3..12), vec![0..4, 10..10]);
        assert_eq!(found(19..31), vec![12..20, 30..35]);
        assert_eq!(found(21..29), Vec::<Range<usize>>::new());
    }

    fn change(len: usize, changes: Vec<(usize, usize, Option<&str>)>) -> ChangeSet {
        ChangeSet::from_changes(
            len,
            changes
                .into_iter()
                .map(|(from, to, text)| (from, to, text.map(str::to_string))),
        )
    }

    #[test]
    fn each_server_replaces_only_its_own_diagnostics() {
        let mut diagnostics = Diagnostics::default();
        // pyright (1) and ruff (2) report on the same document.
        diagnostics.set(1, vec![diagnostic(10, 12, Severity::Error)]);
        diagnostics.set(2, vec![diagnostic(3, 5, Severity::Warning)]);
        assert_eq!(ranges(&diagnostics), vec![3..5, 10..12]);
        assert_eq!(diagnostics.counts(), (1, 1));

        // A fresh publication from pyright keeps ruff's.
        diagnostics.set(1, vec![diagnostic(20, 21, Severity::Error)]);
        assert_eq!(ranges(&diagnostics), vec![3..5, 20..21]);
        assert_eq!(diagnostics.at(4)[0].owner, 2);

        // ruff restarted: only its diagnostics go.
        diagnostics.clear_owner(2);
        assert_eq!(ranges(&diagnostics), vec![20..21]);
    }

    #[test]
    fn diagnostics_follow_edits() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(
            0,
            vec![
                diagnostic(10, 13, Severity::Error),
                diagnostic(3, 6, Severity::Warning),
                diagnostic(6, 6, Severity::Info),
            ],
        );
        // Sorted by start.
        assert_eq!(ranges(&diagnostics), [3..6, 6..6, 10..13]);
        // Typing at the edges stays outside; an empty range moves with text typed at it.
        diagnostics.map(&change(20, vec![(3, 3, Some("<")), (6, 6, Some(">"))]));
        assert_eq!(ranges(&diagnostics), [4..7, 8..8, 12..15]);
        // Typing inside grows the range.
        diagnostics.map(&change(22, vec![(13, 13, Some("xy"))]));
        assert_eq!(ranges(&diagnostics), [4..7, 8..8, 12..17]);
        // Deleting the text of a range leaves an empty one in its place.
        diagnostics.map(&change(24, vec![(11, 18, None)]));
        assert_eq!(ranges(&diagnostics), [4..7, 8..8, 11..11]);
    }

    #[test]
    fn overlapping_ranges_map_independently() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(
            0,
            vec![
                diagnostic(0, 10, Severity::Warning),
                diagnostic(2, 4, Severity::Error),
            ],
        );
        diagnostics.map(&change(12, vec![(3, 3, Some("abc"))]));
        assert_eq!(ranges(&diagnostics), [0..13, 2..7]);
    }

    #[test]
    fn diagnostics_at_a_position_most_severe_first() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(
            0,
            vec![
                diagnostic(0, 10, Severity::Warning),
                diagnostic(2, 4, Severity::Error),
                diagnostic(6, 6, Severity::Hint),
            ],
        );
        let at =
            |pos| -> Vec<Severity> { diagnostics.at(pos).iter().map(|d| d.severity).collect() };
        assert_eq!(at(3), [Severity::Error, Severity::Warning]);
        assert_eq!(at(4), [Severity::Warning]);
        assert_eq!(at(6), [Severity::Warning, Severity::Hint]);
        assert!(at(10).is_empty());
        assert_eq!(diagnostics.counts(), (1, 1));
    }

    #[test]
    fn f2_visits_errors_first_and_wraps() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(
            0,
            vec![
                diagnostic(5, 8, Severity::Warning),
                diagnostic(10, 12, Severity::Error),
                diagnostic(20, 22, Severity::Error),
                diagnostic(30, 31, Severity::Hint),
            ],
        );
        let next = |from| diagnostics.next_stop(from, true).map(|d| d.range.start);
        let prev = |from| diagnostics.next_stop(from, false).map(|d| d.range.start);
        // Only errors while there are any.
        assert_eq!(next(0), Some(10));
        assert_eq!(next(10), Some(20));
        assert_eq!(next(20), Some(10));
        assert_eq!(prev(20), Some(10));
        assert_eq!(prev(10), Some(20));
        assert_eq!(prev(15), Some(10));

        // Without errors: warnings and infos, never hints.
        diagnostics.set(
            0,
            vec![
                diagnostic(5, 8, Severity::Warning),
                diagnostic(9, 9, Severity::Info),
                diagnostic(30, 31, Severity::Hint),
            ],
        );
        let next = |from| diagnostics.next_stop(from, true).map(|d| d.range.start);
        assert_eq!(next(0), Some(5));
        assert_eq!(next(5), Some(9));
        assert_eq!(next(9), Some(5));

        diagnostics.set(0, vec![diagnostic(30, 31, Severity::Hint)]);
        assert_eq!(diagnostics.next_stop(0, true), None);
    }

    #[test]
    fn previous_stop_at_a_shared_start_is_the_most_severe() {
        let mut diagnostics = Diagnostics::default();
        diagnostics.set(
            0,
            vec![
                diagnostic(4, 9, Severity::Info),
                diagnostic(4, 6, Severity::Warning),
            ],
        );
        let stop = diagnostics.next_stop(10, false).unwrap();
        assert_eq!(stop.severity, Severity::Warning);
    }

    #[test]
    fn summary_has_severity_first_line_and_origin() {
        let mut d = diagnostic(0, 1, Severity::Error);
        d.message = "mismatched types\nexpected `u32`".into();
        d.source = Some("rustc".into());
        d.code = Some("E0308".into());
        assert_eq!(d.summary(), "Error: mismatched types (rustc E0308)");
        d.source = None;
        d.code = None;
        d.severity = Severity::Warning;
        assert_eq!(d.summary(), "Warning: mismatched types");
    }

    #[test]
    fn server_diagnostics_convert_to_char_positions() {
        let text = Rope::from("fn main() {\n    let x: u32 = \"a\";\n}\n");
        let lsp = lsp_types::Diagnostic {
            range: lsp_types::Range::new(
                lsp_types::Position::new(1, 17),
                lsp_types::Position::new(1, 20),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("E0308".into())),
            source: Some("rustc".into()),
            message: "mismatched types".into(),
            ..Default::default()
        };
        let converted = from_lsp(&text, &[lsp]);
        assert_eq!(converted.len(), 1);
        let d = &converted[0];
        assert_eq!(text.slice(d.range.clone()).to_string(), "\"a\"");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code.as_deref(), Some("E0308"));
    }
}
