//! A plugin's page in Settings (stage 8, ADR-029): a form made from the `[[settings]]` of its
//! manifest — a switch, a text field, a number, a choice, a list of strings; values are kept in
//! `settings.json` under `plugins.settings.<id>`.
//!
//! A value equal to the manifest's default isn't stored (Reset brings a setting back to it). A
//! switch or a choice reaches the plugin at once (`settings-changed`); typed text a moment after
//! the typing stops, so a pattern being typed doesn't make a plugin redo its work at every key.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use flux_plugin::manifest::{SettingKind, SettingSpec};
use flux_plugin::registry::PluginEntry;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, Focusable, Render,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use serde_json::Value;

use crate::i18n::{lang_code, tr, trf};
use crate::icons::IconName;
use crate::input::{InputEvent, TextInput};
use crate::plugins::PluginStore;
use crate::settings;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// How long typed text waits before the plugin learns it.
const NOTIFY_DELAY: Duration = Duration::from_millis(400);

pub fn init(cx: &mut App) {
    let _ = cx;
}

/// The settings page of one plugin.
pub struct PluginSettingsPage {
    plugin: SharedString,
    /// The plugin as it was when the page was made: the settings and their translations.
    entry: Option<Arc<PluginEntry>>,
    plugins: Entity<PluginStore>,
    fields: Vec<Field>,
    /// Bumped by every change: a delayed notification goes out only if no newer change came.
    generation: Rc<Cell<u64>>,
}

struct Field {
    spec: SettingSpec,
    state: FieldState,
    /// Why the typed value isn't saved: not a number, out of range.
    error: Option<SharedString>,
}

enum FieldState {
    /// A switch or a choice: drawn from the stored value.
    Plain,
    /// Text or a number: the field keeps what is typed.
    Input(Row),
    /// A list of strings: a field per row.
    List(Vec<Row>),
}

/// A text field and the subscription to its changes.
struct Row {
    input: Entity<TextInput>,
    _subscription: Subscription,
}

impl PluginSettingsPage {
    pub fn new(
        plugin: SharedString,
        plugins: Entity<PluginStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = window;
        let entry = plugins
            .read(cx)
            .plugin(&plugin)
            .map(|state| state.entry.clone());
        let mut page = Self {
            plugin,
            entry,
            plugins,
            fields: Vec::new(),
            generation: Rc::new(Cell::new(0)),
        };
        let specs = page
            .entry
            .as_ref()
            .map(|entry| entry.manifest.settings.clone())
            .unwrap_or_default();
        page.fields = specs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                let value = page.value(&spec, cx);
                let state = page.state_for(index, &spec, &value, cx);
                Field {
                    spec,
                    state,
                    error: None,
                }
            })
            .collect();
        page
    }

    /// A manifest string in the interface language.
    fn tr<'a>(&'a self, text: &'a str) -> &'a str {
        match &self.entry {
            Some(entry) => entry.translate(lang_code(), text),
            None => text,
        }
    }

    /// The setting's value: the user's, or the manifest's default.
    fn value(&self, spec: &SettingSpec, cx: &App) -> Value {
        settings::plugin_values(&self.plugin, cx)
            .get(&spec.key)
            .cloned()
            .unwrap_or_else(|| spec.default.clone())
    }

    /// The fields of a setting, filled with `value`.
    fn state_for(
        &self,
        index: usize,
        spec: &SettingSpec,
        value: &Value,
        cx: &mut Context<Self>,
    ) -> FieldState {
        match &spec.kind {
            SettingKind::Bool | SettingKind::Choice(_) => FieldState::Plain,
            SettingKind::String => {
                FieldState::Input(self.row(index, value.as_str().unwrap_or(""), cx))
            }
            SettingKind::Integer { .. } => {
                let text = value.as_i64().map(|n| n.to_string()).unwrap_or_default();
                FieldState::Input(self.row(index, &text, cx))
            }
            SettingKind::StringList => FieldState::List(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|item| self.row(index, item, cx))
                    .collect(),
            ),
        }
    }

    /// A text field of the setting at `index`; its changes are saved.
    fn row(&self, index: usize, text: &str, cx: &mut Context<Self>) -> Row {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx);
            input.set_text(text, cx);
            input
        });
        let subscription = cx.subscribe(&input, move |this, _, _: &InputEvent, cx| {
            this.typed(index, cx)
        });
        Row {
            input,
            _subscription: subscription,
        }
    }

    /// Stores a value (none when it is the default) and tells the plugin: at once, or after a
    /// pause when typed.
    fn store(&mut self, index: usize, value: Value, typed: bool, cx: &mut Context<Self>) {
        let spec = &self.fields[index].spec;
        let stored = (value != spec.default).then_some(value);
        settings::set_plugin_value(&self.plugin, &spec.key, stored, cx);
        self.tell_plugin(typed, cx);
        cx.notify();
    }

    fn tell_plugin(&mut self, delayed: bool, cx: &mut Context<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let plugin = self.plugin.clone();
        if !delayed {
            self.plugins
                .update(cx, |store, cx| store.settings_changed(&plugin, cx));
            return;
        }
        // Detached: Settings may close before the pause is over, and the change must still reach
        // the plugin.
        let plugins = self.plugins.clone();
        let latest = self.generation.clone();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(NOTIFY_DELAY).await;
            if latest.get() == generation {
                plugins
                    .update(cx, |store, cx| store.settings_changed(&plugin, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// A field of the setting at `index` changed: its value is checked and saved.
    fn typed(&mut self, index: usize, cx: &mut Context<Self>) {
        let field = &self.fields[index];
        let value = match (&field.spec.kind, &field.state) {
            (SettingKind::String, FieldState::Input(row)) => {
                Ok(Value::String(row.input.read(cx).text()))
            }
            (SettingKind::Integer { min, max }, FieldState::Input(row)) => {
                parse_integer(&row.input.read(cx).text(), *min, *max).map(Value::from)
            }
            (SettingKind::StringList, FieldState::List(rows)) => Ok(Value::Array(
                rows.iter()
                    .map(|row| row.input.read(cx).text())
                    .filter(|text| !text.is_empty())
                    .map(Value::String)
                    .collect(),
            )),
            _ => return,
        };
        match value {
            Ok(value) => {
                self.fields[index].error = None;
                self.store(index, value, true, cx);
            }
            Err(error) => {
                self.fields[index].error = Some(error);
                cx.notify();
            }
        }
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        let on = self.value(&self.fields[index].spec, cx).as_bool() == Some(true);
        self.store(index, Value::Bool(!on), false, cx);
    }

    fn choose(&mut self, index: usize, option: String, cx: &mut Context<Self>) {
        self.store(index, Value::String(option), false, cx);
    }

    /// «+» of a list: an empty row, focused; it is saved once something is typed.
    fn add_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.row(index, "", cx);
        window.focus(&row.input.focus_handle(cx));
        if let FieldState::List(rows) = &mut self.fields[index].state {
            rows.push(row);
        }
        cx.notify();
    }

    /// «−» of a list row.
    fn remove_row(&mut self, index: usize, row: usize, cx: &mut Context<Self>) {
        if let FieldState::List(rows) = &mut self.fields[index].state
            && row < rows.len()
        {
            rows.remove(row);
        }
        self.typed(index, cx);
        // The notification of a removed row needn't wait: nothing is being typed.
        self.tell_plugin(false, cx);
    }

    /// Reset: the manifest's default, and its fields refilled.
    fn reset(&mut self, index: usize, cx: &mut Context<Self>) {
        let spec = self.fields[index].spec.clone();
        settings::set_plugin_value(&self.plugin, &spec.key, None, cx);
        let state = self.state_for(index, &spec, &spec.default, cx);
        self.fields[index].state = state;
        self.fields[index].error = None;
        self.tell_plugin(false, cx);
        cx.notify();
    }
}

/// A number field's text as a value: an error tells what's wrong.
fn parse_integer(text: &str, min: Option<i64>, max: Option<i64>) -> Result<i64, SharedString> {
    let range_error = || -> SharedString {
        match (min, max) {
            (Some(min), Some(max)) => trf("From {0} to {1}", &[&min, &max]).into(),
            (Some(min), None) => trf("At least {0}", &[&min]).into(),
            (None, Some(max)) => trf("At most {0}", &[&max]).into(),
            (None, None) => tr("A whole number").into(),
        }
    };
    let value: i64 = text
        .trim()
        .parse()
        .map_err(|_| SharedString::from(tr("A whole number")))?;
    if min.is_some_and(|min| value < min) || max.is_some_and(|max| value > max) {
        return Err(range_error());
    }
    Ok(value)
}

// --- Drawing ---

impl PluginSettingsPage {
    fn render_field(&self, index: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let field = &self.fields[index];
        let spec = &field.spec;
        let value = self.value(spec, cx);
        let changed = value != spec.default;
        let title = self.tr(&spec.title).to_string();
        let description = spec
            .description
            .as_deref()
            .map(|text| self.tr(text).to_string());
        let reset = changed.then(|| {
            div()
                .id(SharedString::from(format!("plugin-setting-reset-{index}")))
                .flex_none()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.accent_text)
                .cursor_pointer()
                .hover(|style| style.underline())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.reset(index, cx)))
                .child(tr("Reset"))
        });
        let caption = |reset| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(title.clone()))
                        .children(reset),
                )
                .children(description.clone().map(|text| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(text)
                }))
        };
        let card = div()
            .id(SharedString::from(format!("plugin-setting-{index}")))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border);
        match (&spec.kind, &field.state) {
            // A switch: the whole card toggles it, as the switches of the other pages.
            (SettingKind::Bool, _) => card
                .flex_row()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(index, cx)))
                .child(div().flex_1().min_w_0().child(caption(reset)))
                .child(ui::switch(
                    SharedString::from(format!("plugin-setting-switch-{index}")),
                    value.as_bool() == Some(true),
                    ui,
                ))
                .into_any_element(),
            (SettingKind::Choice(options), _) => {
                let current = value.as_str().unwrap_or_default().to_string();
                let rows = options.iter().enumerate().map(|(position, option)| {
                    let selected = option.value == current;
                    let chosen = option.value.clone();
                    div()
                        .id(SharedString::from(format!(
                            "plugin-setting-{index}-option-{position}"
                        )))
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .px_2()
                        .py_1()
                        .rounded(px(ui::RADIUS_SM))
                        .cursor_pointer()
                        .hover(move |style| style.bg(ui.hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.choose(index, chosen.clone(), cx)
                        }))
                        .child(ui::radio(
                            SharedString::from(format!("plugin-setting-{index}-radio-{position}")),
                            selected,
                            ui,
                        ))
                        .child(self.tr(&option.title).to_string())
                });
                card.child(caption(reset))
                    .child(div().flex().flex_col().gap_0p5().children(rows))
                    .into_any_element()
            }
            (_, FieldState::Input(row)) => card
                .child(caption(reset))
                .child(row.input.clone())
                .children(field.error.clone().map(|error| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.error)
                        .child(error)
                }))
                .into_any_element(),
            (_, FieldState::List(rows)) => card
                .child(caption(reset))
                .child(self.render_list(index, rows, window, cx))
                .into_any_element(),
            (_, FieldState::Plain) => card.child(caption(reset)).into_any_element(),
        }
    }

    /// A list of strings: a field per row with «−», and «+ Add» under them (the list editor of
    /// JetBrains IDEs).
    fn render_list(
        &self,
        index: usize,
        rows: &[Row],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let ui = Theme::ui(cx);
        let _ = window;
        let fields = rows.iter().enumerate().map(|(position, row)| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(div().flex_1().min_w_0().child(row.input.clone()))
                .child(
                    ui::icon_button(
                        SharedString::from(format!("plugin-setting-{index}-remove-{position}")),
                        IconName::Minus,
                        ui,
                    )
                    .tooltip(ui::tooltip(tr("Remove"), None))
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.remove_row(index, position, cx),
                    )),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .children(fields)
            .when(rows.is_empty(), |list| {
                list.child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(tr("The list is empty")),
                )
            })
            .child(
                div().flex().child(
                    ui::text_button(
                        SharedString::from(format!("plugin-setting-{index}-add")),
                        tr("Add"),
                        false,
                        ui,
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| this.add_row(index, window, cx),
                    )),
                ),
            )
    }
}

impl Render for PluginSettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let name = match &self.entry {
            Some(entry) => self.tr(&entry.manifest.name).to_string(),
            None => self.plugin.to_string(),
        };
        let fields: Vec<AnyElement> = (0..self.fields.len())
            .map(|index| self.render_field(index, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(header(&name, ui))
            .when(fields.is_empty(), |page| {
                page.child(
                    div()
                        .text_color(ui.dim)
                        .child(tr("The plugin has no settings.")),
                )
            })
            .children(fields)
    }
}

/// The page's title (the plugin's name) and a line about whose settings these are.
fn header(name: &str, ui: UiColors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TEXT_LG))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(name.to_string()),
        )
        .child(div().text_color(ui.text_muted).child(trf(
            "Settings of the plugin “{0}”: it reads them and follows their changes.",
            &[&name],
        )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_checked_against_their_range() {
        assert_eq!(parse_integer(" 42 ", None, None), Ok(42));
        assert_eq!(parse_integer("-3", Some(-5), Some(5)), Ok(-3));
        assert_eq!(
            parse_integer("x", None, None),
            Err(SharedString::from("A whole number"))
        );
        assert_eq!(
            parse_integer("9", Some(1), Some(5)),
            Err(SharedString::from("From 1 to 5"))
        );
        assert_eq!(
            parse_integer("0", Some(1), None),
            Err(SharedString::from("At least 1"))
        );
    }
}
