//! Переход к строке (cmd-l): «строка» или «строка:колонка», с единицы.
//!
//! Маленькое всплывающее окно с полем ввода. Строка вне документа — ошибка в подсказке,
//! окно не закрывается; колонка за краем строки — курсор в её конец.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, Render,
    SharedString, Subscription, Window, actions, div, prelude::*, px,
};

use crate::editor::Editor;
use crate::input::{InputEvent, TextInput};
use crate::theme::Theme;
use crate::workspace::Workspace;

actions!(go_to_line, [Toggle, Confirm, Dismiss]);

const WIDTH: f32 = 420.;
const HINT_TEXT_SIZE: f32 = 12.;

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
    /// Строк в документе и строка курсора — с единицы, для подсказки.
    line_count: usize,
    current_line: usize,
    /// Ввод не разобрался или строка вне документа.
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
        let input = cx.new(|cx| TextInput::new("Line number or line:column", cx));
        let subscription = cx.subscribe_in(&input, window, |this, input, event, _, cx| {
            match event {
                InputEvent::Changed => {
                    // Ошибку показываем по мере ввода, но пустое поле — не ошибка.
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

    /// Строка и колонка с нуля — или текст ошибки для подсказки.
    fn check(&self, text: &str) -> Result<(usize, usize), SharedString> {
        let Some(target) = parse_target(text) else {
            return Err("Type a line number, or line:column".into());
        };
        if target.line > self.line_count {
            return Err(format!("Line must be between 1 and {}", self.line_count).into());
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
        let (hint, color) = match &self.error {
            Some(error) => (error.clone(), ui.error),
            None => (
                format!("Current: line {} of {}", self.current_line, self.line_count).into(),
                ui.dim,
            ),
        };
        div()
            .key_context("GoToLine")
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(ui.panel)
            .border_1()
            .border_color(ui.border)
            .rounded_lg()
            .shadow_lg()
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(self.input.clone())
            .child(
                div()
                    .px_1()
                    .text_size(px(HINT_TEXT_SIZE))
                    .text_color(color)
                    .child(hint),
            )
    }
}

/// Куда перейти: строка и колонка — с единицы, как их видит человек.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    line: usize,
    column: Option<usize>,
}

/// «12», «12:5», «12,5», « 12 : 5 », «12:» — да; «0», «abc», «12:0», «1:2:3» — нет.
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
