//! Контекстное меню (правая кнопка мыши): список пунктов поверх окна у курсора.
//!
//! [`ContextMenu`] — вид со своим фокусом: ↑/↓, ↵, ⎋ работают сразу. Владелец рисует его
//! поверх всего у точки щелчка ([`ContextMenu::overlay`]) и закрывает по [`DismissEvent`]:
//! выбор пункта, Esc, щелчок мимо. Пункт — действие gpui: перед отправкой фокус
//! возвращается туда, где был до меню (`dispatch_action` берёт фокус в момент вызова, как
//! в палитре команд), поэтому действие получает тот, кто открыл меню. Сочетания справа —
//! из keymap того же места.

use gpui::{
    Action, AnyElement, App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, KeyBinding, Pixels, Point, Render, SharedString, Window, actions, anchored,
    deferred, div, prelude::*, px,
};

use crate::command_palette::keystroke_label;
use crate::theme::{self, Theme};

const MENU_MIN_WIDTH: f32 = 220.;
const ITEM_HEIGHT: f32 = 24.;
const TEXT_SIZE: f32 = 13.;

actions!(context_menu, [SelectNext, SelectPrevious, Confirm, Cancel]);

pub fn init(cx: &mut App) {
    let context = Some("ContextMenu");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Cancel, context),
    ]);
}

enum Item {
    Entry {
        label: SharedString,
        action: Box<dyn Action>,
        enabled: bool,
    },
    Separator,
}

pub struct ContextMenu {
    items: Vec<Item>,
    /// Пункт, выбранный стрелками; мышь подсвечивает свой сама.
    selected: Option<usize>,
    focus_handle: FocusHandle,
    /// Кто был в фокусе до меню: ему уходит действие, по нему ищутся сочетания.
    previous_focus: Option<FocusHandle>,
}

impl EventEmitter<DismissEvent> for ContextMenu {}

impl ContextMenu {
    /// Пустое меню для того, кто сейчас в фокусе. Пункты — [`Self::entry`], [`Self::separator`].
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            items: Vec::new(),
            selected: None,
            focus_handle: cx.focus_handle(),
            previous_focus: window.focused(cx),
        }
    }

    pub fn entry(self, label: impl Into<SharedString>, action: impl Action) -> Self {
        self.entry_if(true, label, action)
    }

    /// Пункт, который виден, но недоступен при `!enabled` (например, «Paste» без скопированного).
    pub fn entry_if(
        mut self,
        enabled: bool,
        label: impl Into<SharedString>,
        action: impl Action,
    ) -> Self {
        self.items.push(Item::Entry {
            label: label.into(),
            action: Box::new(action),
            enabled,
        });
        self
    }

    /// Разделитель; в начале, в конце и подряд — не рисуется.
    pub fn separator(mut self) -> Self {
        if !self.items.is_empty() && !matches!(self.items.last(), Some(Item::Separator)) {
            self.items.push(Item::Separator);
        }
        self
    }

    /// Меню поверх окна в точке `position` (координаты окна); у края окна — сдвигается внутрь.
    pub fn overlay(menu: &Entity<Self>, position: Point<Pixels>) -> AnyElement {
        deferred(
            anchored()
                .position(position)
                .snap_to_window()
                .child(menu.clone()),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn enabled_items(&self) -> impl Iterator<Item = usize> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, Item::Entry { enabled: true, .. }))
            .map(|(index, _)| index)
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let next = self
            .enabled_items()
            .find(|&index| self.selected.is_none_or(|selected| index > selected))
            .or_else(|| self.enabled_items().next());
        self.selected = next;
        cx.notify();
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let enabled: Vec<usize> = self.enabled_items().collect();
        let previous = enabled
            .iter()
            .rev()
            .find(|&&index| self.selected.is_none_or(|selected| index < selected))
            .or(enabled.last())
            .copied();
        self.selected = previous;
        cx.notify();
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.selected {
            self.run(index, window, cx);
        }
    }

    /// Выполняет пункт: фокус — назад, действие — туда же, меню закрывается.
    fn run(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Item::Entry {
            action,
            enabled: true,
            ..
        }) = self.items.get(index)
        else {
            return;
        };
        let action = action.boxed_clone();
        if let Some(focus) = &self.previous_focus {
            window.focus(focus);
        }
        cx.emit(DismissEvent);
        window.dispatch_action(action, cx);
    }

    /// Сочетание действия значками macOS — в контексте клавиш того, кто открыл меню.
    fn keys_for(&self, action: &dyn Action, window: &Window) -> Option<SharedString> {
        let bindings = match &self.previous_focus {
            Some(focus) => window.bindings_for_action_in(action, focus),
            None => window.bindings_for_action(action),
        };
        let binding = bindings.first()?;
        let keys: Vec<String> = binding
            .keystrokes()
            .iter()
            .map(|keystroke| keystroke_label(keystroke.modifiers(), keystroke.key()))
            .collect();
        Some(keys.join(" ").into())
    }
}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let last = self.items.len().saturating_sub(1);
        let items: Vec<AnyElement> = self
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| !(matches!(item, Item::Separator) && *index == last))
            .map(|(index, item)| match item {
                Item::Separator => div().my_1().h(px(1.)).bg(ui.border).into_any_element(),
                Item::Entry {
                    label,
                    action,
                    enabled,
                } => {
                    let keys = self.keys_for(action.as_ref(), window);
                    let selected = self.selected == Some(index);
                    div()
                        .id(index)
                        .mx_1()
                        .px_2()
                        .h(px(ITEM_HEIGHT))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_6()
                        .rounded_sm()
                        .whitespace_nowrap()
                        .text_color(if *enabled { ui.foreground } else { ui.dim })
                        .when(*enabled && selected, |item| item.bg(ui.list_selected))
                        .when(*enabled && !selected, |item| {
                            item.hover(|style| style.bg(ui.list_hover))
                        })
                        .when(*enabled, |item| {
                            item.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.run(index, window, cx)
                            }))
                        })
                        .child(label.clone())
                        .children(keys.map(|keys| div().flex_none().text_color(ui.dim).child(keys)))
                        .into_any_element()
                }
            })
            .collect();
        div()
            .key_context("ContextMenu")
            .track_focus(&self.focus_handle)
            .min_w(px(MENU_MIN_WIDTH))
            .py_1()
            .flex()
            .flex_col()
            .bg(ui.panel)
            .border_1()
            .border_color(ui.border)
            .rounded_md()
            .shadow_lg()
            .font_family(theme::FONT_FAMILY)
            .text_size(px(TEXT_SIZE))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .children(items)
    }
}
