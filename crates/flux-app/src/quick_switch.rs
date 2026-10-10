//! Quick Switch (⌃`, stage 8.3), as Quick Switch Scheme in JetBrains IDEs: a popup to switch the
//! theme, the file icons or the interface language without opening Settings; moving through the
//! themes shows each of them right away.
//!
//! The first level lists what can be switched, with the current choice of each; ↵ or → opens one,
//! ← or Esc goes back (a theme being looked at goes back to the chosen one), ↵ on a choice applies it
//! and closes the popup. Digits pick a row, as the numbered rows of JetBrains' popup. The palette
//! offers the levels directly: Select Theme, Select File Icons, Select Language.

use gpui::{
    App, ClickEvent, Context, DismissEvent, Div, EventEmitter, FocusHandle, Focusable, FontWeight,
    KeyBinding, KeyDownEvent, MouseMoveEvent, NoAction, Render, SharedString, Subscription, Window,
    actions, div, prelude::*, px,
};

use crate::appearance_settings::{self, Page};
use crate::i18n::{self, Lang, tr};
use crate::icon_themes::{self, IconThemeInfo};
use crate::icons::{FileIcon, IconName, icon};
use crate::popup;
use crate::settings::{self, LanguageChoice};
use crate::theme::{self, Appearance, Theme, ThemeInfo};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    quick_switch,
    [Toggle, SelectTheme, SelectFileIcons, SelectLanguage]
);

// The popup's own keys; a namespace of their own keeps them out of the palette's names.
actions!(
    quick_switch_popup,
    [SelectNext, SelectPrevious, Confirm, Open, Back, Dismiss]
);

const WIDTH: f32 = 380.;
const ROW_HEIGHT: f32 = 28.;
const PADDING: f32 = 5.;
/// The rows a level shows without scrolling.
const MAX_VISIBLE_ROWS: usize = 12;

pub fn init(cx: &mut App) {
    let popup = Some("QuickSwitch");
    cx.bind_keys([
        KeyBinding::new("ctrl-`", Toggle, Some("Workspace")),
        // In a terminal ⌃` stays the program's (it sends NUL), as ⌃V does.
        KeyBinding::new("ctrl-`", NoAction, Some("Terminal")),
        KeyBinding::new("down", SelectNext, popup),
        KeyBinding::new("up", SelectPrevious, popup),
        KeyBinding::new("ctrl-n", SelectNext, popup),
        KeyBinding::new("ctrl-p", SelectPrevious, popup),
        KeyBinding::new("enter", Confirm, popup),
        KeyBinding::new("right", Open, popup),
        KeyBinding::new("left", Back, popup),
        KeyBinding::new("backspace", Back, popup),
        KeyBinding::new("escape", Dismiss, popup),
    ]);
}

/// The window's handlers of Quick Switch.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|workspace, _: &Toggle, window, cx| toggle(workspace, window, cx)))
        .on_action(cx.listener(|workspace, _: &SelectTheme, window, cx| {
            open(workspace, Level::Theme, window, cx)
        }))
        .on_action(cx.listener(|workspace, _: &SelectFileIcons, window, cx| {
            open(workspace, Level::FileIcons, window, cx)
        }))
        .on_action(cx.listener(|workspace, _: &SelectLanguage, window, cx| {
            open(workspace, Level::Language, window, cx)
        }))
}

/// Opens Quick Switch, or closes it if open.
pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    open(workspace, Level::Root, window, cx);
}

/// Opens Quick Switch on a level (the palette's Select Theme… opens the themes right away).
fn open(workspace: &mut Workspace, level: Level, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.toggle_modal(window, cx, move |window, cx| {
        QuickSwitch::new(level, window, cx)
    });
}

/// What the popup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// What can be switched.
    Root,
    Theme,
    FileIcons,
    Language,
}

/// The levels the first level opens, in its order.
const LEVELS: [Level; 3] = [Level::Theme, Level::FileIcons, Level::Language];

/// The interface languages, in the order of the Language level and of Settings.
const LANGUAGES: [LanguageChoice; 3] = [
    LanguageChoice::System,
    LanguageChoice::English,
    LanguageChoice::Russian,
];

/// How a theme picked in Quick Switch is applied. With Sync with OS on, a theme of the system's
/// current appearance becomes the theme of that appearance; a theme of the other one means the
/// person wants it now: following macOS stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    Select,
    SelectFor(Appearance),
    StopSyncAndSelect,
}

fn theme_choice(sync: bool, system: Appearance, picked: Appearance) -> ThemeChoice {
    match (sync, picked == system) {
        (false, _) => ThemeChoice::Select,
        (true, true) => ThemeChoice::SelectFor(picked),
        (true, false) => ThemeChoice::StopSyncAndSelect,
    }
}

/// The row a digit picks (`1` — the first): numbers as JetBrains' popups show them.
fn digit_row(key: &str, count: usize) -> Option<usize> {
    let digit: usize = key.parse().ok()?;
    (1..=count.min(9)).contains(&digit).then(|| digit - 1)
}

/// A row of a level.
struct Row {
    label: SharedString,
    /// A quiet text on the right: the current choice of a level, the system's language.
    detail: Option<SharedString>,
    icon: Option<IconName>,
    /// A few icons of an icon set.
    samples: Vec<FileIcon>,
    /// The current choice.
    checked: bool,
}

pub struct QuickSwitch {
    focus_handle: FocusHandle,
    level: Level,
    selected: usize,
    /// The row of the first level a level was opened from: going back selects it again.
    root_selected: usize,
    /// The choices of the levels, read when the popup opens.
    themes: Vec<ThemeInfo>,
    icon_sets: Vec<IconThemeInfo>,
    /// The theme shown when the theme level opened: the one checked while others are looked at.
    current_theme: SharedString,
    /// Closing the popup any way (Esc, a click outside) shows the chosen theme again.
    _release: Subscription,
}

impl EventEmitter<DismissEvent> for QuickSwitch {}

impl Focusable for QuickSwitch {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl QuickSwitch {
    fn new(level: Level, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let release = cx.on_release(|_, cx| theme::preview(None, cx));
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            level: Level::Root,
            selected: 0,
            root_selected: 0,
            themes: theme::available(cx),
            icon_sets: icon_themes::available(),
            current_theme: Theme::get(cx).name.clone(),
            _release: release,
        };
        if level != Level::Root {
            this.root_selected = LEVELS.iter().position(|l| *l == level).unwrap_or(0);
            this.enter(level, cx);
        }
        this
    }

    /// Shows a level with its current choice selected.
    fn enter(&mut self, level: Level, cx: &mut Context<Self>) {
        self.level = level;
        self.selected = match level {
            Level::Root => self.root_selected,
            Level::Theme => {
                self.current_theme = Theme::get(cx).name.clone();
                self.themes
                    .iter()
                    .position(|theme| theme.name == self.current_theme)
                    .unwrap_or(0)
            }
            Level::FileIcons => {
                let active = icon_themes::active();
                self.icon_sets
                    .iter()
                    .position(|set| Some(&set.name) == active.as_ref())
                    .unwrap_or(0)
            }
            Level::Language => {
                let current = settings::appearance(cx).language;
                LANGUAGES
                    .iter()
                    .position(|choice| *choice == current)
                    .unwrap_or(0)
            }
        };
        cx.notify();
    }

    fn row_count(&self) -> usize {
        match self.level {
            Level::Root => LEVELS.len(),
            Level::Theme => self.themes.len(),
            Level::FileIcons => self.icon_sets.len(),
            Level::Language => LANGUAGES.len(),
        }
    }

    /// Selects a row; on the theme level the window shows its theme right away.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        self.selected = index.min(count - 1);
        if self.level == Level::Theme
            && let Some(theme) = self.themes.get(self.selected)
        {
            theme::preview(Some(&theme.name), cx);
        }
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.row_count();
        if count > 0 {
            self.select((self.selected + 1) % count, cx);
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.row_count();
        if count > 0 {
            self.select((self.selected + count - 1) % count, cx);
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.choose(self.selected, window, cx);
    }

    /// → opens the selected level; on a level of choices it does nothing.
    fn open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        if self.level == Level::Root {
            self.choose(self.selected, window, cx);
        }
    }

    /// ← and Backspace go back to the first level; on it they do nothing.
    fn back(&mut self, _: &Back, _: &mut Window, cx: &mut Context<Self>) {
        if self.level != Level::Root {
            self.go_back(cx);
        }
    }

    /// Esc goes back from a level, and closes the popup on the first one.
    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.level == Level::Root {
            cx.emit(DismissEvent);
        } else {
            self.go_back(cx);
        }
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.level == Level::Theme {
            theme::preview(None, cx);
        }
        self.enter(Level::Root, cx);
    }

    /// Applies row `index`: opens a level, or applies a choice and closes the popup.
    fn choose(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        match self.level {
            Level::Root => {
                if let Some(level) = LEVELS.get(index) {
                    self.root_selected = index;
                    self.enter(*level, cx);
                }
            }
            Level::Theme => {
                let Some(picked) = self.themes.get(index).cloned() else {
                    return;
                };
                let sync = settings::appearance(cx).sync_with_os;
                let system = theme::system_appearance(cx);
                match theme_choice(sync, system, picked.appearance) {
                    ThemeChoice::Select => theme::select(&picked.name, cx),
                    ThemeChoice::SelectFor(appearance) => {
                        theme::select_for(appearance, &picked.name, cx)
                    }
                    ThemeChoice::StopSyncAndSelect => {
                        theme::set_sync_with_os(false, cx);
                        theme::select(&picked.name, cx);
                    }
                }
                cx.emit(DismissEvent);
            }
            Level::FileIcons => {
                if let Some(set) = self.icon_sets.get(index) {
                    icon_themes::select(&set.name, cx);
                    cx.emit(DismissEvent);
                }
            }
            Level::Language => {
                if let Some(choice) = LANGUAGES.get(index) {
                    appearance_settings::set_language(*choice, cx);
                    cx.emit(DismissEvent);
                }
            }
        }
    }

    /// Digits pick a row, without modifiers.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        if let Some(index) = digit_row(&keystroke.key, self.row_count()) {
            cx.stop_propagation();
            self.select(index, cx);
            self.choose(index, window, cx);
        }
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let row = |label: SharedString| Row {
            label,
            detail: None,
            icon: None,
            samples: Vec::new(),
            checked: false,
        };
        match self.level {
            Level::Root => {
                let language = match settings::appearance(cx).language {
                    LanguageChoice::System => SharedString::from(tr("System")),
                    LanguageChoice::English => i18n::lang_name(Lang::En).into(),
                    LanguageChoice::Russian => i18n::lang_name(Lang::Ru).into(),
                };
                let details = [
                    Some(Theme::get(cx).name.clone()),
                    icon_themes::active(),
                    Some(language),
                ];
                LEVELS
                    .iter()
                    .zip(details)
                    .map(|(level, detail)| Row {
                        detail,
                        icon: Some(match level {
                            Level::Theme => IconName::Palette,
                            Level::FileIcons => IconName::File,
                            _ => IconName::Globe,
                        }),
                        ..row(page_of(*level).title().into())
                    })
                    .collect()
            }
            Level::Theme => self
                .themes
                .iter()
                .map(|theme| Row {
                    icon: Some(match theme.appearance {
                        Appearance::Dark => IconName::Moon,
                        Appearance::Light => IconName::Sun,
                    }),
                    checked: theme.name == self.current_theme,
                    ..row(theme.name.clone())
                })
                .collect(),
            Level::FileIcons => {
                let ui = Theme::ui(cx);
                let active = icon_themes::active();
                self.icon_sets
                    .iter()
                    .map(|set| Row {
                        samples: appearance_settings::sample_icons(&set.name, &ui)
                            .into_iter()
                            .take(4)
                            .collect(),
                        checked: Some(&set.name) == active.as_ref(),
                        ..row(set.name.clone())
                    })
                    .collect()
            }
            Level::Language => {
                let current = settings::appearance(cx).language;
                LANGUAGES
                    .iter()
                    .map(|choice| match choice {
                        LanguageChoice::System => Row {
                            detail: Some(
                                i18n::lang_name(i18n::system_lang()).into(),
                            ),
                            checked: current == *choice,
                            ..row(tr("System").into())
                        },
                        LanguageChoice::English => Row {
                            checked: current == *choice,
                            ..row(i18n::lang_name(Lang::En).into())
                        },
                        LanguageChoice::Russian => Row {
                            checked: current == *choice,
                            ..row(i18n::lang_name(Lang::Ru).into())
                        },
                    })
                    .collect()
            }
        }
    }
}

/// The page of Settings → Appearance a level switches: its title is the level's.
fn page_of(level: Level) -> Page {
    match level {
        Level::FileIcons => Page::FileIcons,
        Level::Language => Page::Language,
        _ => Page::Theme,
    }
}

impl Render for QuickSwitch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let rows = self.rows(cx);
        let root = self.level == Level::Root;
        let title: SharedString = if root {
            tr("Quick Switch").into()
        } else {
            page_of(self.level).title().into()
        };
        let items = rows.into_iter().enumerate().map(|(index, row)| {
            let selected = index == self.selected;
            div()
                .id(("quick-switch-row", index))
                .h(px(ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS_SM))
                .whitespace_nowrap()
                .cursor_pointer()
                .when(selected, |item| item.bg(ui.list_selected))
                .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                    if this.selected != index {
                        this.select(index, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.selected = index;
                    this.choose(index, window, cx);
                }))
                // The number a digit picks, as JetBrains' popups number their rows.
                .child(
                    div()
                        .flex_none()
                        .w(px(12.))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .when(index < 9, |number| number.child((index + 1).to_string())),
                )
                .children(
                    row.icon
                        .map(|name| icon(name, ui.text_muted).flex_none().size(px(14.))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(ui.foreground)
                        .child(row.label),
                )
                .children(row.samples.iter().map(|sample| {
                    sample.render().size(px(14.))
                }))
                .children(row.detail.map(|detail| {
                    div()
                        .flex_none()
                        .max_w(px(150.))
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(detail)
                }))
                .child(div().flex_none().w(px(14.)).map(|mark| {
                    if root {
                        mark.child(icon(IconName::ChevronRight, ui.dim).size(px(12.)))
                    } else if row.checked {
                        mark.child(icon(IconName::Check, ui.accent_text).size(px(14.)))
                    } else {
                        mark
                    }
                }))
        });
        let empty = (self.row_count() == 0).then(|| {
            div()
                .h(px(ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .text_color(ui.dim)
                .child(tr("No sets of icons: their plugins are turned off"))
        });
        // On a level ← takes the place of ↑↓ (the lists are short): the hints fit in Russian too.
        let mut hints = if root {
            vec![("↑↓", tr("navigate")), ("↵", tr("choose"))]
        } else {
            vec![("↵", tr("choose")), ("←", tr("back"))]
        };
        hints.push(("esc", tr("close")));
        popup::panel(ui)
            .key_context("QuickSwitch")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::dismiss))
            .on_key_down(cx.listener(Self::key_down))
            .w(px(WIDTH))
            .p(px(PADDING))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .when(!root, |header| {
                        header.child(
                            div()
                                .id("quick-switch-back")
                                .flex_none()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.go_back(cx)
                                }))
                                .child(icon(IconName::ArrowLeft, ui.text_muted).size(px(13.))),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui.text_muted)
                            .child(title),
                    ),
            )
            .child(ui::divider(ui).mx_1().mb_1())
            .child(
                div()
                    .id("quick-switch-rows")
                    .max_h(px(ROW_HEIGHT * MAX_VISIBLE_ROWS as f32))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(items)
                    .children(empty),
            )
            .child(ui::divider(ui).mx_1().mt_1())
            .child(
                div()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .child(ui::hint_bar(&hints, ui)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_theme_follows_macos_only_when_it_fits_its_appearance() {
        use Appearance::{Dark, Light};
        assert_eq!(theme_choice(false, Dark, Light), ThemeChoice::Select);
        assert_eq!(theme_choice(false, Light, Light), ThemeChoice::Select);
        assert_eq!(theme_choice(true, Dark, Dark), ThemeChoice::SelectFor(Dark));
        assert_eq!(theme_choice(true, Light, Light), ThemeChoice::SelectFor(Light));
        // A light theme while macOS is dark: the person wants it now.
        assert_eq!(theme_choice(true, Dark, Light), ThemeChoice::StopSyncAndSelect);
    }

    #[test]
    fn digits_pick_rows() {
        assert_eq!(digit_row("1", 3), Some(0));
        assert_eq!(digit_row("3", 3), Some(2));
        assert_eq!(digit_row("4", 3), None);
        assert_eq!(digit_row("0", 3), None);
        assert_eq!(digit_row("9", 12), Some(8));
        assert_eq!(digit_row("a", 3), None);
        assert_eq!(digit_row("1", 0), None);
    }

    #[test]
    fn levels_and_languages_are_in_settings_order() {
        assert_eq!(LEVELS, [Level::Theme, Level::FileIcons, Level::Language]);
        assert_eq!(page_of(Level::FileIcons), Page::FileIcons);
        assert_eq!(page_of(Level::Language), Page::Language);
        assert_eq!(page_of(Level::Theme), Page::Theme);
        assert_eq!(
            LANGUAGES,
            [
                LanguageChoice::System,
                LanguageChoice::English,
                LanguageChoice::Russian
            ]
        );
    }
}
