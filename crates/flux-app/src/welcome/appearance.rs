//! The look (the guide's second slide): the theme — Flux's own and the catalog's, a card each
//! drawn in its colors, one click shows it (a theme of the catalog is installed first) — Sync with
//! OS, the look of the window (Islands or Flat) and the scale. Everything applies at once, to the
//! guide too.
//!
//! The catalog's themes aren't installed yet, so their colors are read from their packages (a few
//! kilobytes each) in the background when the slide first shows: their cards are drawn as the
//! installed ones are. A click installs the plugin quietly (it asks for no permissions) and then
//! shows the theme.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use flux_plugin::catalog::{Category, IndexEntry};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, FontWeight, Global, SharedString,
    Stateful, Window, div, prelude::*, px,
};

use super::{WelcomeGuide, heading};
use crate::appearance_settings::{STYLES, style_name, style_preview, theme_preview};
use crate::i18n::{lang_code, tr, trf};
use crate::icons::{IconName, icon};
use crate::notifications::{Progress, progress_bar};
use crate::settings::{self, UiStyle};
use crate::theme::{self, Appearance, FLUX_DAY, FLUX_NIGHT, Theme, UiColors};
use crate::{plugin_catalog, quick_switch, ui, ui_scale};

/// A theme card: as wide as six fit in a row of the slide.
const CARD_WIDTH: f32 = 140.;
/// The room between the dark and the light themes.
const GROUP_GAP: f32 = 18.;
/// A card of the window's look.
const STYLE_CARD_WIDTH: f32 = 196.;

/// The catalog's themes not installed yet, their colors read from their packages for the cards; and
/// the installs started from here.
#[derive(Default)]
struct CatalogThemes {
    /// The themes of a plugin's package, by plugin id.
    themes: HashMap<String, Vec<Theme>>,
    /// Packages being read, by plugin id; a package that couldn't be read stays here (its cards
    /// are drawn in the base colors of their appearance).
    started: HashSet<String>,
    /// Why an install from the guide failed, by plugin id (until it is tried again).
    failed: HashMap<String, SharedString>,
    /// The catalog was asked for (once per process: asking touches the catalog's global, whose
    /// observers — the Marketplace — redraw).
    asked: bool,
}

impl Global for CatalogThemes {}

/// Where a card's theme comes from.
#[derive(Clone)]
enum Source {
    /// Installed: Flux's own (`None`) or a plugin's.
    Installed(Option<SharedString>),
    /// The catalog's plugin, not installed yet.
    Catalog(Arc<IndexEntry>),
}

/// A card of the theme row.
struct Card {
    name: SharedString,
    appearance: Appearance,
    source: Source,
    /// Its colors; none for a catalog theme whose package hasn't been read.
    preview: Option<Theme>,
}

pub fn render(
    _guide: &mut WelcomeGuide,
    window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    read_catalog_themes(cx);
    let lead = match ui::shortcut_for(&quick_switch::Toggle, cx) {
        Some(keys) => trf(
            "Everything changes at once — this window already shows your choice. Later: Settings → Appearance, or {0} Quick Switch.",
            &[&keys],
        ),
        None => tr(
            "Everything changes at once — this window already shows your choice. Later: Settings → Appearance.",
        )
        .to_string(),
    };
    div()
        .id("welcome-appearance")
        .size_full()
        .overflow_y_scroll()
        .px(px(36.))
        .pt(px(28.))
        .pb(px(24.))
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(heading(
            tr("Setup · Appearance"),
            tr("Make Flux Yours"),
            lead,
            ui,
        ))
        .child(themes_section(window, cx))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(px(28.))
                .gap_y(px(22.))
                .child(style_section(cx))
                .child(scale_section(cx)),
        )
        .into_any_element()
}

// --- Themes ---

fn themes_section(window: &mut Window, cx: &mut Context<WelcomeGuide>) -> Div {
    let ui = Theme::ui(cx);
    let appearance = settings::appearance(cx);
    let sync = appearance.sync_with_os;
    let cards = cards(cx);
    let shown = Theme::get(cx).name.clone();
    // With Sync with OS, the dark and the light choice are marked, each falling back to the
    // default of its appearance when its theme is gone, as `theme::chosen` does.
    let choice = |of: Appearance, setting: Option<&str>, default: &str| {
        let name = setting.unwrap_or(default);
        let known = cards.iter().any(|card| {
            card.name.as_ref() == name
                && card.appearance == of
                && matches!(card.source, Source::Installed(_))
        });
        SharedString::from(if known { name } else { default }.to_string())
    };
    let dark = choice(
        Appearance::Dark,
        appearance.dark_theme.as_deref(),
        FLUX_NIGHT,
    );
    let light = choice(
        Appearance::Light,
        appearance.light_theme.as_deref(),
        FLUX_DAY,
    );
    let groups = [Appearance::Dark, Appearance::Light].map(|group| {
        let marked = match (sync, group) {
            (false, _) => shown.clone(),
            (true, Appearance::Dark) => dark.clone(),
            (true, Appearance::Light) => light.clone(),
        };
        div().flex().gap(px(10.)).children(
            cards
                .iter()
                .enumerate()
                .filter(|(_, card)| card.appearance == group)
                .map(|(index, card)| {
                    let selected = card.name == marked;
                    theme_card(index, card, selected, sync, window, cx).into_any_element()
                }),
        )
    });
    let system = theme::system_appearance(cx);
    let caption = sync.then(|| match system {
        Appearance::Dark => tr("macOS is dark now: Flux shows the dark theme."),
        Appearance::Light => tr("macOS is light now: Flux shows the light theme."),
    });
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ui::section_label(tr("Theme"), ui))
                .children(caption.map(|caption| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(caption)
                }))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM + 0.5))
                        .text_color(ui.text_muted)
                        .child(tr("Sync with OS")),
                )
                .child(
                    ui::switch("welcome-sync-with-os", sync, ui).on_click(cx.listener(
                        move |_, _: &ClickEvent, _, cx| {
                            theme::set_sync_with_os(!sync, cx);
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(px(GROUP_GAP))
                .gap_y(px(10.))
                .children(groups),
        )
}

/// The installed themes, then the catalog's ones not installed, the dark ones first.
fn cards(cx: &mut Context<WelcomeGuide>) -> Vec<Card> {
    let installed = theme::available(cx);
    let installed_plugins: HashSet<SharedString> = installed
        .iter()
        .filter_map(|info| info.plugin.clone())
        .collect();
    let mut cards: Vec<Card> = installed
        .iter()
        .map(|info| Card {
            name: info.name.clone(),
            appearance: info.appearance,
            source: Source::Installed(info.plugin.clone()),
            preview: theme::by_name(&info.name, cx),
        })
        .collect();
    let catalog = catalog_theme_plugins(cx);
    let previews = cx.try_global::<CatalogThemes>();
    for entry in catalog {
        if installed_plugins.contains(entry.id.as_str()) {
            continue;
        }
        let entry = Arc::new(entry);
        for spec in &entry.themes {
            if cards.iter().any(|card| card.name.as_ref() == spec.name) {
                continue;
            }
            let appearance = if spec.appearance == "light" {
                Appearance::Light
            } else {
                Appearance::Dark
            };
            let preview = previews
                .and_then(|previews| previews.themes.get(&entry.id))
                .and_then(|themes| themes.iter().find(|theme| theme.name.as_ref() == spec.name))
                .cloned();
            cards.push(Card {
                name: spec.name.clone().into(),
                appearance,
                source: Source::Catalog(entry.clone()),
                preview,
            });
        }
    }
    // Flux's own first in each group, then the installed plugins' ones, then the catalog's.
    let rank = |card: &Card| match &card.source {
        Source::Installed(None) => 0,
        Source::Installed(Some(_)) => 1,
        Source::Catalog(_) => 2,
    };
    cards.sort_by_key(|card| (card.appearance != Appearance::Dark, rank(card)));
    cards
}

/// The catalog's plugins with themes this Flux runs (the index in memory).
fn catalog_theme_plugins(cx: &App) -> Vec<IndexEntry> {
    let Some(index) = plugin_catalog::peek(cx) else {
        return Vec::new();
    };
    index
        .plugins
        .iter()
        .filter(|entry| {
            entry.compatible()
                && !entry.themes.is_empty()
                && entry.categories.contains(&Category::Themes)
        })
        .cloned()
        .collect()
}

/// Reads the colors of the catalog's themes not installed yet from their packages, once per
/// process; the catalog is read first if it isn't yet.
fn read_catalog_themes(cx: &mut App) {
    if !cx
        .try_global::<CatalogThemes>()
        .is_some_and(|previews| previews.asked)
    {
        cx.default_global::<CatalogThemes>().asked = true;
        // The disk cache at once, the catalog itself in the background.
        plugin_catalog::index(cx);
        plugin_catalog::ensure_fresh(cx);
    }
    let installed: HashSet<SharedString> = theme::available(cx)
        .into_iter()
        .filter_map(|info| info.plugin)
        .collect();
    let started = cx
        .try_global::<CatalogThemes>()
        .map(|previews| previews.started.clone())
        .unwrap_or_default();
    let wanted: Vec<IndexEntry> = catalog_theme_plugins(cx)
        .into_iter()
        .filter(|entry| !installed.contains(entry.id.as_str()) && !started.contains(&entry.id))
        .collect();
    for entry in wanted {
        cx.default_global::<CatalogThemes>()
            .started
            .insert(entry.id.clone());
        let id = entry.id.clone();
        let read = cx.background_spawn(async move { package_themes(&entry) });
        cx.spawn(async move |cx| {
            let themes = read.await;
            cx.update(|cx| {
                if let Some(themes) = themes {
                    cx.default_global::<CatalogThemes>()
                        .themes
                        .insert(id, themes);
                    cx.refresh_windows();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// The themes in the package of `entry`: downloaded into the catalog's cache (an install later
/// takes it from there), unpacked, read. Blocks: off the UI thread.
fn package_themes(entry: &IndexEntry) -> Option<Vec<Theme>> {
    let archive = flux_plugin::catalog::download(entry, &AtomicBool::new(false), &mut |_, _| {})
        .inspect_err(|err| log::info!("theme previews of {}: {err}", entry.id))
        .ok()?;
    let candidate = flux_plugin::install::inspect(&archive).ok()?;
    let plugin = &candidate.entry;
    Some(
        plugin
            .manifest
            .themes
            .iter()
            .filter_map(|spec| {
                let bytes = plugin.files.read(&spec.file)?;
                theme::parse_theme(std::str::from_utf8(&bytes).ok()?).ok()
            })
            .collect(),
    )
}

/// A theme's card: the small window in its colors, its name, where it comes from; the chosen one
/// has the accent border and a check.
fn theme_card(
    index: usize,
    card: &Card,
    selected: bool,
    sync: bool,
    window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> Stateful<Div> {
    let ui = Theme::ui(cx);
    let preview = card
        .preview
        .clone()
        .unwrap_or_else(|| Theme::base(card.appearance));
    let progress = match &card.source {
        Source::Catalog(entry) => plugin_catalog::progress(&entry.id, cx),
        Source::Installed(_) => None,
    };
    let failed = match &card.source {
        Source::Catalog(entry) => cx
            .try_global::<CatalogThemes>()
            .and_then(|previews| previews.failed.get(&entry.id).cloned()),
        Source::Installed(_) => None,
    };
    // Where it comes from; a catalog theme says so with a mark, and its install's progress or
    // failure takes the line.
    let origin: AnyElement = match (&card.source, &progress, &failed) {
        (_, Some(progress), _) => origin_text(
            if progress.installing() {
                tr("Installing…").into()
            } else {
                progress.label().into()
            },
            ui.dim,
        ),
        (_, None, Some(_)) => {
            origin_text(tr("Couldn't install: click to try again").into(), ui.error)
        }
        (Source::Installed(plugin), ..) => origin_text(origin_name(plugin.as_ref(), cx), ui.dim),
        (Source::Catalog(_), ..) => catalog_badge(ui).into_any_element(),
    };
    let glyph = match card.appearance {
        Appearance::Dark => IconName::Moon,
        Appearance::Light => IconName::Sun,
    };
    let trailing: AnyElement = if selected {
        icon(IconName::Check, ui.accent_text)
            .size(px(14.))
            .into_any_element()
    } else {
        icon(glyph, ui.dim).size(px(13.)).into_any_element()
    };
    let name = card.name.clone();
    let appearance = card.appearance;
    let source = card.source.clone();
    let window_handle = window.window_handle();
    div()
        .id(("welcome-theme", index))
        .flex_none()
        .w(px(CARD_WIDTH))
        .flex()
        .flex_col()
        .gap_1p5()
        .p_1p5()
        .rounded(px(ui::RADIUS_LG))
        .border_2()
        .border_color(if selected {
            ui.accent
        } else {
            ui.island_border
        })
        .when(selected, |card| card.bg(UiColors::tint(ui.accent, 0.06)))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
            match &source {
                Source::Installed(_) => choose(&name, appearance, sync, cx),
                Source::Catalog(entry) => {
                    install_and_choose(entry, name.clone(), appearance, window_handle, cx)
                }
            }
            cx.notify();
        }))
        .child(theme_preview(&preview))
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
                        .gap_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ui.foreground)
                                .child(card.name.clone()),
                        )
                        .child(trailing),
                )
                .child(div().flex().child(origin))
                .children(progress.map(|progress| {
                    progress_bar(
                        match progress.fraction() {
                            Some(done) if !progress.installing() => Progress::Fraction(done),
                            _ => Progress::Indeterminate,
                        },
                        ui,
                    )
                })),
        )
}

/// A card's second line: where its theme comes from, or how its install goes.
fn origin_text(text: SharedString, color: gpui::Hsla) -> AnyElement {
    div()
        .min_w_0()
        .truncate()
        .text_size(px(theme::TEXT_XS))
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// The small "Catalog" mark of a theme not installed yet.
fn catalog_badge(ui: UiColors) -> Div {
    div()
        .flex_none()
        .h(px(18.))
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(9.))
        .bg(ui.accent_soft)
        .text_size(px(theme::TEXT_XS))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ui.accent_text)
        .child(icon(IconName::Download, ui.accent_text).size(px(10.)))
        .child(tr("Catalog"))
}

/// Where an installed theme comes from: Flux itself (its bundled plugins), or the name of its
/// plugin in the interface language.
fn origin_name(plugin: Option<&SharedString>, cx: &App) -> SharedString {
    let Some(plugin) = plugin.filter(|plugin| !bundled_ids().contains(plugin.as_ref())) else {
        return tr("Built into Flux").into();
    };
    plugin_catalog::peek(cx)
        .and_then(|index| {
            let entry = index.plugin(plugin)?;
            Some(entry.translate(lang_code(), &entry.name).to_string())
        })
        .unwrap_or_else(|| plugin.to_string())
        .into()
}

/// The ids of the plugins compiled into Flux.
fn bundled_ids() -> &'static HashSet<String> {
    static IDS: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        crate::bundled::plugins()
            .iter()
            .filter_map(|plugin| flux_plugin::registry::load_bundled(plugin).ok())
            .map(|entry| entry.id().to_string())
            .collect()
    })
}

/// Shows the theme: the chosen one, or — with Sync with OS — the one for its appearance.
fn choose(name: &str, appearance: Appearance, sync: bool, cx: &mut App) {
    if sync {
        theme::select_for(appearance, name, cx);
    } else {
        theme::select(name, cx);
    }
}

/// A catalog theme's card was clicked: its plugin is installed in the background, then the theme
/// shows (as the settings say at that moment: Sync with OS may have been turned on meanwhile).
fn install_and_choose(
    entry: &IndexEntry,
    name: SharedString,
    appearance: Appearance,
    window: gpui::AnyWindowHandle,
    cx: &mut Context<WelcomeGuide>,
) {
    if let Some(previews) = cx.try_global::<CatalogThemes>()
        && previews.failed.contains_key(&entry.id)
    {
        cx.default_global::<CatalogThemes>()
            .failed
            .remove(&entry.id);
    }
    let id = entry.id.clone();
    let install = plugin_catalog::install_quietly(entry.clone(), Some(window), cx);
    cx.spawn(async move |_, cx| {
        let result = install.await;
        cx.update(|cx| match result {
            Ok(true) => {
                let sync = settings::appearance(cx).sync_with_os;
                choose(&name, appearance, sync, cx);
            }
            Ok(false) => {}
            Err(reason) => {
                cx.default_global::<CatalogThemes>()
                    .failed
                    .insert(id, reason);
                cx.refresh_windows();
            }
        })
        .ok();
    })
    .detach();
}

// --- The window's look and the scale ---

fn style_section(cx: &mut Context<WelcomeGuide>) -> Div {
    let ui = Theme::ui(cx);
    let current = ui::style(cx);
    // The small windows in the shown theme's own colors, glass included (the global theme's
    // surfaces are opaque while Flat is on).
    let shown = Theme::get(cx);
    let theme = theme::by_name(&shown.name, cx).unwrap_or_else(|| shown.clone());
    let cards = STYLES.into_iter().enumerate().map(|(index, style)| {
        let selected = style == current;
        div()
            .id(("welcome-style", index))
            .flex_none()
            .w(px(STYLE_CARD_WIDTH))
            .flex()
            .flex_col()
            .gap_1p5()
            .p_1p5()
            .rounded(px(ui::RADIUS_LG))
            .border_2()
            .border_color(if selected {
                ui.accent
            } else {
                ui.island_border
            })
            .when(selected, |card| card.bg(UiColors::tint(ui.accent, 0.06)))
            .cursor_pointer()
            .hover(move |card| card.bg(ui.hover))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                ui::set_style(style, cx);
                cx.notify();
            }))
            .child(style_preview(&theme, style))
            .child(
                div().px_1().pb_0p5().flex().flex_col().gap_0p5().child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ui.foreground)
                                .child(style_name(style)),
                        )
                        .when(selected, |row| {
                            row.child(icon(IconName::Check, ui.accent_text).size(px(14.)))
                        }),
                ),
            )
            .tooltip(ui::tooltip(
                match style {
                    UiStyle::Islands => tr("Rounded panels on the window’s glass"),
                    UiStyle::Flat => tr("Panels edge to edge, divided by lines"),
                },
                None,
            ))
    });
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(ui::section_label(tr("Window Style"), ui))
        .child(div().flex().gap(px(10.)).children(cards))
}

fn scale_section(cx: &mut Context<WelcomeGuide>) -> Div {
    let ui = Theme::ui(cx);
    let scale = settings::ui_scale(cx);
    let steps = ui_scale::STEPS.iter().enumerate().map(|(index, value)| {
        let value = *value;
        let selected = (value - scale).abs() < 0.001;
        div()
            .id(("welcome-scale", index))
            .flex_none()
            .h(px(26.))
            .px(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .whitespace_nowrap()
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .text_size(px(theme::TEXT_SM + 0.5))
            .text_color(if selected {
                ui.foreground
            } else {
                ui.text_muted
            })
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
    let hints: Vec<(SharedString, &'static str)> = [
        (ui::shortcut_for(&ui_scale::ZoomIn, cx), tr("Zoom In")),
        (ui::shortcut_for(&ui_scale::ZoomOut, cx), tr("Zoom Out")),
    ]
    .into_iter()
    .filter_map(|(keys, label)| Some((keys?, label)))
    .collect();
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(ui::section_label(tr("Scale"), ui))
        .child(
            div().flex().child(
                div()
                    .flex()
                    .gap_0p5()
                    .p(px(3.))
                    .rounded(px(ui::RADIUS_MD))
                    .bg(ui.input_background)
                    .border_1()
                    .border_color(ui.input_border)
                    .children(steps),
            ),
        )
        .child(
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
}
