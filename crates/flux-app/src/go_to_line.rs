//! Go to line (cmd-l): "line" or "line:column", counting from one.
//!
//! A small popover with an input field. A line outside the document shows an error in the hint and
//! the window stays open; a column past the end of the line puts the cursor at the end of the line.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, Render,
    SharedString, Subscription, Window, actions, div, prelude::*, px,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, Theme};
use crate::ui;
use crate::workspace::Workspace;

actions!(go_to_line, [Toggle, Confirm, Dismiss]);

const WIDTH: f32 = 460.;
/// Header with the input field and footer with the hint, as in the pickers.
const HEADER_HEIGHT: f32 = 50.;
const FOOTER_HEIGHT: f32 = 36.;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-l", Toggle, Some("Workspace")),
        KeyBinding::new("enter", Confirm, Some("GoToLine")),
        KeyBinding::new("escape", Dismiss, Some("GoToLine")),
    ]);
}

pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace.active_editor() else {
        return;
    };
    workspace.toggle_modal(window, cx, move |window, cx| {
        GoToLine::new(editor, window, cx)
    });
}

pub struct GoToLine {
    editor: Entity<Editor>,
    input: Entity<TextInput>,
    /// Line count of the document and the cursor line (one-based), for the hint.
    line_count: usize,
    current_line: usize,
    /// The input could not be parsed, or the line is outside the document.
    error: Option<SharedString>,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for GoToLine {}

impl GoToLine {
    fn new(editor: Entity<Editor>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (line_count, current_line) = {
            let document = &editor.read(cx).document;
            let text = document.text();
            let head = document.selection().primary().head;
            (text.len_lines(), text.char_to_line(head) + 1)
        };
        let input = cx.new(|cx| {
            TextInput::new(tr("Line number or line:column"), cx)
                .borderless()
                .large()
        });
        let subscription = cx.subscribe_in(&input, window, |this, input, event, _, cx| {
            match event {
                InputEvent::Changed => {
                    // The error is shown as the user types, but an empty field is not an error.
                    let text = input.read(cx).text();
                    this.error = match text.trim() {
                        "" => None,
                        text => this.check(text).err(),
                    };
                    cx.notify();
                }
            }
        });
        Self {
            editor,
            input,
            line_count,
            current_line,
            error: None,
            _subscription: subscription,
        }
    }

    /// Zero-based line and column, or the error text for the hint.
    fn check(&self, text: &str) -> Result<(usize, usize), SharedString> {
        let Some(target) = parse_target(text) else {
            return Err(tr("Type a line number, or line:column").into());
        };
        if target.line > self.line_count {
            return Err(trf("Line must be between 1 and {0}", &[&self.line_count]).into());
        }
        Ok((
            target.line - 1,
            target.column.map_or(0, |column| column - 1),
        ))
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text();
        if text.trim().is_empty() {
            return cx.emit(DismissEvent);
        }
        match self.check(&text) {
            Ok((line, column)) => {
                self.editor.update(cx, |editor, cx| {
                    let position = editor.position(line, column);
                    editor.select_range(position..position, cx);
                });
                cx.emit(DismissEvent);
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}

impl Focusable for GoToLine {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for GoToLine {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let hint = match &self.error {
            Some(error) => div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_color(ui.error)
                .child(icon(IconName::Warning, ui.error).size(px(13.)))
                .child(error.clone()),
            None => div().child(trf(
                "Current line {0} of {1}",
                &[&self.current_line, &self.line_count],
            )),
        };
        ui::popover(ui)
            .key_context("GoToLine")
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(icon(IconName::Hash, ui.text_muted))
                    .child(div().flex_1().min_w_0().child(self.input.clone())),
            )
            .child(ui::divider(ui))
            .child(
                div()
                    .flex_none()
                    .h(px(FOOTER_HEIGHT))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(div().min_w_0().truncate().child(hint))
                    .child(ui::hint_bar(&[("↵", tr("go")), ("esc", tr("close"))], ui)),
            )
    }
}

/// Where to jump: line and column, one-based, as a person sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    line: usize,
    column: Option<usize>,
}

/// "12", "12:5", "12,5", " 12 : 5 ", "12:" are accepted; "0", "abc", "12:0", "1:2:3" are rejected.
fn parse_target(text: &str) -> Option<Target> {
    let mut parts = text.trim().splitn(2, [':', ',']);
    let line = parse_positive(parts.next()?)?;
    let column = match parts.next().map(str::trim) {
        None | Some("") => None,
        Some(column) => Some(parse_positive(column)?),
    };
    Some(Target { line, column })
}

fn parse_positive(text: &str) -> Option<usize> {
    text.trim().parse().ok().filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(line: usize, column: Option<usize>) -> Option<Target> {
        Some(Target { line, column })
    }

    #[test]
    fn line_and_optional_column() {
        assert_eq!(parse_target("12"), target(12, None));
        assert_eq!(parse_target("12:5"), target(12, Some(5)));
        assert_eq!(parse_target("12,5"), target(12, Some(5)));
        assert_eq!(parse_target("  12 : 5 "), target(12, Some(5)));
        assert_eq!(parse_target("12:"), target(12, None));
    }

    #[test]
    fn garbage_and_zero_are_rejected() {
        for text in [
            "", "  ", "abc", "0", "-3", "12:0", "12:x", "1:2:3", ":5", "1.5",
        ] {
            assert_eq!(parse_target(text), None, "{text:?}");
        }
    }
}
