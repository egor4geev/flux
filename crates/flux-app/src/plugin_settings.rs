//! A plugin's page in Settings (stage 8, ADR-029): a form made from the `[[settings]]` of its
//! manifest — a switch, a text field, a number, a choice, a list of strings; values are kept in
//! `settings.json` under `plugins.settings.<id>`.
//!
//! A value equal to the manifest's default isn't stored (Reset brings a setting back to it). A
//! switch or a choice reaches the plugin at once (`settings-changed`); typed text a moment after
//! the typing stops, so a pattern being typed doesn't make a plugin redo its work at every key.
//!
//! API 0.3 (part 10.3) adds what a sign-in needs: a `section` — a heading with its description
//! over the settings that follow; a `secret` — a masked field whose value goes to the macOS
//! keychain (the plugin's secret of that key, `secrets.get`), never to `settings.json`; a `button`
//! — runs one of the plugin's commands (buttons in a row share a line); a `status` — a line the
//! plugin writes (`settings.set-status`: "Signed in as …", a spinner while it works). A value the
//! plugin writes itself (`settings.set`) shows at once.
//!
//! And what a plugin's own actions need: `text` — several lines (a prompt, a template), growing
//! with the text; `records` — a list of records with the same fields (a name, a prompt, a mode, a
//! switch), a card each: collapsed to its name and what's switched on, added (the new one open,
//! its first field with the keyboard), moved (⌥⇧↑ / ⌥⇧↓), removed (asked first when something is
//! typed in it). ⇥ / ⇧⇥ go from field to field; ↵ in a multi-line field is a new line.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use flux_plugin::api::settings::SettingStatus;
use flux_plugin::api::types::CommandSource;
use flux_plugin::api::ui::Tone;
use flux_plugin::manifest::{RecordField, RecordFieldKind, SettingKind, SettingSpec};
use flux_plugin::registry::PluginEntry;
use gpui::{
    Animation, AnimationExt as _, AnyElement, AnyWindowHandle, App, AppContext as _, ClickEvent,
    Context, CursorStyle, Div, Entity, FocusHandle, Focusable, KeyBinding, Render, SharedString,
    Subscription, WeakEntity, Window, actions, div, prelude::*, px,
};
use serde_json::{Map, Value};

use crate::dialog::Dialog;
use crate::i18n::{lang_code, tr, trf};
use crate::icons::IconName;
use crate::input::{InputEvent, TextInput};
use crate::plugins::{CommandOrigin, PluginStore};
use crate::prompt_input::{PromptInput, PromptInputEvent};
use crate::settings;
use crate::theme::{self, Theme, UiColors};
use crate::ui;

/// How long typed text waits before the plugin learns it.
const NOTIFY_DELAY: Duration = Duration::from_millis(400);
/// A `text` setting grows to this many rows, then scrolls.
const TEXT_ROWS: usize = 10;
/// A record's multi-line field grows to this many rows.
const RECORD_TEXT_ROWS: usize = 8;
/// The page: ⇥ / ⇧⇥ between its fields.
const PAGE_CONTEXT: &str = "PluginSettingsPage";
/// A multi-line field: ↵ is a new line.
const LINES_CONTEXT: &str = "PluginSettingsLines";
/// A record's card: ⌥⇧↑ / ⌥⇧↓ move it.
const RECORD_CONTEXT: &str = "PluginSettingsRecord";

// The records of a `records` setting and the page's keyboard (API 0.3). The record actions act on
// the record that has the keyboard; without one (a scenario's `action:` step) — on the open page's
// first `records` setting: AddRecord adds to it, ToggleRecord opens its first card, the others
// take its last card; NextField focuses the page's first field.
actions!(
    plugin_settings,
    [
        AddRecord,
        RemoveRecord,
        MoveRecordUp,
        MoveRecordDown,
        ToggleRecord,
        NextField,
        PreviousField,
        NewLine,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NextField, Some(PAGE_CONTEXT)),
        KeyBinding::new("shift-tab", PreviousField, Some(PAGE_CONTEXT)),
        KeyBinding::new("enter", NewLine, Some(LINES_CONTEXT)),
        KeyBinding::new("alt-shift-up", MoveRecordUp, Some(RECORD_CONTEXT)),
        KeyBinding::new("alt-shift-down", MoveRecordDown, Some(RECORD_CONTEXT)),
    ]);
    // Without the keyboard in the page, the record actions still reach the open one.
    cx.on_action(|_: &AddRecord, cx| {
        with_open_page(cx, |page, window, cx| page.add_record_action(window, cx))
    });
    cx.on_action(|_: &RemoveRecord, cx| {
        with_open_page(cx, |page, window, cx| page.remove_record_action(window, cx))
    });
    cx.on_action(|_: &MoveRecordUp, cx| {
        with_open_page(cx, |page, window, cx| {
            page.move_record_action(-1, window, cx)
        })
    });
    cx.on_action(|_: &MoveRecordDown, cx| {
        with_open_page(cx, |page, window, cx| {
            page.move_record_action(1, window, cx)
        })
    });
    cx.on_action(|_: &ToggleRecord, cx| {
        with_open_page(cx, |page, window, cx| page.toggle_record_action(window, cx))
    });
    // ⇥ with the keyboard outside the page (a scenario's step): its first field.
    cx.on_action(|_: &NextField, cx| {
        with_open_page(cx, |page, window, cx| page.focus_step(true, window, cx))
    });
}

thread_local! {
    /// The page drawn last, and its window: the record actions reach it without the keyboard.
    static OPEN_PAGE: std::cell::RefCell<Option<(WeakEntity<PluginSettingsPage>, AnyWindowHandle)>> =
        const { std::cell::RefCell::new(None) };
}

fn with_open_page(
    cx: &mut App,
    f: impl FnOnce(&mut PluginSettingsPage, &mut Window, &mut Context<PluginSettingsPage>) + 'static,
) {
    // The action runs while its window is being updated: the page's window is reached once that
    // update is over.
    cx.defer(move |cx| {
        let Some((page, handle)) = OPEN_PAGE.with(|open| open.borrow().clone()) else {
            return;
        };
        let Some(page) = page.upgrade() else {
            return;
        };
        handle
            .update(cx, |_, window, cx| {
                page.update(cx, |page, cx| f(page, window, cx))
            })
            .ok();
    });
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
    /// The store: the plugin's status lines, the values it writes, whether it runs (buttons).
    _subscription: Option<Subscription>,
    /// The ids of records' cards: unique on the page, so a card's elements stay with it when
    /// records move.
    next_card: Cell<u64>,
}

struct Field {
    spec: SettingSpec,
    state: FieldState,
    /// Why the typed value isn't saved: not a number, out of range; a secret the keychain refused.
    error: Option<SharedString>,
    /// The value the fields were filled with or saved: a different stored one — the plugin wrote
    /// it (`settings.set`) — refills them.
    synced: Value,
}

enum FieldState {
    /// A switch or a choice: drawn from the stored value; a button, a status, a section: no value.
    Plain,
    /// Text or a number: the field keeps what is typed.
    Input(Row),
    /// A list of strings: a field per row.
    List(Vec<Row>),
    /// A secret: a masked field. What is typed goes to the keychain once the typing stops; the
    /// field stays empty when a value is there already — it isn't shown.
    Secret(Secret),
    /// Several lines of text.
    Lines(Lines),
    /// A list of records: a card each.
    Records(Vec<RecordCard>),
}

/// A multi-line field and the subscription to its changes.
struct Lines {
    input: Entity<PromptInput>,
    _subscription: Subscription,
}

/// A record of a `records` setting: its card.
struct RecordCard {
    id: u64,
    /// A field's input per field of the setting, in its order.
    inputs: Vec<RecordInput>,
    expanded: bool,
}

/// The input of a record's field.
enum RecordInput {
    Line(Row),
    Lines(Lines),
    Switch(bool),
    /// The chosen option's value.
    Choice(String),
}

struct Secret {
    row: Row,
    /// The keychain has a value: none until it is read (in the background, when the page opens).
    saved: Option<bool>,
    /// Bumped by every keystroke: the typed value is saved only if no newer one came.
    generation: Rc<Cell<u64>>,
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
        let subscription = cx.observe_in(&plugins, window, |this, _, window, cx| {
            this.sync(window, cx);
            cx.notify();
        });
        let mut page = Self {
            plugin,
            entry,
            plugins,
            fields: Vec::new(),
            generation: Rc::new(Cell::new(0)),
            _subscription: Some(subscription),
            next_card: Cell::new(0),
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
                    synced: value,
                }
            })
            .collect();
        page.read_secrets(cx);
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
            SettingKind::Secret => FieldState::Secret(Secret {
                row: self.masked_row(index, cx),
                saved: None,
                generation: Rc::new(Cell::new(0)),
            }),
            SettingKind::Text => {
                FieldState::Lines(self.lines(index, value.as_str().unwrap_or(""), TEXT_ROWS, cx))
            }
            SettingKind::Records(fields) => {
                let records: Vec<&Value> = value.as_array().into_iter().flatten().collect();
                // A single record shows its fields at once; more — their names.
                let expanded = records.len() == 1;
                FieldState::Records(
                    records
                        .into_iter()
                        .map(|record| self.record_card(index, fields, record, expanded, cx))
                        .collect(),
                )
            }
            SettingKind::Button { .. } | SettingKind::Status | SettingKind::Section => {
                FieldState::Plain
            }
        }
    }

    /// A text field of the setting at `index`; its changes are saved.
    fn row(&self, index: usize, text: &str, cx: &mut Context<Self>) -> Row {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx);
            input.set_text(text, cx);
            input
        });
        self.subscribed(index, input, cx)
    }

    /// The masked field of a secret.
    fn masked_row(&self, index: usize, cx: &mut Context<Self>) -> Row {
        let input = cx.new(|cx| TextInput::new(tr("Not set"), cx).masked());
        self.subscribed(index, input, cx)
    }

    fn subscribed(&self, index: usize, input: Entity<TextInput>, cx: &mut Context<Self>) -> Row {
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
        self.fields[index].synced = value.clone();
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
        if let FieldState::Secret(secret) = &field.state {
            let text = secret.row.input.read(cx).text();
            return self.save_secret_later(index, text, cx);
        }
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
            (SettingKind::Text, FieldState::Lines(lines)) => {
                Ok(Value::String(lines.input.read(cx).text()))
            }
            (SettingKind::Records(fields), FieldState::Records(cards)) => {
                Ok(records_value(fields, cards, cx))
            }
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
        self.fields[index].synced = spec.default.clone();
        self.tell_plugin(false, cx);
        cx.notify();
    }

    /// The store changed: values the plugin wrote (`settings.set`) refill their fields — unless the
    /// user is typing in one.
    fn sync(&mut self, window: &Window, cx: &mut Context<Self>) {
        for index in 0..self.fields.len() {
            let field = &self.fields[index];
            if !field.spec.kind.has_value() {
                continue;
            }
            let value = self.value(&field.spec, cx);
            if value == field.synced {
                continue;
            }
            let typing = match &field.state {
                FieldState::Input(row) => row.input.focus_handle(cx).is_focused(window),
                FieldState::List(rows) => rows
                    .iter()
                    .any(|row| row.input.focus_handle(cx).is_focused(window)),
                FieldState::Lines(lines) => lines.input.focus_handle(cx).is_focused(window),
                FieldState::Records(cards) => cards.iter().any(|card| {
                    card.inputs
                        .iter()
                        .any(|input| input_focus(input, cx).is_some_and(|f| f.is_focused(window)))
                }),
                _ => false,
            };
            if typing {
                continue;
            }
            // Cards that were open stay open (by place).
            let open: Vec<bool> = match &field.state {
                FieldState::Records(cards) => cards.iter().map(|card| card.expanded).collect(),
                _ => Vec::new(),
            };
            let spec = field.spec.clone();
            let mut state = self.state_for(index, &spec, &value, cx);
            if let FieldState::Records(cards) = &mut state {
                for (card, expanded) in cards.iter_mut().zip(open) {
                    card.expanded = expanded;
                }
            }
            let field = &mut self.fields[index];
            field.state = state;
            field.error = None;
            field.synced = value;
        }
    }

    // --- Secrets, buttons, status lines (API 0.3) ---

    /// Whether the keychain has the secrets of the page: read in the background (the keychain may
    /// ask the user to unlock it).
    fn read_secrets(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.fields.len() {
            if !matches!(self.fields[index].state, FieldState::Secret(_)) {
                continue;
            }
            let plugin = self.plugin.to_string();
            let key = self.fields[index].spec.key.clone();
            let read =
                cx.background_spawn(async move { flux_plugin::plugin_secret(&plugin, &key) });
            cx.spawn(async move |this, cx| {
                let result = read.await;
                this.update(cx, |this, cx| {
                    let field = &mut this.fields[index];
                    if let FieldState::Secret(secret) = &mut field.state {
                        match result {
                            Ok(value) => secret.mark_saved(value.is_some(), cx),
                            Err(err) => field.error = Some(keychain_error(&err)),
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Typed into a secret: once the typing stops, the text goes to the keychain (an emptied field
    /// keeps the saved value: Clear takes it away).
    fn save_secret_later(&mut self, index: usize, text: String, cx: &mut Context<Self>) {
        let FieldState::Secret(secret) = &self.fields[index].state else {
            return;
        };
        let generation = secret.generation.get() + 1;
        secret.generation.set(generation);
        if text.is_empty() {
            return;
        }
        let latest = secret.generation.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTIFY_DELAY).await;
            if latest.get() == generation {
                this.update(cx, |this, cx| this.write_secret(index, Some(text), cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Saves (or, none, removes) a secret in the background; then the plugin hears of the change.
    fn write_secret(&mut self, index: usize, value: Option<String>, cx: &mut Context<Self>) {
        let plugin = self.plugin.to_string();
        let key = self.fields[index].spec.key.clone();
        let saved = value.is_some();
        let write = cx.background_spawn(async move {
            flux_plugin::set_plugin_secret(&plugin, &key, value.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                let field = &mut this.fields[index];
                match result {
                    Ok(()) => {
                        field.error = None;
                        if let FieldState::Secret(secret) = &mut field.state {
                            secret.mark_saved(saved, cx);
                        }
                        this.tell_plugin(false, cx);
                    }
                    Err(err) => field.error = Some(keychain_error(&err)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Clear of a secret: out of the keychain, and the field emptied.
    fn clear_secret(&mut self, index: usize, cx: &mut Context<Self>) {
        if let FieldState::Secret(secret) = &self.fields[index].state {
            // A save still waiting for the typing to stop doesn't come after the removal.
            secret.generation.set(secret.generation.get() + 1);
            secret
                .row
                .input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        self.write_secret(index, None, cx);
    }

    /// A button: runs the plugin's command, from Settings, with the button's argument.
    fn press(&mut self, index: usize, cx: &mut Context<Self>) {
        let SettingKind::Button { command, argument } = &self.fields[index].spec.kind else {
            return;
        };
        let origin = CommandOrigin::with_argument(CommandSource::Settings, argument.clone());
        let (plugin, command) = (self.plugin.clone(), command.clone());
        self.plugins.update(cx, |store, cx| {
            store.run_command(&plugin, &command, Some(&origin), cx)
        });
    }
}

// --- Text and records (API 0.3) ---

impl PluginSettingsPage {
    /// A multi-line field of the setting at `index`; its changes are saved.
    fn lines(&self, index: usize, text: &str, rows: usize, cx: &mut Context<Self>) -> Lines {
        let input = cx.new(|cx| {
            let mut input = PromptInput::new("", cx).max_rows(rows);
            input.set_text(text, cx);
            // A long prompt shows its beginning: the caret at the start (an empty replacement
            // puts it there).
            input.replace(0..0, "", cx);
            input
        });
        let subscription = cx.subscribe(&input, move |this, _, event: &PromptInputEvent, cx| {
            if matches!(event, PromptInputEvent::Changed) {
                this.typed(index, cx)
            }
        });
        Lines {
            input,
            _subscription: subscription,
        }
    }

    /// A record's card, its fields filled with the record's values — a field's default where the
    /// record lacks the key (or its value doesn't fit).
    fn record_card(
        &self,
        index: usize,
        fields: &[RecordField],
        record: &Value,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> RecordCard {
        let inputs = fields
            .iter()
            .map(|field| {
                let value = record
                    .get(&field.key)
                    .filter(|value| field.kind.fits(value))
                    .unwrap_or(&field.default);
                let text = value.as_str().unwrap_or("");
                match &field.kind {
                    RecordFieldKind::String => RecordInput::Line(self.row(index, text, cx)),
                    RecordFieldKind::Text => {
                        RecordInput::Lines(self.lines(index, text, RECORD_TEXT_ROWS, cx))
                    }
                    RecordFieldKind::Bool => RecordInput::Switch(value.as_bool() == Some(true)),
                    RecordFieldKind::Choice(_) => RecordInput::Choice(text.to_string()),
                }
            })
            .collect();
        let id = self.next_card.get();
        self.next_card.set(id + 1);
        RecordCard {
            id,
            inputs,
            expanded,
        }
    }

    /// Saves a `records` setting as its cards hold it, and tells the plugin at once.
    fn store_records(&mut self, index: usize, cx: &mut Context<Self>) {
        let field = &self.fields[index];
        let (SettingKind::Records(fields), FieldState::Records(cards)) =
            (&field.spec.kind, &field.state)
        else {
            return;
        };
        let value = records_value(fields, cards, cx);
        self.store(index, value, false, cx);
    }

    /// The cards of the `records` setting at `index`.
    fn cards_mut(&mut self, index: usize) -> Option<&mut Vec<RecordCard>> {
        match &mut self.fields.get_mut(index)?.state {
            FieldState::Records(cards) => Some(cards),
            _ => None,
        }
    }

    /// Add: a record of the fields' defaults at the end, open, its first text field with the
    /// keyboard.
    fn add_record(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let SettingKind::Records(fields) = self.fields[index].spec.kind.clone() else {
            return;
        };
        let card = self.record_card(index, &fields, &Value::Object(Map::new()), true, cx);
        if let Some(focus) = card.inputs.iter().find_map(|input| input_focus(input, cx)) {
            window.focus(&focus);
        }
        if let Some(cards) = self.cards_mut(index) {
            cards.push(card);
        }
        self.store_records(index, cx);
    }

    /// Remove of a card: asks first when something is typed in the record.
    fn remove_record(
        &mut self,
        index: usize,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = &self.fields[index];
        let (SettingKind::Records(fields), FieldState::Records(cards)) =
            (&field.spec.kind, &field.state)
        else {
            return;
        };
        let Some(card) = cards.iter().find(|card| card.id == id) else {
            return;
        };
        if record_is_empty(card, cx) {
            return self.drop_record(index, id, cx);
        }
        let name = record_name(fields, card, cx).unwrap_or_else(|| tr("Untitled").to_string());
        let answer = Dialog::warning(trf("Remove “{0}”?", &[&name]))
            .message(tr(
                "Its fields go with it; Reset brings back the plugin's own list.",
            ))
            .danger(tr("Remove"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Some(0) {
                this.update(cx, |this, cx| this.drop_record(index, id, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn drop_record(&mut self, index: usize, id: u64, cx: &mut Context<Self>) {
        if let Some(cards) = self.cards_mut(index) {
            cards.retain(|card| card.id != id);
        }
        self.store_records(index, cx);
    }

    /// Moves a card one place up (−1) or down (1).
    fn move_record(&mut self, index: usize, id: u64, delta: isize, cx: &mut Context<Self>) {
        let Some(cards) = self.cards_mut(index) else {
            return;
        };
        let Some(place) = cards.iter().position(|card| card.id == id) else {
            return;
        };
        let Some(target) = place
            .checked_add_signed(delta)
            .filter(|&target| target < cards.len())
        else {
            return;
        };
        cards.swap(place, target);
        self.store_records(index, cx);
    }

    fn toggle_card(&mut self, index: usize, id: u64, cx: &mut Context<Self>) {
        if let Some(card) = self
            .cards_mut(index)
            .and_then(|cards| cards.iter_mut().find(|card| card.id == id))
        {
            card.expanded = !card.expanded;
        }
        cx.notify();
    }

    /// A record's switch.
    fn toggle_record_switch(
        &mut self,
        index: usize,
        id: u64,
        field: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(RecordInput::Switch(on)) = self
            .cards_mut(index)
            .and_then(|cards| cards.iter_mut().find(|card| card.id == id))
            .and_then(|card| card.inputs.get_mut(field))
        {
            *on = !*on;
        }
        self.store_records(index, cx);
    }

    /// A record's choice.
    fn choose_record_option(
        &mut self,
        index: usize,
        id: u64,
        field: usize,
        option: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(RecordInput::Choice(value)) = self
            .cards_mut(index)
            .and_then(|cards| cards.iter_mut().find(|card| card.id == id))
            .and_then(|card| card.inputs.get_mut(field))
        {
            *value = option;
        }
        self.store_records(index, cx);
    }

    /// The record whose field has the keyboard: (the setting, the card's id).
    fn focused_record(&self, window: &Window, cx: &App) -> Option<(usize, u64)> {
        self.fields.iter().enumerate().find_map(|(index, field)| {
            let FieldState::Records(cards) = &field.state else {
                return None;
            };
            cards
                .iter()
                .find(|card| {
                    card.inputs.iter().any(|input| {
                        input_focus(input, cx).is_some_and(|focus| focus.is_focused(window))
                    })
                })
                .map(|card| (index, card.id))
        })
    }

    /// The page's first `records` setting.
    fn first_records(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| matches!(field.state, FieldState::Records(_)))
    }

    /// The record an action is for: the one with the keyboard, else the last one of the page's
    /// first `records` setting.
    fn action_record(&self, window: &Window, cx: &App) -> Option<(usize, u64)> {
        self.focused_record(window, cx).or_else(|| {
            let index = self.first_records()?;
            match &self.fields[index].state {
                FieldState::Records(cards) => cards.last().map(|card| (index, card.id)),
                _ => None,
            }
        })
    }

    fn add_record_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self
            .focused_record(window, cx)
            .map(|(index, _)| index)
            .or_else(|| self.first_records());
        if let Some(index) = index {
            self.add_record(index, window, cx);
        }
    }

    fn remove_record_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((index, id)) = self.action_record(window, cx) {
            self.remove_record(index, id, window, cx);
        }
    }

    fn move_record_action(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((index, id)) = self.action_record(window, cx) {
            self.move_record(index, id, delta, cx);
        }
    }

    fn toggle_record_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.focused_record(window, cx).or_else(|| {
            let index = self.first_records()?;
            match &self.fields[index].state {
                FieldState::Records(cards) => cards.first().map(|card| (index, card.id)),
                _ => None,
            }
        });
        if let Some((index, id)) = target {
            self.toggle_card(index, id, cx);
        }
    }

    /// The fields the keyboard goes through with ⇥, in the page's order (the open records').
    fn focusables(&self, cx: &App) -> Vec<FocusHandle> {
        let mut handles = Vec::new();
        for field in &self.fields {
            match &field.state {
                FieldState::Input(row) => handles.push(row.input.focus_handle(cx)),
                FieldState::List(rows) => {
                    handles.extend(rows.iter().map(|row| row.input.focus_handle(cx)))
                }
                FieldState::Secret(secret) => handles.push(secret.row.input.focus_handle(cx)),
                FieldState::Lines(lines) => handles.push(lines.input.focus_handle(cx)),
                FieldState::Records(cards) => {
                    for card in cards.iter().filter(|card| card.expanded) {
                        handles.extend(
                            card.inputs
                                .iter()
                                .filter_map(|input| input_focus(input, cx)),
                        );
                    }
                }
                FieldState::Plain => {}
            }
        }
        handles
    }

    /// ⇥ / ⇧⇥: the next or the previous field (round the end).
    fn focus_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let handles = self.focusables(cx);
        if handles.is_empty() {
            return;
        }
        let count = handles.len();
        let next = match handles.iter().position(|focus| focus.is_focused(window)) {
            Some(at) if forward => (at + 1) % count,
            Some(at) => (at + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        };
        window.focus(&handles[next]);
    }
}

/// A `records` setting's value as its cards hold it: a list of objects by the fields' keys.
fn records_value(fields: &[RecordField], cards: &[RecordCard], cx: &App) -> Value {
    Value::Array(
        cards
            .iter()
            .map(|card| {
                Value::Object(
                    fields
                        .iter()
                        .zip(&card.inputs)
                        .map(|(field, input)| (field.key.clone(), input_value(input, cx)))
                        .collect(),
                )
            })
            .collect(),
    )
}

fn input_value(input: &RecordInput, cx: &App) -> Value {
    match input {
        RecordInput::Line(row) => Value::String(row.input.read(cx).text()),
        RecordInput::Lines(lines) => Value::String(lines.input.read(cx).text()),
        RecordInput::Switch(on) => Value::Bool(*on),
        RecordInput::Choice(value) => Value::String(value.clone()),
    }
}

/// The keyboard's place in a record's field: its text fields have one.
fn input_focus(input: &RecordInput, cx: &App) -> Option<FocusHandle> {
    match input {
        RecordInput::Line(row) => Some(row.input.focus_handle(cx)),
        RecordInput::Lines(lines) => Some(lines.input.focus_handle(cx)),
        RecordInput::Switch(_) | RecordInput::Choice(_) => None,
    }
}

/// A record's name: its first line field's text; none while it is empty.
fn record_name(fields: &[RecordField], card: &RecordCard, cx: &App) -> Option<String> {
    fields
        .iter()
        .zip(&card.inputs)
        .find(|(field, _)| field.kind == RecordFieldKind::String)
        .and_then(|(_, input)| record_title(&input_value(input, cx)))
}

/// A name from a field's value: its first line, trimmed; none when empty.
fn record_title(value: &Value) -> Option<String> {
    let line = value.as_str()?.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Nothing is typed in the record: its text fields are empty (switches and choices don't count).
fn record_is_empty(card: &RecordCard, cx: &App) -> bool {
    card.inputs.iter().all(|input| match input {
        RecordInput::Line(row) => row.input.read(cx).is_empty(),
        RecordInput::Lines(lines) => lines.input.read(cx).is_empty(),
        RecordInput::Switch(_) | RecordInput::Choice(_) => true,
    })
}

impl Secret {
    /// The keychain has a value or not: an empty field shows dots for a saved one.
    fn mark_saved(&mut self, saved: bool, cx: &mut App) {
        self.saved = Some(saved);
        let placeholder = if saved {
            SharedString::from("••••••••")
        } else {
            tr("Not set").into()
        };
        self.row
            .input
            .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
    }
}

/// A keychain error as a line under a secret's field.
fn keychain_error(error: &str) -> SharedString {
    trf("The keychain refused: {0}", &[&error]).into()
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
        match &spec.kind {
            SettingKind::Section => return self.render_section(index, ui),
            SettingKind::Status => return self.render_status(index, ui, cx),
            _ => {}
        }
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
            (_, FieldState::Secret(secret)) => card
                .child(caption(None))
                .child(self.render_secret(index, secret, ui, cx))
                .children(field.error.clone().map(|error| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.error)
                        .child(error)
                }))
                .into_any_element(),
            (_, FieldState::Lines(lines)) => card
                .child(caption(reset))
                .child(render_lines(lines, window, cx))
                .into_any_element(),
            (_, FieldState::Records(cards)) => {
                let fields: &[RecordField] = match &spec.kind {
                    SettingKind::Records(fields) => fields,
                    _ => &[],
                };
                card.child(caption(reset))
                    .child(self.render_records(index, fields, cards, window, cx))
                    .into_any_element()
            }
            (_, FieldState::Plain) => card.child(caption(reset)).into_any_element(),
        }
    }

    /// The cards of a `records` setting and Add under them.
    fn render_records(
        &self,
        index: usize,
        fields: &[RecordField],
        cards: &[RecordCard],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let ui = Theme::ui(cx);
        let count = cards.len();
        let cards: Vec<AnyElement> = cards
            .iter()
            .enumerate()
            .map(|(place, card)| self.render_record(index, fields, card, place, count, window, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(cards)
            .when(count == 0, |list| {
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
                        move |this, _: &ClickEvent, window, cx| this.add_record(index, window, cx),
                    )),
                ),
            )
    }

    /// A record's card: the header (open / close, the name, what's switched on while it is
    /// closed, Move Up / Move Down / Remove) and, open, its fields.
    #[allow(clippy::too_many_arguments)]
    fn render_record(
        &self,
        index: usize,
        fields: &[RecordField],
        card: &RecordCard,
        place: usize,
        count: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let id = card.id;
        let name = record_name(fields, card, cx);
        let untitled = name.is_none();
        let title = name.unwrap_or_else(|| tr("Untitled").to_string());
        // Closed: what's switched on, and choices other than the first option.
        let summary: Vec<Div> = if card.expanded {
            Vec::new()
        } else {
            fields
                .iter()
                .zip(&card.inputs)
                .filter_map(|(field, input)| match (&field.kind, input) {
                    (RecordFieldKind::Bool, RecordInput::Switch(true)) => {
                        Some(ui::badge(self.tr(&field.title).to_string(), ui.accent_text))
                    }
                    (RecordFieldKind::Choice(options), RecordInput::Choice(value))
                        if options.first().is_some_and(|first| first.value != *value) =>
                    {
                        let option = options.iter().find(|option| option.value == *value)?;
                        Some(ui::badge(self.tr(&option.title).to_string(), ui.text_muted))
                    }
                    _ => None,
                })
                .collect()
        };
        let toggle = div()
            .id(SharedString::from(format!(
                "plugin-record-{index}-{id}-toggle"
            )))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .py_0p5()
            .cursor_pointer()
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_card(index, id, cx)),
            )
            .child(
                crate::icons::icon(
                    if card.expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    ui.text_muted,
                )
                .size(px(12.)),
            )
            .child(
                div()
                    .flex_shrink()
                    .min_w_0()
                    .truncate()
                    .when(untitled, |name| name.text_color(ui.dim))
                    .child(title),
            )
            .children(summary.into_iter().map(|badge| badge.flex_none()));
        let button = |name: &str, icon: IconName, tooltip: &'static str, enabled: bool| {
            ui::icon_button(
                SharedString::from(format!("plugin-record-{index}-{id}-{name}")),
                icon,
                ui,
            )
            .tooltip(ui::tooltip(tr(tooltip), None))
            .when(!enabled, |button| button.opacity(0.35).cursor_default())
        };
        let header = div()
            .flex()
            .items_center()
            .gap_1()
            .pl_2()
            .pr_1()
            .py_1()
            .child(toggle)
            .child(button("up", IconName::ArrowUp, "Move Up", place > 0).when(
                place > 0,
                |button| {
                    button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.move_record(index, id, -1, cx)
                    }))
                },
            ))
            .child(
                button("down", IconName::ArrowDown, "Move Down", place + 1 < count).when(
                    place + 1 < count,
                    |button| {
                        button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.move_record(index, id, 1, cx)
                        }))
                    },
                ),
            )
            .child(
                button("remove", IconName::Trash, "Remove", true).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.remove_record(index, id, window, cx)
                    },
                )),
            );
        div()
            .id(SharedString::from(format!("plugin-record-{index}-{id}")))
            .key_context(RECORD_CONTEXT)
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(header)
            .when(card.expanded, |record| {
                record.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2p5()
                        .px_3()
                        .pt_1()
                        .pb_3()
                        .children(fields.iter().zip(&card.inputs).enumerate().map(
                            |(position, (field, input))| {
                                self.render_record_field(
                                    index, id, position, field, input, window, cx,
                                )
                            },
                        )),
                )
            })
            .into_any_element()
    }

    /// A field of an open record: its label over a text field, or a checkbox, or the options.
    #[allow(clippy::too_many_arguments)]
    fn render_record_field(
        &self,
        index: usize,
        id: u64,
        position: usize,
        field: &RecordField,
        input: &RecordInput,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let label = self.tr(&field.title).to_string();
        let caption = || {
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(label.clone())
        };
        match (input, &field.kind) {
            (RecordInput::Line(row), _) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(caption())
                .child(row.input.clone())
                .into_any_element(),
            (RecordInput::Lines(lines), _) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(caption())
                .child(render_lines(lines, window, cx))
                .into_any_element(),
            (RecordInput::Switch(on), _) => div()
                .id(SharedString::from(format!(
                    "plugin-record-{index}-{id}-{position}"
                )))
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_record_switch(index, id, position, cx)
                }))
                .child(ui::checkbox(
                    SharedString::from(format!("plugin-record-{index}-{id}-{position}-check")),
                    if *on {
                        ui::CheckState::Checked
                    } else {
                        ui::CheckState::Unchecked
                    },
                    ui,
                ))
                .child(label.clone())
                .into_any_element(),
            (RecordInput::Choice(value), RecordFieldKind::Choice(options)) => {
                let options = options.iter().enumerate().map(|(option, choice)| {
                    let chosen = choice.value.clone();
                    div()
                        .id(SharedString::from(format!(
                            "plugin-record-{index}-{id}-{position}-option-{option}"
                        )))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1p5()
                        .py_0p5()
                        .rounded(px(ui::RADIUS_SM))
                        .cursor_pointer()
                        .hover(move |style| style.bg(ui.hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.choose_record_option(index, id, position, chosen.clone(), cx)
                        }))
                        .child(ui::radio(
                            SharedString::from(format!(
                                "plugin-record-{index}-{id}-{position}-radio-{option}"
                            )),
                            choice.value == *value,
                            ui,
                        ))
                        .child(self.tr(&choice.title).to_string())
                });
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(caption())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x_3()
                            .gap_y_1()
                            .children(options),
                    )
                    .into_any_element()
            }
            (RecordInput::Choice(_), _) => div().into_any_element(),
        }
    }

    /// A heading over the settings that follow, with its description.
    fn render_section(&self, index: usize, ui: UiColors) -> AnyElement {
        let spec = &self.fields[index].spec;
        div()
            .flex()
            .flex_col()
            .gap_1()
            // A new group: the design system's 16–24 px between groups (the page's gap included).
            .when(index > 0, |section| section.pt_2())
            .child(ui::section_label(self.tr(&spec.title).to_string(), ui))
            .children(spec.description.as_deref().map(|text| {
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(self.tr(text).to_string())
            }))
            .into_any_element()
    }

    /// The plugin's line: its text in its tone, a pulsing dot while it works; until it writes one,
    /// the manifest's text.
    fn render_status(&self, index: usize, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        self.render_status_with(index, &[], ui, cx)
    }

    /// A status line with the buttons next to it in the manifest (Sign In / Sign Out after the
    /// account's line, Check Connection before the connection's): one card, the buttons on the
    /// right, as JetBrains' settings put an account and its actions on one line.
    fn render_status_with(
        &self,
        index: usize,
        buttons: &[usize],
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spec = &self.fields[index].spec;
        let status = self
            .plugins
            .read(cx)
            .setting_status(&self.plugin, &spec.key)
            .cloned()
            .unwrap_or_else(|| SettingStatus {
                text: spec
                    .default
                    .as_str()
                    .map(|text| self.tr(text).to_string())
                    .unwrap_or_default(),
                tone: Tone::Muted,
                busy: false,
            });
        let title = self.tr(&spec.title).to_string();
        let line = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .text_color(tone_color(status.tone, ui))
            .when(status.busy, |line| line.child(busy_dot(index, ui)))
            .child(div().min_w_0().child(status.text));
        let actions = (!buttons.is_empty()).then(|| self.render_buttons(buttons, false, cx));
        div()
            .id(SharedString::from(format!("plugin-setting-{index}")))
            .px_3()
            .min_h(px(44.))
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .when(!title.is_empty(), |card| {
                card.child(div().flex_none().text_color(ui.text_muted).child(title))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .when(actions.is_none(), |line_box| line_box.justify_end())
                    .child(line),
            )
            .children(actions.map(|actions| div().flex_none().child(actions)))
            .into_any_element()
    }

    /// A secret's masked field: "Saved in the keychain" while it is empty and a value is there;
    /// Clear takes the value away.
    fn render_secret(
        &self,
        index: usize,
        secret: &Secret,
        ui: UiColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let typed = !secret.row.input.read(cx).is_empty();
        let hint = match secret.saved {
            _ if typed => None,
            Some(true) => Some((tr("Saved in the keychain"), true)),
            Some(false) => None,
            None => Some((tr("Reading the keychain…"), false)),
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(div().flex_1().min_w_0().child(secret.row.input.clone()))
                    .when(secret.saved == Some(true) || typed, |row| {
                        row.child(
                            ui::text_button(
                                SharedString::from(format!("plugin-setting-{index}-clear")),
                                tr("Clear"),
                                false,
                                ui,
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| this.clear_secret(index, cx),
                            )),
                        )
                    }),
            )
            .children(hint.map(|(hint, saved)| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .when(saved, |line| {
                        line.child(
                            crate::icons::icon(IconName::CheckCircle, ui.success).size(px(12.)),
                        )
                    })
                    .child(hint)
            }))
    }

    /// Buttons that follow one another share a line: "Sign In…", "Sign Out".
    /// Buttons in a line: on their own (`wrap` — a line that doesn't fit goes on), or in a status
    /// line's card, where they stay on one line at its end.
    fn render_buttons(&self, indices: &[usize], wrap: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Theme::ui(cx);
        let store = self.plugins.read(cx);
        let buttons: Vec<AnyElement> = indices
            .iter()
            .map(|&index| {
                let spec = &self.fields[index].spec;
                let enabled = match &spec.kind {
                    SettingKind::Button { command, .. } => {
                        store.command_enabled(&self.plugin, command)
                    }
                    _ => false,
                };
                ui::text_button(
                    SharedString::from(format!("plugin-setting-{index}")),
                    self.tr(&spec.title).to_string(),
                    false,
                    ui,
                )
                .h(px(28.))
                .px(px(12.))
                .when(!enabled, |button| button.opacity(0.5).cursor_default())
                .when(enabled, |button| {
                    button.on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.press(index, cx)),
                    )
                })
                .when_some(spec.description.as_deref(), |button, text| {
                    button.tooltip(ui::tooltip(self.tr(text).to_string(), None))
                })
                .into_any_element()
            })
            .collect();
        div()
            .flex()
            .when(wrap, |line| line.flex_wrap())
            .items_center()
            .gap_2()
            .children(buttons)
            .into_any_element()
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
        // Buttons that follow one another share a line; next to a status line (just before or
        // just after it) they join its card.
        let is_button = |fields: &[Field], index: usize| {
            matches!(fields[index].spec.kind, SettingKind::Button { .. })
        };
        let is_status = |fields: &[Field], index: usize| {
            fields
                .get(index)
                .is_some_and(|field| field.spec.kind == SettingKind::Status)
        };
        let mut fields: Vec<AnyElement> = Vec::new();
        let mut index = 0;
        while index < self.fields.len() {
            if is_button(&self.fields, index) {
                let start = index;
                while index < self.fields.len() && is_button(&self.fields, index) {
                    index += 1;
                }
                let buttons: Vec<usize> = (start..index).collect();
                if is_status(&self.fields, index) {
                    fields.push(self.render_status_with(index, &buttons, ui, cx));
                    index += 1;
                } else {
                    fields.push(self.render_buttons(&buttons, true, cx));
                }
            } else if is_status(&self.fields, index) {
                let status = index;
                index += 1;
                let start = index;
                while index < self.fields.len() && is_button(&self.fields, index) {
                    index += 1;
                }
                let buttons: Vec<usize> = (start..index).collect();
                fields.push(self.render_status_with(status, &buttons, ui, cx));
            } else {
                fields.push(self.render_field(index, window, cx));
                index += 1;
            }
        }
        let page = cx.entity().downgrade();
        let handle = window.window_handle();
        OPEN_PAGE.with(|open| *open.borrow_mut() = Some((page, handle)));
        div()
            .key_context(PAGE_CONTEXT)
            .on_action(
                cx.listener(|this, _: &NextField, window, cx| this.focus_step(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousField, window, cx| {
                    this.focus_step(false, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &AddRecord, window, cx| this.add_record_action(window, cx)),
            )
            .on_action(cx.listener(|this, _: &RemoveRecord, window, cx| {
                this.remove_record_action(window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveRecordUp, window, cx| {
                this.move_record_action(-1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveRecordDown, window, cx| {
                this.move_record_action(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRecord, window, cx| {
                this.toggle_record_action(window, cx)
            }))
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

/// A multi-line field in a box like a text field's: ↵ — a new line; a click on the box's padding
/// puts the caret into it.
fn render_lines(lines: &Lines, window: &Window, cx: &mut Context<PluginSettingsPage>) -> Div {
    let ui = Theme::ui(cx);
    let focus = lines.input.focus_handle(cx);
    let focused = focus.is_focused(window);
    let input = lines.input.clone();
    div()
        .key_context(LINES_CONTEXT)
        .w_full()
        .px_2p5()
        .py(px(6.))
        .rounded(px(ui::RADIUS_MD))
        .bg(ui.input_background)
        .border_1()
        .border_color(if focused {
            ui.focus_border
        } else {
            ui.input_border
        })
        .when(focused, |field| field.shadow(ui::focus_ring(ui)))
        .when(!focused, |field| {
            field.hover(move |style| style.border_color(ui.elevated_border))
        })
        .cursor(CursorStyle::IBeam)
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, _| {
            window.focus(&focus)
        })
        .on_action(move |_: &NewLine, _, cx| input.update(cx, |input, cx| input.insert("\n", cx)))
        .child(lines.input.clone())
}

/// The color of a status line's tone, as in the plugins' tool windows.
fn tone_color(tone: Tone, ui: UiColors) -> gpui::Hsla {
    match tone {
        Tone::Normal => ui.foreground,
        Tone::Muted => ui.text_muted,
        Tone::Dim => ui.dim,
        Tone::Accent => ui.accent_text,
        Tone::Success => ui.success,
        Tone::Warning => ui.warning,
        Tone::Error => ui.error,
    }
}

/// A pulsing dot: the plugin works ("Checking…").
fn busy_dot(index: usize, ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(8.))
        .rounded(px(4.))
        .bg(ui.accent)
        .with_animation(
            SharedString::from(format!("plugin-setting-busy-{index}")),
            Animation::new(Duration::from_millis(900)).repeat(),
            |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
        )
}

/// `settings.set` (API 0.3): a plugin writes one of its settings — checked against the manifest
/// by `flux-plugin`; none brings back the default. An open page shows it at once (it observes the
/// store); the plugin isn't told `settings-changed` about its own write.
pub(crate) fn set_from_plugin(
    store: &mut PluginStore,
    plugin: &str,
    key: &str,
    value: Option<Value>,
    cx: &mut Context<PluginStore>,
) {
    let _ = store;
    settings::set_plugin_value(plugin, key, value, cx);
    cx.notify();
}

/// `system.open-settings` (API 0.3): Settings on the plugin's own page.
pub(crate) fn open(
    workspace: &mut crate::workspace::Workspace,
    plugin: &str,
    window: &mut Window,
    cx: &mut Context<crate::workspace::Workspace>,
) {
    let section = crate::settings_view::Section::Plugin(plugin.to_string().into());
    crate::settings_view::open(workspace, section, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_named_by_the_first_line_of_its_text() {
        assert_eq!(
            record_title(&Value::String("  Take to Work \nmore".into())),
            Some("Take to Work".to_string())
        );
        assert_eq!(record_title(&Value::String("   ".into())), None);
        assert_eq!(record_title(&Value::String(String::new())), None);
        assert_eq!(record_title(&Value::Bool(true)), None);
    }

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
