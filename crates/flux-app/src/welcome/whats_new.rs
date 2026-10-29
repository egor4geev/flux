//! What's New (part 10.7): after an update to a new minor version the guide's window shows, once,
//! what the version brought — a few cards, each with Try It, which runs the feature — and a link to
//! the release notes. The number of the version that ran last is kept in a file of its own
//! (`last-version` next to `settings.json`); a fresh install sees the guide, not this.
//!
//! The notes come with Flux: a small file per version in `assets/whats-new/`, in English and
//! Russian (data, not `tr` strings: a release writes them, not the code). Help → What's New shows
//! the notes of the running version — or of the latest version before it that has some — whenever
//! asked, seen or not.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, Context, FontWeight, Hsla, SharedString, Window, div, img, prelude::*, px,
};

use super::{BAR_HEIGHT, Slide, Start, WelcomeGuide};
use crate::i18n::{self, Lang, tr, trf};
use crate::icons::{self, IconName, icon};
use crate::theme::{self, Theme, UiColors};
use crate::workspace::Workspace;
use crate::{settings, ui};

/// The notes of every version that has them: `assets/whats-new/<version>.toml`.
const NOTES: &[&str] = &[
    include_str!("../../assets/whats-new/0.1.0.toml"),
    include_str!("../../assets/whats-new/0.2.0.toml"),
    include_str!("../../assets/whats-new/0.3.0.toml"),
];

/// Where a version's full release notes are read: the release of its tag.
const RELEASES: &str = "https://github.com/egor4geev/flux/releases/tag/v";

/// The running version, as Cargo knows it ("0.1.0").
const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// The width of the notes' column, centered in the window.
const COLUMN_WIDTH: f32 = 600.;
const LOGO_SIZE: f32 = 48.;
/// An item's tinted icon tile.
const TILE_SIZE: f32 = 40.;

/// The notes of a version: when it came out and the few things worth trying in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Notes {
    /// "0.1.0".
    pub version: String,
    /// The release's date, "2026-10-15".
    pub date: String,
    pub items: Vec<Item>,
}

/// A thing worth trying.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub icon: IconName,
    /// The tint of its tile: a shade of the theme by its token's name ("violet", "claude").
    pub color: String,
    pub title: Text,
    pub text: Text,
    /// What Try It runs: an action by its name ("claude::ToggleClaude"); none — no button.
    pub action: Option<String>,
}

/// A text in English and, if the notes have it, in Russian.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    en: String,
    ru: Option<String>,
}

impl Text {
    /// The text in the interface language (English when there is no translation).
    pub fn get(&self, lang: Lang) -> &str {
        match lang {
            Lang::Ru => self.ru.as_deref().unwrap_or(&self.en),
            Lang::En => &self.en,
        }
    }
}

impl Notes {
    /// The notes Help → What's New shows: the running version's, else the latest version's before
    /// it that has notes.
    pub fn current() -> Option<Notes> {
        latest_up_to(&all_notes(), RUNNING)
    }

    /// "0.1" — the version as the title names it.
    pub fn short_version(&self) -> String {
        match parse_version(&self.version) {
            Some((major, minor, _)) => format!("{major}.{minor}"),
            None => self.version.clone(),
        }
    }
}

/// Every bundled version's notes; a file that can't be read is left out (and said so in the log).
fn all_notes() -> Vec<Notes> {
    NOTES
        .iter()
        .filter_map(|text| match parse_notes(text) {
            Ok(notes) => Some(notes),
            Err(error) => {
                log::warn!("what's new: notes can't be read: {error}");
                None
            }
        })
        .collect()
}

/// The notes of the latest version not newer than `running`.
fn latest_up_to(notes: &[Notes], running: &str) -> Option<Notes> {
    let running = parse_version(running)?;
    notes
        .iter()
        .filter_map(|notes| Some((parse_version(&notes.version)?, notes)))
        .filter(|(version, _)| *version <= running)
        .max_by_key(|(version, _)| *version)
        .map(|(_, notes)| notes.clone())
}

/// The notes of `running`'s minor version (any of its patches up to it): what an update to it
/// shows.
fn notes_of_minor(notes: &[Notes], running: &str) -> Option<Notes> {
    let (major, minor, _) = parse_version(running)?;
    latest_up_to(notes, running).filter(|notes| {
        parse_version(&notes.version).is_some_and(|(a, b, _)| (a, b) == (major, minor))
    })
}

/// "0.2.3" → (0, 2, 3); a pre-release or build suffix ("0.2.0-beta.1") doesn't count: the beta of a
/// version is that version for What's New.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

/// Reads a version's notes file.
fn parse_notes(text: &str) -> Result<Notes, String> {
    let table: toml::Table = text
        .parse()
        .map_err(|err: toml::de::Error| err.message().trim().to_string())?;
    let string = |table: &toml::Table, key: &str| -> Result<String, String> {
        table
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("`{key}` is missing"))
    };
    let version = string(&table, "version")?;
    if parse_version(&version).is_none() {
        return Err(format!("`{version}` isn't a version"));
    }
    let date = string(&table, "date")?;
    let items = table
        .get("items")
        .and_then(|items| items.as_array())
        .ok_or("`[[items]]` are missing")?
        .iter()
        .map(|item| {
            let item = item.as_table().ok_or("an item isn't a table")?;
            let icon_name = string(item, "icon")?;
            let icon = IconName::from_file_name(&icon_name)
                .ok_or_else(|| format!("no icon `{icon_name}`"))?;
            let ru = item.get("ru").and_then(|ru| ru.as_table());
            let translated = |key: &str| ru.and_then(|ru| string(ru, key).ok());
            Ok(Item {
                icon,
                color: string(item, "color")?,
                title: Text {
                    en: string(item, "title")?,
                    ru: translated("title"),
                },
                text: Text {
                    en: string(item, "text")?,
                    ru: translated("text"),
                },
                action: item
                    .get("action")
                    .and_then(|action| action.as_str())
                    .map(str::to_string),
            })
        })
        .collect::<Result<Vec<Item>, String>>()?;
    Ok(Notes {
        version,
        date,
        items,
    })
}

// --- After an update ---

/// At startup: Flux runs a newer minor version than the one that ran last, that version has notes
/// and What's New is on — it shows. The running version is written down either way, so the notes
/// show once; on the first launch (no record yet) there is nothing to tell.
pub fn after_update(cx: &mut App) -> bool {
    let Some(path) = last_version_path(
        std::env::var_os("FLUX_LAST_VERSION_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    ) else {
        return false;
    };
    let recorded = std::fs::read_to_string(&path)
        .ok()
        .map(|text| text.trim().to_string());
    if recorded.as_deref() != Some(RUNNING)
        && let Err(error) = write_atomically(&path, RUNNING)
    {
        log::warn!("what's new: can't write {}: {error}", path.display());
    }
    let has_notes = notes_of_minor(&all_notes(), RUNNING).is_some();
    should_show(
        RUNNING,
        recorded.as_deref(),
        has_notes,
        settings::welcome(cx).whats_new,
    )
}

/// What's New shows: the running version is a newer minor (or major) one than `recorded`, it has
/// notes, and What's New is on. No record — the first launch, or a Flux from before the record
/// existed — shows nothing; a patch or a downgrade neither.
fn should_show(running: &str, recorded: Option<&str>, has_notes: bool, enabled: bool) -> bool {
    let minor = |version: &str| parse_version(version).map(|(major, minor, _)| (major, minor));
    let newer = match (minor(running), recorded.and_then(minor)) {
        (Some(now), Some(before)) => now > before,
        _ => false,
    };
    newer && has_notes && enabled
}

/// Where the last version is written: `FLUX_LAST_VERSION_FILE`, else next to `settings.json`; a
/// scenario without the variable keeps nothing (as it keeps no settings).
fn last_version_path(
    explicit: Option<std::ffi::OsString>,
    scenario: bool,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(file) = explicit.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    Some(PathBuf::from(home?).join("Library/Application Support/flux/last-version"))
}

/// Through a temporary file and a `rename`: a Flux quitting halfway leaves the old record whole.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, format!("{text}\n"))?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        std::fs::remove_file(&temp).ok();
    })
}

// --- The page ---

/// What's New in the guide's window: the version's items, and a bar of its own at the bottom (the
/// guide's dots and Continue don't belong here).
pub fn render(
    guide: &mut WelcomeGuide,
    _window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let lang = i18n::lang();
    let body = match guide.notes.clone() {
        Some(notes) => {
            let rows: Vec<AnyElement> = notes
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| item_row(index, item, lang, ui, cx).into_any_element())
                .collect();
            div()
                .w_full()
                .max_w(px(COLUMN_WIDTH))
                .flex()
                .flex_col()
                .child(title(&notes, ui))
                .child(div().mt_4().h(px(1.)).bg(ui.divider))
                .children(rows)
                .into_any_element()
        }
        None => div()
            .pt_10()
            .text_color(ui.text_muted)
            .child(tr("There are no notes for this version yet."))
            .into_any_element(),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id("whats-new")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .items_center()
                .px_10()
                .pt(px(34.))
                .pb_6()
                .child(body),
        )
        .child(bar(guide.notes.as_ref(), ui, cx))
        .into_any_element()
}

/// The logo, «Update · 15 October 2026» and «What's New in Flux 0.1».
fn title(notes: &Notes, ui: UiColors) -> impl IntoElement {
    let eyebrow = trf("Update · {0}", &[&date_label(&notes.date)]);
    div()
        .flex()
        .items_center()
        .gap_4()
        .child(img(icons::LOGO).flex_none().size(px(LOGO_SIZE)))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui.accent_text)
                        .child(eyebrow.to_uppercase()),
                )
                .child(
                    div()
                        .text_size(px(26.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(trf("What's New in Flux {0}", &[&notes.short_version()])),
                ),
        )
}

/// An item: its tinted tile, the title and the text, Try It.
fn item_row(
    index: usize,
    item: &Item,
    lang: Lang,
    ui: UiColors,
    cx: &mut Context<WelcomeGuide>,
) -> impl IntoElement {
    let color = shade(&item.color, ui);
    let action = item.action.clone();
    div()
        .py_3()
        .flex()
        .items_start()
        .gap_4()
        .child(
            div()
                .flex_none()
                .size(px(TILE_SIZE))
                .rounded(px(10.))
                .flex()
                .items_center()
                .justify_center()
                .bg(UiColors::tint(color, 0.16))
                .child(icon(item.icon, color).size(px(20.))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(item.title.get(lang).to_string()),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_MD))
                        .line_height(px(19.))
                        .text_color(ui.text_muted)
                        .child(item.text.get(lang).to_string()),
                ),
        )
        .children(action.map(|action| {
            ui::text_button(
                SharedString::from(format!("whats-new-try-{index}")),
                tr("Try It"),
                false,
                ui,
            )
            .mt(px(2.))
            .on_click(cx.listener(move |guide, _, window, cx| try_it(&action, guide, window, cx)))
        }))
}

/// The bar at the bottom: the release notes, «Show after updates», Continue.
fn bar(notes: Option<&Notes>, ui: UiColors, cx: &mut Context<WelcomeGuide>) -> impl IntoElement {
    let on = settings::welcome(cx).whats_new;
    let version = notes.map(|notes| notes.version.clone());
    div()
        .flex_none()
        .h(px(BAR_HEIGHT))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .border_t_1()
        .border_color(ui.divider)
        .children(version.map(|version| {
            div()
                .id("whats-new-release-notes")
                .h(px(ui::ICON_BUTTON_SIZE))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(ui::RADIUS_SM))
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .cursor_pointer()
                .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                .child(icon(IconName::Globe, ui.text_muted).size(px(13.)))
                .child(tr("Release Notes"))
                .on_click(move |_, _, cx| {
                    crate::plugin_links::open_url(&format!("{RELEASES}{version}"), cx)
                })
        }))
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(ui.text_muted)
                .child(tr("Show after updates")),
        )
        .child(
            ui::switch("whats-new-show", on, ui).on_click(cx.listener(move |_, _, _, cx| {
                settings::update_welcome(cx, |welcome| welcome.whats_new = !on);
                cx.notify();
            })),
        )
        .child(div().w_1())
        .child(
            ui::primary_button("whats-new-continue", tr("Continue"), true, ui)
                .on_click(|_, window, _| window.remove_window()),
        )
}

/// Try It: the item's action runs in the Flux window in front (a new start window when there is
/// none), and What's New goes. The guide's own action shows the guide right here.
fn try_it(
    action: &str,
    guide: &mut WelcomeGuide,
    window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) {
    if action == "welcome::OpenGuide" {
        guide.go(Start::Slide(Slide::Welcome), window, cx);
        return;
    }
    let Ok(action) = cx.build_action(action, None) else {
        log::warn!("what's new: no action `{action}`");
        return;
    };
    // The window in front: the stack's first Flux window; a scenario's panels aren't in the stack —
    // the one opened last.
    let front = cx
        .window_stack()
        .unwrap_or_default()
        .into_iter()
        .chain(cx.windows().into_iter().rev())
        .find_map(|handle| handle.downcast::<Workspace>());
    window.remove_window();
    // After this update: the guide's window is being updated, another one can't be meanwhile.
    cx.defer(move |cx| {
        let handle = front.or_else(|| crate::workspace::open_startup_windows(None, Vec::new(), cx));
        if let Some(handle) = handle {
            handle
                .update(cx, |_, window, cx| {
                    window.activate_window();
                    window.dispatch_action(action, cx);
                })
                .ok();
        }
    });
}

/// A shade of the theme by its token's name; the accent for a name it doesn't know.
fn shade(name: &str, ui: UiColors) -> Hsla {
    match name {
        "blue" => ui.blue,
        "indigo" => ui.indigo,
        "violet" => ui.violet,
        "pink" => ui.pink,
        "red" => ui.red,
        "orange" => ui.orange,
        "amber" => ui.amber,
        "lime" => ui.lime,
        "green" => ui.green,
        "teal" => ui.teal,
        "cyan" => ui.cyan,
        "claude" => ui.claude,
        _ => ui.accent,
    }
}

/// "2026-10-15" → «15 October 2026» in the interface language; a date it can't read — as it is.
fn date_label(date: &str) -> String {
    let mut parts = date.split('-').map(|part| part.parse::<u32>().ok());
    let (Some(Some(year)), Some(Some(month)), Some(Some(day))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return date.to_string();
    };
    let template = match month {
        1 => "{0} January {1}",
        2 => "{0} February {1}",
        3 => "{0} March {1}",
        4 => "{0} April {1}",
        5 => "{0} May {1}",
        6 => "{0} June {1}",
        7 => "{0} July {1}",
        8 => "{0} August {1}",
        9 => "{0} September {1}",
        10 => "{0} October {1}",
        11 => "{0} November {1}",
        12 => "{0} December {1}",
        _ => return date.to_string(),
    };
    trf(template, &[&day, &year])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_major_and_minor() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("v0.2.3"), Some((0, 2, 3)));
        assert_eq!(parse_version("0.2.0-beta.1"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.4"), Some((1, 4, 0)));
        assert_eq!(parse_version("one"), None);
        // A newer minor or major with notes shows, once enabled.
        assert!(should_show("0.2.0", Some("0.1.0"), true, true));
        assert!(should_show("1.0.0", Some("0.9.4"), true, true));
        assert!(should_show("0.2.0", Some("0.1.9\n"), true, true));
        // A patch, the same version, a downgrade, no notes, turned off: nothing.
        assert!(!should_show("0.2.1", Some("0.2.0"), true, true));
        assert!(!should_show("0.2.0", Some("0.2.0"), true, true));
        assert!(!should_show("0.1.0", Some("0.2.0"), true, true));
        assert!(!should_show("0.2.0", Some("0.1.0"), false, true));
        assert!(!should_show("0.2.0", Some("0.1.0"), true, false));
        // No record — the first launch, or a Flux from before the record: nothing to tell.
        assert!(!should_show("0.2.0", None, true, true));
        // A beta of the next minor is that minor.
        assert!(should_show("0.2.0-beta.2", Some("0.1.0"), true, true));
    }

    #[test]
    fn the_record_lives_next_to_the_settings_unless_told_otherwise() {
        let home = Some("/Users/someone".into());
        assert_eq!(
            last_version_path(None, false, home.clone()),
            Some(PathBuf::from(
                "/Users/someone/Library/Application Support/flux/last-version"
            ))
        );
        assert_eq!(
            last_version_path(Some("/tmp/x".into()), true, home.clone()),
            Some(PathBuf::from("/tmp/x"))
        );
        // A scenario without the variable keeps nothing.
        assert_eq!(last_version_path(None, true, home), None);
        assert_eq!(last_version_path(Some("".into()), true, None), None);
    }

    #[test]
    fn the_record_is_written_whole() {
        let dir = std::env::temp_dir().join(format!("flux-whats-new-{}", std::process::id()));
        let path = dir.join("nested").join("last-version");
        write_atomically(&path, "0.1.0").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0.1.0\n");
        write_atomically(&path, "0.2.0").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "0.2.0");
        // No temporary file is left behind.
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bundled_notes_read_in_both_languages() {
        let notes = all_notes();
        assert_eq!(notes.len(), NOTES.len(), "every bundled notes file reads");
        for notes in &notes {
            assert!(parse_version(&notes.version).is_some());
            assert!(
                !date_label(&notes.date).contains('-'),
                "a real date: {}",
                notes.date
            );
            assert!(
                (3..=5).contains(&notes.items.len()),
                "3 to 5 items: {}",
                notes.version
            );
            for item in &notes.items {
                for text in [&item.title, &item.text] {
                    assert!(!text.get(Lang::En).is_empty());
                    assert!(text.ru.is_some(), "a Russian text for {:?}", text.en);
                    assert_ne!(text.get(Lang::Ru), text.get(Lang::En));
                }
            }
        }
        // The running version's notes are there (Help → What's New never comes up empty).
        assert!(Notes::current().is_some());
    }

    #[test]
    fn notes_are_parsed_and_picked_by_version() {
        let file = |version: &str| {
            format!(
                "version = \"{version}\"\ndate = \"2026-11-02\"\n\n[[items]]\nicon = \"sparkle\"\n\
                 color = \"violet\"\ntitle = \"T\"\ntext = \"X\"\nru.title = \"Т\"\n\n\
                 [[items]]\nicon = \"claude\"\ncolor = \"claude\"\n\
                 action = \"claude::ToggleClaude\"\ntitle = \"C\"\ntext = \"Y\"\n"
            )
        };
        let notes = parse_notes(&file("0.2.0")).unwrap();
        assert_eq!(notes.items.len(), 2);
        assert_eq!(notes.items[0].icon, IconName::Sparkle);
        assert_eq!(notes.items[0].title.get(Lang::Ru), "Т");
        // No Russian — English.
        assert_eq!(notes.items[0].text.get(Lang::Ru), "X");
        assert_eq!(notes.items[0].action, None);
        assert_eq!(
            notes.items[1].action.as_deref(),
            Some("claude::ToggleClaude")
        );
        assert_eq!(notes.short_version(), "0.2");
        assert!(parse_notes("version = \"0.2.0\"").is_err());
        assert!(parse_notes(&file("0.2.0").replace("sparkle", "no-such-icon")).is_err());

        let all = vec![
            parse_notes(&file("0.1.0")).unwrap(),
            parse_notes(&file("0.2.0")).unwrap(),
            parse_notes(&file("0.4.0")).unwrap(),
        ];
        let version = |notes: Option<Notes>| notes.map(|notes| notes.version);
        assert_eq!(
            version(latest_up_to(&all, "0.2.5")).as_deref(),
            Some("0.2.0")
        );
        assert_eq!(
            version(latest_up_to(&all, "0.3.0")).as_deref(),
            Some("0.2.0")
        );
        assert_eq!(version(latest_up_to(&all, "0.0.9")), None);
        // An update shows only its own minor's notes.
        assert_eq!(
            version(notes_of_minor(&all, "0.2.1")).as_deref(),
            Some("0.2.0")
        );
        assert_eq!(version(notes_of_minor(&all, "0.3.0")), None);
    }
}
