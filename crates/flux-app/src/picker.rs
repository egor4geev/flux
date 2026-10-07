//! Список выбора с полем запроса — основа палитры команд и поиска файла.
//!
//! [`Picker`] владеет полем ([`TextInput`]), выбранной строкой, прокруткой и клавишами;
//! что искать и как рисовать строку — решает [`PickerDelegate`]. Показывается как
//! всплывающее окно: `Workspace::toggle_modal(window, cx, |window, cx| Picker::new(..))`.

use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, Hsla, KeyBinding, Render, ScrollStrategy, SharedString, StyledText,
    Subscription, UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::input::{InputEvent, TextInput};
use crate::theme::Theme;

/// Высота строки списка.
pub const ROW_HEIGHT: f32 = 26.;
/// Сколько строк видно без прокрутки.
const MAX_VISIBLE_ROWS: usize = 12;
const PICKER_WIDTH: f32 = 560.;
/// Кегль строк списка.
const LIST_TEXT_SIZE: f32 = 13.;

actions!(
    picker,
    [
        SelectNext,
        SelectPrevious,
        SelectNextPage,
        SelectPreviousPage,
        Confirm,
        Dismiss,
    ]
);

pub fn init(cx: &mut App) {
    let context = Some("Picker");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("ctrl-n", SelectNext, context),
        KeyBinding::new("ctrl-p", SelectPrevious, context),
        KeyBinding::new("pagedown", SelectNextPage, context),
        KeyBinding::new("pageup", SelectPreviousPage, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// Что показывает список выбора. Про клавиши, выбор и прокрутку делегат не знает.
pub trait PickerDelegate: Sized + 'static {
    /// Подсказка в пустом поле запроса.
    fn placeholder(&self) -> SharedString;

    fn match_count(&self) -> usize;

    /// Запрос изменился (и один раз при создании — с пустым запросом): пересчитать
    /// совпадения. Можно асинхронно: по готовности перерисовать Picker (`cx.notify()` на
    /// нём) — выбранная строка сама останется в пределах `match_count`.
    fn update_matches(&mut self, query: &str, window: &mut Window, cx: &mut Context<Picker<Self>>);

    /// Enter или щелчок по строке `index` (< `match_count`). Закрывает окно сам делегат —
    /// `cx.emit(DismissEvent)`, — когда и если нужно: палитре сначала нужно вернуть фокус
    /// и отправить действие.
    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>);

    /// Содержимое строки `index`; фон выбранной строки и строки под мышью рисует Picker.
    /// Вызывается только для видимых строк — здесь можно лениво досчитать то, что нужно
    /// для отрисовки (например, позиции совпавших символов).
    fn render_match(
        &mut self,
        index: usize,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement;

    /// Строка под списком: например, «1234 files · indexing…».
    fn render_footer(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        None
    }

    /// Текст вместо пустого списка.
    fn empty_message(&self) -> SharedString {
        "No matches".into()
    }
}

pub struct Picker<D: PickerDelegate> {
    pub delegate: D,
    query: Entity<TextInput>,
    selected: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}

impl<D: PickerDelegate> Picker<D> {
    pub fn new(mut delegate: D, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new(delegate.placeholder(), cx));
        let subscription = cx.subscribe_in(&query, window, Self::on_query_event);
        delegate.update_matches("", window, cx);
        Self {
            delegate,
            query,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        }
    }

    fn on_query_event(
        &mut self,
        query: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                let text = query.read(cx).text();
                self.delegate.update_matches(&text, window, cx);
                self.selected = 0;
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        }
    }

    fn clamp_selection(&mut self) {
        let count = self.delegate.match_count();
        self.selected = self.selected.min(count.saturating_sub(1));
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        if count == 0 {
            return;
        }
        let index = index.min(count - 1);
        // Прокрутка на минимум: вверх — строка встаёт к верхнему краю, вниз — к нижнему.
        let strategy = if index < self.selected {
            ScrollStrategy::Top
        } else {
            ScrollStrategy::Bottom
        };
        self.selected = index;
        self.scroll.scroll_to_item(index, strategy);
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected.saturating_sub(1), cx);
    }

    fn select_next_page(&mut self, _: &SelectNextPage, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + MAX_VISIBLE_ROWS - 1, cx);
    }

    fn select_previous_page(
        &mut self,
        _: &SelectPreviousPage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(self.selected.saturating_sub(MAX_VISIBLE_ROWS - 1), cx);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected < self.delegate.match_count() {
            self.delegate.confirm(self.selected, window, cx);
        }
    }

    fn render_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let selected = index == self.selected;
        div()
            .id(index)
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .overflow_hidden()
            .when(selected, |row| row.bg(ui.list_selected))
            .when(!selected, |row| row.hover(|style| style.bg(ui.list_hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.selected = index;
                this.confirm(&Confirm, window, cx);
            }))
            .child(self.delegate.render_match(index, selected, window, cx))
            .into_any_element()
    }
}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        self.clamp_selection();
        let count = self.delegate.match_count();
        let list = if count == 0 {
            div()
                .px_3()
                .py_2()
                .text_color(ui.dim)
                .child(self.delegate.empty_message())
                .into_any_element()
        } else {
            let height = ROW_HEIGHT * count.min(MAX_VISIBLE_ROWS) as f32;
            uniform_list(
                "picker-matches",
                count,
                cx.processor(|this, range: Range<usize>, window, cx| {
                    range
                        .map(|index| this.render_row(index, window, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .h(px(height))
            .into_any_element()
        };
        let footer = self.delegate.render_footer(window, cx);
        div()
            .key_context("Picker")
            .w(px(PICKER_WIDTH))
            .flex()
            .flex_col()
            .bg(ui.panel)
            .border_1()
            .border_color(ui.border)
            .rounded_lg()
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(LIST_TEXT_SIZE))
            .text_color(ui.foreground)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next_page))
            .on_action(cx.listener(Self::select_previous_page))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(div().p_2().child(self.query.clone()))
            .child(
                div()
                    .border_t_1()
                    .border_color(ui.border)
                    .py_1()
                    .child(list),
            )
            .children(footer.map(|footer| {
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(ui.border)
                    .text_color(ui.dim)
                    .child(footer)
            }))
    }
}

/// Строка с подсвеченными символами: `positions` — индексы `char` (как у
/// `flux_search::FuzzyMatch`), подсвечиваются цветом `color`.
pub fn highlighted_text(
    text: impl Into<SharedString>,
    positions: &[usize],
    color: Hsla,
) -> StyledText {
    let text = text.into();
    let style = HighlightStyle {
        color: Some(color),
        ..Default::default()
    };
    let ranges = byte_ranges(&text, positions);
    StyledText::new(text).with_highlights(ranges.into_iter().map(|range| (range, style)))
}

/// Индексы символов → байтовые диапазоны, соседние символы — одним диапазоном.
/// Индексы за концом строки отбрасываются.
pub fn byte_ranges(text: &str, positions: &[usize]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut positions = positions.iter().copied().peekable();
    for (index, (byte, c)) in text.char_indices().enumerate() {
        while positions.next_if(|&p| p < index).is_some() {}
        if positions.next_if_eq(&index).is_none() {
            continue;
        }
        let end = byte + c.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.end == byte => last.end = end,
            _ => ranges.push(byte..end),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_become_merged_byte_ranges() {
        assert_eq!(byte_ranges("move left", &[0, 1, 5]), vec![0..2, 5..6]);
        assert_eq!(byte_ranges("abc", &[]), Vec::<Range<usize>>::new());
        // «п» и «ф» — по 2 байта.
        assert_eq!(byte_ranges("путь к файлу", &[0, 7]), vec![0..2, 12..14]);
        // Повторы и индексы за концом строки не мешают.
        assert_eq!(byte_ranges("ab", &[1, 1, 9]), vec![1..2]);
    }
}
