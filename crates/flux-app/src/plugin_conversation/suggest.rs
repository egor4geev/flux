//! The field's suggestions (API 0.5, FluxGram part 5): a member for "@", a bot's command for "/".
//! A word at the caret that starts with one of the field's triggers (`composer-spec.triggers`:
//! "@" at a word's start, "/" only at the field's start) asks the plugin — `suggest(trigger,
//! text)` at once when the trigger is typed, then once per burst of typing; its answer
//! (`suggestions(text, items)`) is the list over the field, by the trigger, as Claude's field shows
//! its mentions. While the list has items, the field's ↑ ↓ ↵ ⇥ Esc drive it
//! ([`PromptInput::set_menu_open`]): ↵ or ⇥ (or a click) put the chosen item's `insert` in place
//! of the trigger and the word after it, Esc closes the list until the caret leaves the word. A
//! space, a line break or the caret leaving the word closes it too; an answer for another text
//! than the one at the caret now is dropped.
//!
//! [`PromptInput::set_menu_open`]: crate::prompt_input::PromptInput::set_menu_open

use std::ops::Range;
use std::time::Duration;

use flux_plugin::api::events::UiEvent;
use flux_plugin::api::ui::Suggestion;
use gpui::{
    AnyElement, ClickEvent, Context, Corner, Focusable, ScrollHandle, Task, Window, anchored,
    deferred, div, point, prelude::*, px,
};

use super::{Conversation, render_avatar};
use crate::popup;
use crate::theme::Theme;
use crate::ui;

/// The plugin is asked again this long after the typing stops.
const ASK_DELAY: Duration = Duration::from_millis(120);
/// The list's width, its rows' height and how many are in sight (the rest scroll).
const WIDTH: f32 = 300.;
const ROW_HEIGHT: f32 = 32.;
const ROWS: usize = 6;
const AVATAR: f32 = 22.;
const PADDING: f32 = 5.;

/// The word at the caret that asks for suggestions.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
    pub trigger: String,
    /// The word's characters: from the trigger to the word's end.
    pub range: Range<usize>,
    /// What is typed after the trigger, up to the caret.
    pub query: String,
}

/// The state of the list.
#[derive(Default)]
pub(super) struct Suggest {
    token: Option<Token>,
    items: Vec<Suggestion>,
    /// The chosen item, by its key: it stays chosen while the list changes.
    selected: Option<String>,
    /// Esc closed the list for the word that starts here.
    dismissed: Option<usize>,
    timer: Option<Task<()>>,
    scroll: ScrollHandle,
}

impl Suggest {
    /// The list shows: a word asks and the plugin has items for it.
    pub fn is_open(&self) -> bool {
        self.token.is_some() && !self.items.is_empty()
    }

    fn selected_index(&self) -> usize {
        self.selected
            .as_ref()
            .and_then(|key| self.items.iter().position(|item| item.key == *key))
            .unwrap_or(0)
    }
}

/// The word at `caret` (a character index) when it starts with one of `triggers`: "/" only at
/// the text's start, the others at any word's start.
pub(super) fn token_at(text: &str, caret: usize, triggers: &[String]) -> Option<Token> {
    let chars: Vec<char> = text.chars().collect();
    let caret = caret.min(chars.len());
    let mut start = caret;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = caret;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    let word: String = chars[start..caret].iter().collect();
    let trigger = triggers.iter().find(|trigger| {
        !trigger.is_empty() && word.starts_with(trigger.as_str()) && (*trigger != "/" || start == 0)
    })?;
    Some(Token {
        trigger: trigger.clone(),
        range: start..end,
        query: word[trigger.len()..].to_string(),
    })
}

impl Conversation {
    fn triggers(&self) -> &[String] {
        self.spec
            .composer
            .as_ref()
            .map_or(&[], |composer| composer.triggers.as_slice())
    }

    /// The text or the caret changed: the word at the caret asks (at once when it is a new one,
    /// after the burst of typing when its text changed), or the list closes.
    pub(super) fn update_suggestions(&mut self, cx: &mut Context<Self>) {
        let token = {
            let input = self.input.read(cx);
            (!input.has_selection())
                .then(|| token_at(&input.text(), input.cursor(), self.triggers()))
                .flatten()
        };
        let Some(token) = token else {
            self.suggest.dismissed = None;
            return self.close_suggestions(cx);
        };
        if self.suggest.dismissed == Some(token.range.start) {
            return self.close_suggestions(cx);
        }
        self.suggest.dismissed = None;
        let before = self.suggest.token.replace(token.clone());
        match before {
            Some(before) if before.range.start == token.range.start => {
                if before.query != token.query {
                    self.suggest.timer = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(ASK_DELAY).await;
                        this.update(cx, |this, cx| this.ask_suggestions(cx)).ok();
                    }));
                }
            }
            // A new word: its items are the plugin's answer for it.
            _ => {
                self.suggest.items.clear();
                self.suggest.selected = None;
                self.ask_suggestions(cx);
            }
        }
        self.sync_suggestions(cx);
    }

    fn ask_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggest.timer = None;
        if let Some(token) = &self.suggest.token {
            let event = UiEvent::Suggest((token.trigger.clone(), token.query.clone()));
            self.emit(event, cx);
        }
    }

    /// The plugin's answer for `text`: the list's items, if the word at the caret still asks
    /// for that text.
    pub(super) fn set_suggestions(
        &mut self,
        text: String,
        items: Vec<Suggestion>,
        cx: &mut Context<Self>,
    ) {
        if self
            .suggest
            .token
            .as_ref()
            .is_none_or(|token| token.query != text)
        {
            return;
        }
        let kept = self
            .suggest
            .selected
            .as_ref()
            .is_some_and(|key| items.iter().any(|item| item.key == *key));
        if !kept {
            self.suggest.selected = items.first().map(|item| item.key.clone());
        }
        self.suggest.items = items;
        self.suggest
            .scroll
            .scroll_to_item(self.suggest.selected_index());
        self.sync_suggestions(cx);
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggest.timer = None;
        if self.suggest.token.take().is_some() || !self.suggest.items.is_empty() {
            self.suggest.items.clear();
            self.suggest.selected = None;
            cx.notify();
        }
        self.sync_suggestions(cx);
    }

    /// ↑ ↓ ↵ ⇥ Esc go to the list only while it shows (an empty one lets ↵ send).
    fn sync_suggestions(&mut self, cx: &mut Context<Self>) {
        let open = self.suggest.is_open();
        self.input
            .update(cx, |input, cx| input.set_menu_open(open, cx));
        cx.notify();
    }

    /// ↓ (1) or ↑ (-1) in the list, round.
    pub(super) fn move_suggestion(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.suggest.items.len();
        if count == 0 {
            return;
        }
        let index = (self.suggest.selected_index() as isize + step).rem_euclid(count as isize);
        self.suggest.selected = Some(self.suggest.items[index as usize].key.clone());
        self.suggest.scroll.scroll_to_item(index as usize);
        cx.notify();
    }

    /// Puts the item at `index` in place of the word that asked.
    pub(super) fn accept_suggestion(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = index.unwrap_or_else(|| self.suggest.selected_index());
        let (Some(item), Some(token)) = (self.suggest.items.get(index), &self.suggest.token) else {
            return;
        };
        let (range, insert) = (token.range.clone(), item.insert.clone());
        // The word put stays without a list, whatever it ends with.
        self.suggest.dismissed = Some(range.start);
        self.close_suggestions(cx);
        self.input.update(cx, |input, cx| {
            input.replace(range, &insert, cx);
            window.focus(&input.focus_handle(cx));
        });
    }

    /// Esc: the list closes until the caret leaves the word.
    pub(super) fn dismiss_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggest.dismissed = self.suggest.token.as_ref().map(|token| token.range.start);
        self.close_suggestions(cx);
    }

    /// The list over the field, its left edge by the trigger.
    pub(super) fn render_suggestions(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let token = self.suggest.token.as_ref()?;
        if self.suggest.items.is_empty() {
            return None;
        }
        let field = self.field_bounds.get()?;
        let trigger = self.input.read(cx).bounds_of(token.range.start)?;
        let ui = Theme::ui(cx);
        let selected = self.suggest.selected_index();
        let view = cx.entity_id();
        let rows =
            self.suggest
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let avatar = item.avatar.as_ref().map(|avatar| {
                        render_avatar(avatar, AVATAR, &self.plugin, view, window, cx).flex_none()
                    });
                    div()
                        .id(("fg-suggestion", index))
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(ui::RADIUS_SM))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .cursor_pointer()
                        .when(index == selected, |row| row.bg(ui.list_selected))
                        .when(index != selected, |row| {
                            row.hover(|style| style.bg(ui.hover))
                        })
                        .children(avatar)
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(180.))
                                .truncate()
                                .child(item.title.clone()),
                        )
                        .children(item.detail.clone().map(|detail| {
                            div().min_w_0().truncate().text_color(ui.dim).child(detail)
                        }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.accept_suggestion(Some(index), window, cx)
                        }))
                })
                .collect::<Vec<_>>();
        let list = popup::panel(ui)
            .w(px(WIDTH))
            .p(px(PADDING))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_suggestions(cx)))
            .child(
                div()
                    .id("fg-suggestions")
                    .flex()
                    .flex_col()
                    .max_h(px(ROW_HEIGHT * ROWS as f32))
                    .overflow_y_scroll()
                    .track_scroll(&self.suggest.scroll)
                    .children(rows),
            );
        // Its first row's text starts under the trigger.
        let left = trigger.left() - px(PADDING + 8.);
        Some(
            deferred(
                anchored()
                    .anchor(Corner::BottomLeft)
                    .position(point(left, field.top() - px(popup::POPUP_GAP)))
                    .snap_to_window_with_margin(px(popup::WINDOW_MARGIN))
                    .child(list),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triggers() -> Vec<String> {
        vec!["@".into(), "/".into()]
    }

    #[test]
    fn a_trigger_asks_at_a_word_start_and_slash_only_at_the_start() {
        let token = token_at("Hi @ан", 6, &triggers()).unwrap();
        assert_eq!(token.trigger, "@");
        assert_eq!(token.range, 3..6);
        assert_eq!(token.query, "ан");
        // The query is what is before the caret; the word's end is replaced too.
        let token = token_at("@annabel", 3, &triggers()).unwrap();
        assert_eq!((token.query.as_str(), token.range), ("an", 0..8));
        assert_eq!(token_at("/st", 3, &triggers()).unwrap().query, "st");
        assert_eq!(token_at("@", 1, &triggers()).unwrap().query, "");
        // "/" in the middle, "@" inside a word, a space after it: nothing asks.
        assert_eq!(token_at("a /st", 5, &triggers()), None);
        assert_eq!(token_at("mail@host", 9, &triggers()), None);
        assert_eq!(token_at("@anna ", 6, &triggers()), None);
        assert_eq!(token_at("line\n@x", 7, &triggers()).unwrap().range, 5..7);
        assert_eq!(token_at("@anna", 5, &[]), None);
    }
}
