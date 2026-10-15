//! Settings → Appearance (stage 8.3): the pages Theme (the themes of the plugins, Sync with OS),
//! Interface (the look of the window and its scale, part 10.4), File Icons (the sets of the
//! plugins) and Language (System, English, Русский). Themes and icon sets of plugins are choices on
//! these pages, not pages of their own.
//!
//! - **Theme**: a card per theme, drawn in that theme's own colors (a small window with the tree and
//!   a few lines of code), grouped into dark and light ones. A click shows the theme at once. With
//!   Sync with OS on, each group chooses the theme for its macOS appearance (as the preferred light
//!   and dark themes of JetBrains IDEs).
//! - **Interface**: the look of the window — Islands or Flat, a card each with a small window drawn
//!   that way in the theme's colors, a click applies it to every window ([`ui::set_style`]) — and
//!   the scale of everything in it, as Zoom IDE of JetBrains IDEs ([`ui_scale`]).
//! - **File Icons**: a row per set with a few of its icons; a click puts it in use.
//! - **Language**: the interface language, at once ([`set_language`]).

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FocusHandle, Focusable, FontWeight, Hsla,
    IntoElement, Render, SharedString, Stateful, Subscription, Window, div, prelude::*, px,
};

use crate::contributions::Contributions;
use crate::i18n::{self, Lang, tr, trf};
use crate::icon_themes::{self, IconThemeInfo};
use crate::icons::{FileIcon, IconName, icon};
use crate::plugins::PluginStatus;
use crate::settings::{self, LanguageChoice, UiStyle};
use crate::settings_view::{self, Section};
use crate::theme::{self, Appearance, FLUX_DAY, FLUX_NIGHT, Theme, ThemeInfo, UiColors};
use crate::ui;
use crate::ui_scale;
use crate::workspace::Workspace;

/// A theme card: the preview window and the name under it.
const CARD_WIDTH: f32 = 200.;
const PREVIEW_HEIGHT: f32 = 84.;
/// The code of the preview: small, in the code font.
const PREVIEW_CODE_SIZE: f32 = 9.;
const PREVIEW_LINE_HEIGHT: f32 = 12.;

/// A style card: the window drawn small, its name and what it is.
const STYLE_CARD_WIDTH: f32 = 256.;
const STYLE_PREVIEW_HEIGHT: f32 = 120.;

/// A page of Appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Theme,
    /// The look of the window and its scale (part 10.4).
    Interface,
    FileIcons,
    Language,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Page::Theme => tr("Theme"),
            Page::Interface => tr("Interface"),
            Page::FileIcons => tr("File Icons"),
            Page::Language => tr("Language"),
        }
    }
}

/// The looks of the window, in the order of the page and of Quick Switch.
pub const STYLES: [UiStyle; 2] = [UiStyle::Islands, UiStyle::Flat];

/// A look's name.
pub fn style_name(style: UiStyle) -> &'static str {
    match style {
        UiStyle::Islands => tr("Islands"),
        UiStyle::Flat => tr("Flat"),
    }
}

/// The step of [`ui_scale::STEPS`] a scale is, if it is one (a hand-edited file may hold another).
fn scale_step(scale: f32) -> Option<usize> {
    ui_scale::STEPS
        .iter()
        .position(|step| (step - scale).abs() < 0.001)
}

/// The page shown in Settings.
pub struct AppearancePage {
    page: Page,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl AppearancePage {
    pub fn new(page: Page, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A plugin with themes or icons turned on, off, installed: the lists change.
        let subscriptions = vec![cx.observe_global::<Contributions>(|_, cx| cx.notify())];
        Self {
            page,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Shows another page of Appearance.
    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            cx.notify();
        }
    }

    // --- Theme ---

    fn render_theme(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let appearance = settings::appearance(cx);
        let sync = appearance.sync_with_os;
        let themes = theme::available(cx);
        let shown = Theme::get(cx).name.clone();
        // With Sync with OS, each group marks its own theme; a theme that is gone falls back to
        // the default of its appearance, as `theme::chosen` does.
        let preferred = |appearance_of: Appearance, setting: Option<&str>, default: &str| {
            let name = setting.unwrap_or(default);
            let known = themes
                .iter()
                .any(|theme| theme.name.as_ref() == name && theme.appearance == appearance_of);
            SharedString::from(if known { name } else { default }.to_string())
        };
        let dark_choice = preferred(
            Appearance::Dark,
            appearance.dark_theme.as_deref(),
            FLUX_NIGHT,
        );
        let light_choice = preferred(
            Appearance::Light,
            appearance.light_theme.as_deref(),
            FLUX_DAY,
        );
        let system = theme::system_appearance(cx);
        let groups = [Appearance::Dark, Appearance::Light].map(|group| {
            let members: Vec<(usize, &ThemeInfo)> = themes
                .iter()
                .enumerate()
                .filter(|(_, info)| info.appearance == group)
                .collect();
            let marked = match (sync, group) {
                (false, _) => shown.clone(),
                (true, Appearance::Dark) => dark_choice.clone(),
                (true, Appearance::Light) => light_choice.clone(),
            };
            let (label, caption) = match group {
                Appearance::Dark => (tr("Dark themes"), tr("While macOS is dark")),
                Appearance::Light => (tr("Light themes"), tr("While macOS is light")),
            };
            let cards: Vec<AnyElement> = members
                .into_iter()
                .map(|(index, info)| {
                    let selected = info.name == marked;
                    self.theme_card(index, info, selected, sync, window, cx)
                        .into_any_element()
                })
                .collect();
            div()
                .max_w_full()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .child(ui::section_label(label, ui))
                        .when(sync, |header| {
                            header.child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(if system == group {
                                        ui.accent_text
                                    } else {
                                        ui.dim
                                    })
                                    .child(caption),
                            )
                        }),
                )
                .child(div().flex().flex_wrap().gap_3().children(cards))
        });
        let sync_detail = match (sync, system) {
            (false, _) => tr("Switch between a light and a dark theme together with macOS."),
            (true, Appearance::Dark) => tr("macOS is dark now: Flux shows the dark theme."),
            (true, Appearance::Light) => tr("macOS is light now: Flux shows the light theme."),
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header(
                tr("Theme"),
                tr(
                    "The colors of the window and of the code. Themes come with plugins: Flux has Flux Night and Flux Day, more are in the Marketplace.",
                ),
                ui,
            ))
            .child(
                switch_row(
                    "sync-with-os",
                    tr("Sync with OS"),
                    sync_detail,
                    sync,
                    ui,
                )
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    theme::set_sync_with_os(!sync, cx);
                    cx.notify();
                })),
            )
            // The dark and the light group side by side while they fit, one under the other
            // when they grow.
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x_6()
                    .gap_y_4()
                    .children(groups),
            )
            .child(marketplace_link(
                "more-themes",
                tr("Get more themes in the Marketplace"),
                ui,
            ))
    }

    /// A theme's card: a small window in the theme's colors, its name, where it comes from.
    fn theme_card(
        &self,
        index: usize,
        info: &ThemeInfo,
        selected: bool,
        sync: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let preview = theme::by_name(&info.name, cx)
            .unwrap_or_else(|| Theme::base(info.appearance));
        // Where it comes from: its plugin (unless named as the theme), or Flux itself.
        let source: Option<SharedString> = match &info.plugin {
            Some(plugin) => {
                let name = plugin_name(plugin, window, cx).unwrap_or_else(|| plugin.to_string());
                (name != info.name.as_ref()).then(|| name.into())
            }
            None => Some(tr("Built into Flux").into()),
        };
        let name = info.name.clone();
        let appearance = info.appearance;
        div()
            .id(("theme-card", index))
            .flex_none()
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .gap_2()
            .p_1p5()
            .rounded(px(ui::RADIUS_LG))
            .border_2()
            .border_color(if selected { ui.accent } else { ui.island_border })
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                if sync {
                    theme::select_for(appearance, &name, cx);
                } else {
                    theme::select(&name, cx);
                }
                cx.notify();
            }))
            .child(theme_preview(&preview))
            .child(
                div()
                    .px_1()
                    .pb_0p5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .child(info.name.clone()),
                            )
                            .children(source.map(|source| {
                                div()
                                    .truncate()
                                    .text_size(px(theme::TEXT_XS))
                                    .text_color(ui.dim)
                                    .child(source)
                            })),
                    )
                    .child(
                        icon(
                            match info.appearance {
                                Appearance::Dark => IconName::Moon,
                                Appearance::Light => IconName::Sun,
                            },
                            ui.dim,
                        )
                        .size(px(13.)),
                    )
                    .when(selected, |row| {
                        row.child(icon(IconName::Check, ui.accent_text).size(px(14.)))
                    }),
            )
    }

    // --- Interface (part 10.4) ---

    fn render_interface(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let current = ui::style(cx);
        // The window drawn small in the theme's own colors, glass included: the global theme's
        // surfaces are already opaque while Flat is on.
        let shown = Theme::get(cx);
        let theme = theme::by_name(&shown.name, cx).unwrap_or_else(|| shown.clone());
        let cards = STYLES.into_iter().enumerate().map(|(index, style)| {
            let selected = style == current;
            div()
                .id(("style-card", index))
                .flex_none()
                .w(px(STYLE_CARD_WIDTH))
                .flex()
                .flex_col()
                .gap_2()
                .p_1p5()
                .rounded(px(ui::RADIUS_LG))
                .border_2()
                .border_color(if selected { ui.accent } else { ui.island_border })
                .cursor_pointer()
                .hover(move |card| card.bg(ui.hover))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    ui::set_style(style, cx);
                    cx.notify();
                }))
                .child(style_preview(&theme, style))
                .child(
                    div()
                        .px_1()
                        .pb_0p5()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(ui.foreground)
                                        .child(style_name(style)),
                                )
                                .when(selected, |row| {
                                    row.child(icon(IconName::Check, ui.accent_text).size(px(14.)))
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(ui.dim)
                                .child(match style {
                                    UiStyle::Islands => tr("Rounded panels on the window’s glass"),
                                    UiStyle::Flat => tr("Panels edge to edge, divided by lines"),
                                }),
                        ),
                )
        });
        let scale = settings::ui_scale(cx);
        let step = scale_step(scale);
        let steps = ui_scale::STEPS.iter().enumerate().map(|(index, value)| {
            let value = *value;
            let selected = step == Some(index);
            div()
                .id(("scale-step", index))
                .flex_none()
                .min_w(px(52.))
                .h(px(28.))
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .text_color(if selected { ui.accent_text } else { ui.text_muted })
                .when(selected, |chip| {
                    chip.bg(ui.list_selected).font_weight(FontWeight::SEMIBOLD)
                })
                .when(!selected, |chip| {
                    chip.hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    ui_scale::set(value, cx);
                    cx.notify();
                }))
                .child(ui_scale::label(value))
        });
        // The keys of Zoom In / Zoom Out / Reset, as their commands have them.
        let hints: Vec<(SharedString, &'static str)> = [
            (
                ui::shortcut_for(&ui_scale::ZoomIn, cx),
                tr("Zoom In"),
            ),
            (
                ui::shortcut_for(&ui_scale::ZoomOut, cx),
                tr("Zoom Out"),
            ),
            (ui::shortcut_for(&ui_scale::ResetZoom, cx), tr("Reset")),
        ]
        .into_iter()
        .filter_map(|(keys, label)| Some((keys?, label)))
        .collect();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header(
                tr("Interface"),
                tr("The look of the window and its scale. Both change at once, in every window."),
                ui,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(ui::section_label(tr("Style"), ui))
                    .child(div().flex().flex_wrap().gap_3().children(cards)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(ui::section_label(tr("Scale"), ui))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(ui_scale::label(scale)),
                            ),
                    )
                    .child(div().text_color(ui.text_muted).child(tr(
                        "Everything in the window — panels, icons, the code — larger or smaller.",
                    )))
                    // The steps as one segmented control, as wide as they are (they wrap in a
                    // narrow window: at 200 % the page has half the room).
                    .child(
                        div().flex().child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .p_1()
                                .rounded(px(ui::RADIUS_MD))
                                .border_1()
                                .border_color(ui.island_border)
                                .children(steps),
                        ),
                    )
                    .when(!hints.is_empty(), |section| {
                        section.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_x_4()
                                .gap_y_1()
                                .text_size(px(theme::TEXT_SM))
                                .children(hints.into_iter().map(|(keys, label)| {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(ui::keys(&keys, ui))
                                        .child(div().text_color(ui.dim).child(label))
                                })),
                        )
                    }),
            )
    }

    // --- File Icons ---

    fn render_file_icons(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let sets = icon_themes::available();
        let active = icon_themes::active();
        let rows: Vec<AnyElement> = sets
            .iter()
            .enumerate()
            .map(|(index, set)| {
                let selected = active.as_ref() == Some(&set.name);
                self.icon_set_row(index, set, selected, window, cx)
                    .into_any_element()
            })
            .collect();
        let list = if rows.is_empty() {
            div()
                .p_3()
                .rounded(px(ui::RADIUS_MD))
                .border_1()
                .border_color(ui.island_border)
                .text_color(ui.text_muted)
                .child(tr(
                    "No sets of icons: the plugins that bring them are turned off. Files show plain icons.",
                ))
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_1()
                .rounded(px(ui::RADIUS_MD))
                .border_1()
                .border_color(ui.island_border)
                .children(rows)
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header(
                tr("File Icons"),
                tr(
                    "The icons of files and folders in the project tree, tabs and lists. Sets of icons come with plugins.",
                ),
                ui,
            ))
            .child(list)
            .child(marketplace_link(
                "more-icon-sets",
                tr("Get more icon sets in the Marketplace"),
                ui,
            ))
    }

    fn icon_set_row(
        &self,
        index: usize,
        set: &IconThemeInfo,
        selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        // The plugin that brings it, unless it is named as the set.
        let source = plugin_name(&set.plugin, window, cx)
            .unwrap_or_else(|| set.plugin.to_string())
            .to_string();
        let source = (source != set.name.as_ref()).then_some(source);
        let samples = sample_icons(&set.name, &ui);
        let name = set.name.clone();
        div()
            .id(("icon-set", index))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(ui::RADIUS_MD))
            .cursor_pointer()
            .hover(move |style| style.bg(ui.hover))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                icon_themes::select(&name, cx);
                cx.notify();
            }))
            .child(ui::radio(("icon-set-radio", index), selected, ui))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().truncate().child(set.name.clone()))
                    .children(source.map(|source| {
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(source)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(samples.iter().map(FileIcon::render)),
            )
    }

    // --- Language ---

    fn render_language(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let current = settings::appearance(cx).language;
        let system = i18n::lang_name(i18n::system_lang());
        let choices = [
            (
                LanguageChoice::System,
                SharedString::from(tr("System")),
                Some(trf("Now: {0}", &[&system])),
            ),
            (
                LanguageChoice::English,
                i18n::lang_name(Lang::En).into(),
                None,
            ),
            (
                LanguageChoice::Russian,
                i18n::lang_name(Lang::Ru).into(),
                None,
            ),
        ];
        let rows = choices
            .into_iter()
            .enumerate()
            .map(|(index, (choice, title, detail))| {
                let selected = choice == current;
                div()
                    .id(("language-choice", index))
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(ui::RADIUS_MD))
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        set_language(choice, cx);
                        cx.notify();
                    }))
                    .child(div().pt(px(2.)).child(ui::radio(
                        ("language-radio", index),
                        selected,
                        ui,
                    )))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(title)
                            .children(detail.map(|detail| {
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(detail)
                            })),
                    )
            });
        let pinned = i18n::override_lang().map(|(value, _)| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(px(ui::RADIUS_MD))
                .bg(UiColors::tint(ui.warning, 0.12))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.foreground)
                .child(icon(IconName::Warning, ui.warning).size(px(14.)))
                .child(div().flex_1().min_w_0().child(trf(
                    "FLUX_LANG={0} sets the language of this run; the choice applies without it.",
                    &[&value],
                )))
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header(
                tr("Language"),
                tr(
                    "The language of menus, windows and messages. It changes at once; what is already shown, such as notifications, stays as it was.",
                ),
                ui,
            ))
            .children(pinned)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_1()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .children(rows),
            )
    }
}

impl Focusable for AppearancePage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppearancePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.page {
            Page::Theme => self.render_theme(window, cx),
            Page::Interface => self.render_interface(cx),
            Page::FileIcons => self.render_file_icons(window, cx),
            Page::Language => self.render_language(cx),
        }
    }
}

/// Chooses the interface language (the Language page, Quick Switch): saved, applied at once — the
/// windows redraw in it and the menu bar is built anew. A running plugin with code got the language
/// when it started and translates its own strings with it: it starts again in the new one (its
/// tool windows stay), as JetBrains IDEs restart for a new language.
pub fn set_language(choice: LanguageChoice, cx: &mut App) {
    let before = i18n::lang();
    settings::update_appearance(cx, |appearance| appearance.language = choice);
    i18n::apply_choice(choice);
    crate::app_menu::rebuild(cx);
    cx.refresh_windows();
    if i18n::lang() != before {
        // After the event that chose it: the windows are free to update then.
        cx.defer(restart_plugins_with_code);
    }
}

/// Starts the running plugins with code of every window again.
fn restart_plugins_with_code(cx: &mut App) {
    for window in cx.windows() {
        let Some(workspace) = window.downcast::<Workspace>() else {
            continue;
        };
        let _ = workspace.update(cx, |workspace, _, cx| {
            workspace.plugins.update(cx, |store, cx| {
                let running: Vec<String> = store
                    .plugins()
                    .iter()
                    .filter(|plugin| {
                        matches!(plugin.status, PluginStatus::Running)
                            && plugin.entry.manifest.wasm.is_some()
                    })
                    .map(|plugin| plugin.id().to_string())
                    .collect();
                for id in running {
                    store.restart(&id, cx);
                }
            })
        });
    }
}

/// A few icons of a set (files, then a folder), drawn as the set draws them, without putting it in
/// use.
pub(crate) fn sample_icons(name: &str, ui: &UiColors) -> Vec<FileIcon> {
    icon_themes::sample_icons(Some(name), ui)
}

/// The plugin's name in the interface language, from the window's plugins.
fn plugin_name(id: &str, window: &Window, cx: &App) -> Option<String> {
    let workspace = window.root::<Workspace>().flatten()?;
    let plugins = workspace.read(cx).plugins.read(cx);
    plugins.plugin(id).map(|plugin| plugin.name().to_string())
}

/// The page's title and what it is about, as the other pages of Settings have.
fn header(title: &'static str, description: &'static str, ui: UiColors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TEXT_LG))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_color(ui.text_muted).child(description))
}

/// A row with a switch, as Do Not Disturb in Settings → Notifications.
fn switch_row(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    ui: UiColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(px(ui::RADIUS_MD))
        .border_1()
        .border_color(ui.island_border)
        .cursor_pointer()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(title)
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(detail),
                ),
        )
        .child(ui::switch(SharedString::from(format!("{id}-switch")), on, ui))
}

/// A link to Settings → Plugins, which opens on the Marketplace.
fn marketplace_link(id: &'static str, label: &'static str, ui: UiColors) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .text_color(ui.accent_text)
        .cursor_pointer()
        .hover(|style| style.underline())
        .on_click(|_, window, cx| open_plugins(window, cx))
        .child(icon(IconName::Puzzle, ui.accent_text).size(px(14.)))
        .child(label)
}

/// Settings open on Plugins (the Marketplace is its first tab) instead of Appearance.
fn open_plugins(window: &mut Window, cx: &mut App) {
    let Some(workspace) = window.root::<Workspace>().flatten() else {
        return;
    };
    window.defer(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            settings_view::open(workspace, Section::Plugins, window, cx)
        });
    });
}

/// The code a theme card shows, line by line: (highlight scope, text); no scope — plain text.
const SAMPLE_CODE: &[&[(Option<&str>, &str)]] = &[
    &[
        (Some("keyword"), "fn "),
        (Some("function"), "greet"),
        (Some("punctuation.bracket"), "("),
        (Some("variable.parameter"), "n"),
        (None, ": "),
        (Some("type"), "&str"),
        (Some("punctuation.bracket"), ") {"),
    ],
    &[
        (None, "  "),
        (Some("keyword"), "let "),
        (None, "msg = "),
        (Some("string"), "\"Hi\""),
        (None, ";"),
    ],
    &[(None, "  "), (Some("comment"), "// say hello")],
    &[
        (None, "  "),
        (Some("function"), "print"),
        (None, "(msg, "),
        (Some("number"), "42"),
        (None, ");"),
    ],
    &[(Some("punctuation.bracket"), "}")],
];

/// The second line is the current one: its background, as the editor draws the cursor's line.
const SAMPLE_CURRENT_LINE: usize = 1;

/// A color without the glass's transparency: the preview sits on another theme's surface, so it
/// is drawn opaque.
fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1., ..color }
}

/// A small window in `theme`'s colors: the frame, the tree's island with a few rows (one selected,
/// two with Git colors), and the editor's island with a few lines of code.
fn theme_preview(theme: &Theme) -> Div {
    let ui = theme.ui;
    let bar = |width: f32, color: Hsla| {
        div()
            .h(px(4.))
            .w(gpui::relative(width))
            .rounded(px(2.))
            .bg(UiColors::tint(color, 0.7))
    };
    let tree = div()
        .flex_none()
        .w(px(46.))
        .h_full()
        .p(px(5.))
        .flex()
        .flex_col()
        .gap(px(5.))
        .rounded(px(5.))
        .bg(opaque(ui.island))
        .border_1()
        .border_color(ui.island_border)
        .child(bar(0.7, ui.text_muted))
        .child(
            div()
                .mx(px(-3.))
                .px(px(3.))
                .py(px(2.))
                .rounded(px(3.))
                .bg(ui.list_selected)
                .child(bar(0.8, ui.foreground)),
        )
        .child(bar(0.55, ui.vcs_modified))
        .child(bar(0.75, ui.vcs_added))
        .child(bar(0.5, ui.text_muted));
    let lines = SAMPLE_CODE.iter().enumerate().map(|(index, line)| {
        let pieces = line.iter().map(|(scope, text)| {
            let style = scope.and_then(|scope| theme.style_for(scope));
            div()
                .flex_none()
                .text_color(style.map_or(ui.foreground, |style| style.color))
                .when(style.is_some_and(|style| style.bold), |piece| {
                    piece.font_weight(FontWeight::BOLD)
                })
                .when(style.is_some_and(|style| style.italic), |piece| piece.italic())
                .child(*text)
        });
        div()
            .h(px(PREVIEW_LINE_HEIGHT))
            .px(px(4.))
            .flex()
            .items_center()
            .whitespace_nowrap()
            .overflow_hidden()
            .when(index == SAMPLE_CURRENT_LINE, |line| line.bg(ui.current_line))
            .children(pieces)
            // The cursor at the end of the current line, in the theme's cursor color.
            .when(index == SAMPLE_CURRENT_LINE, |line| {
                line.child(div().flex_none().ml(px(1.)).w(px(1.5)).h(px(10.)).bg(ui.cursor))
            })
    });
    let editor = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .py(px(5.))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(5.))
        .bg(opaque(ui.island))
        .border_1()
        .border_color(ui.island_border)
        .font_family(theme::code_font())
        .text_size(px(PREVIEW_CODE_SIZE))
        .line_height(px(PREVIEW_LINE_HEIGHT))
        .children(lines);
    div()
        .h(px(PREVIEW_HEIGHT))
        .p(px(5.))
        .flex()
        .gap(px(4.))
        .rounded(px(ui::RADIUS_MD))
        .bg(opaque(ui.frame))
        .overflow_hidden()
        .child(tree)
        .child(editor)
}

/// The window drawn small in `theme`'s colors, laid out the way `style` lays it out (part 10.4):
/// Islands — rounded islands apart on the frame with its glow; Flat — panels edge to edge, divided
/// by lines. The title bar with the traffic lights, the launchpad, the tree, the editor with a tab
/// and a few lines of code, the terminal under it, the status bar.
fn style_preview(theme: &Theme, style: UiStyle) -> Div {
    let flat = style == UiStyle::Flat;
    let ui = theme.ui;
    // The preview sits on another surface, so it is drawn opaque, as a Flat window is.
    let surfaces = theme::with_surfaces_for(UiStyle::Flat, theme.clone()).ui;
    let line = ui.island_border;
    let gap = if flat { 0. } else { 3. };
    let bar = |width: f32, color: Hsla| {
        div()
            .flex_none()
            .h(px(3.))
            .w(gpui::relative(width))
            .rounded(px(1.5))
            .bg(UiColors::tint(color, 0.7))
    };
    let code = |scope: &str, width: f32| {
        bar(
            width,
            theme
                .style_for(scope)
                .map_or(ui.foreground, |style| style.color),
        )
    };
    // An island with its border and corners, or a Flat panel (its lines come from the layout).
    let panel = || {
        div()
            .bg(surfaces.island)
            .overflow_hidden()
            .when(!flat, |panel| {
                panel
                    .rounded(px(4.))
                    .border_1()
                    .border_color(line)
            })
    };
    let light = |color: Hsla| div().size(px(5.)).rounded(px(2.5)).bg(UiColors::tint(color, 0.85));
    let title = div()
        .flex_none()
        .h(px(15.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap(px(3.))
        .child(light(ui.red))
        .child(light(ui.amber))
        .child(light(ui.green));
    let tool = |active: bool| {
        div().size(px(6.)).rounded(px(1.5)).bg(if active {
            ui.accent
        } else {
            UiColors::tint(ui.text_muted, 0.45)
        })
    };
    let launchpad = div()
        .flex_none()
        .w(px(13.))
        .h_full()
        .pt(px(if flat { 3. } else { 1. }))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .when(flat, |strip| strip.border_r_1().border_color(line))
        .child(tool(true))
        .child(tool(false))
        .child(tool(false));
    let tree = panel()
        .flex_none()
        .w(px(52.))
        .h_full()
        .p(px(5.))
        .flex()
        .flex_col()
        .gap(px(5.))
        .when(flat, |tree| tree.border_r_1().border_color(line))
        .child(bar(0.7, ui.text_muted))
        .child(
            div()
                .mx(px(-3.))
                .px(px(3.))
                .py(px(2.))
                .rounded(px(2.))
                .bg(ui.list_selected)
                .child(bar(0.8, ui.foreground)),
        )
        .child(bar(0.55, ui.vcs_modified))
        .child(bar(0.75, ui.vcs_added))
        .child(bar(0.5, ui.text_muted));
    let line_of = |indent: f32, pieces: Vec<Div>| {
        div()
            .flex_none()
            .pl(px(indent))
            .flex()
            .gap(px(3.))
            .children(pieces)
    };
    let editor = panel()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .h(px(11.))
                .px(px(4.))
                .flex()
                .items_center()
                .child(
                    div()
                        .w(px(24.))
                        .h(px(6.))
                        .rounded(px(if flat { 1. } else { 3. }))
                        .bg(ui.list_selected_inactive),
                ),
        )
        .child(
            div()
                .flex_none()
                .h(px(1.))
                .mx(px(if flat { 0. } else { 3. }))
                .bg(ui.divider),
        )
        .child(
            div()
                .flex_1()
                .p(px(5.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(line_of(0., vec![code("keyword", 0.16), code("function", 0.28)]))
                .child(line_of(8., vec![code("keyword", 0.14), code("string", 0.3)]))
                .child(line_of(8., vec![code("comment", 0.4)]))
                .child(line_of(0., vec![code("punctuation.bracket", 0.05)])),
        );
    let terminal = panel()
        .flex_none()
        .h(px(24.))
        .p(px(5.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .when(flat, |terminal| terminal.border_t_1().border_color(line))
        .child(bar(0.45, ui.text_muted))
        .child(bar(0.25, ui.success));
    let main = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .gap(px(gap))
        .child(editor)
        .child(terminal);
    let body = div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_row()
        .gap(px(gap))
        .pl(px(if flat { 0. } else { 1. }))
        .pr(px(gap))
        .when(flat, |body| body.border_y_1().border_color(line))
        .child(launchpad)
        .child(tree)
        .child(main);
    div()
        .relative()
        .h(px(STYLE_PREVIEW_HEIGHT))
        .flex()
        .flex_col()
        .rounded(px(ui::RADIUS_MD))
        .bg(surfaces.frame)
        // The frame's glow, with the preview's own corners (a child's background isn't clipped by
        // the parent's rounding).
        .when(!flat, |window| {
            window.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(ui::RADIUS_MD))
                    .bg(gpui::linear_gradient(
                        135.,
                        gpui::linear_color_stop(ui.frame_glow, 0.),
                        gpui::linear_color_stop(UiColors::tint(ui.frame_glow, 0.), 0.55),
                    )),
            )
        })
        .child(title)
        .child(body)
        .child(div().flex_none().h(px(9.)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Interface page marks the step the scale is; a scale between steps (a hand-edited
    /// settings file) marks none.
    #[test]
    fn the_scale_marks_its_step() {
        assert_eq!(scale_step(1.), ui_scale::STEPS.iter().position(|step| *step == 1.));
        assert_eq!(scale_step(1.25), ui_scale::STEPS.iter().position(|step| *step == 1.25));
        assert_eq!(scale_step(1.2500001), scale_step(1.25));
        assert_eq!(scale_step(1.05), None);
        assert_eq!(STYLES, [UiStyle::Islands, UiStyle::Flat]);
    }
}
