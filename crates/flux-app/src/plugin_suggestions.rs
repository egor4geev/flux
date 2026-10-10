//! Plugin suggestions (stage 8.3), as JetBrains IDEs suggest plugins: a file Flux has no language
//! for, while the catalog has a plugin with one, gets a banner above the editor — Install, or Ignore
//! for this kind of file. Once the plugin is installed its language takes the file, and the banner
//! goes by itself.
//!
//! The banner looks at the catalog the process already has ([`crate::plugin_catalog`]: the disk
//! cache, then what was read); without any, the catalog is read once in the background.

use std::path::Path;

use flux_plugin::catalog::{self, IndexEntry};
use gpui::{AnyElement, ClickEvent, Context, Entity, SharedString, Window, div, prelude::*, px};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::IconName;
use crate::theme::{self, Theme, UiColors};
use crate::workspace::Workspace;
use crate::{plugin_catalog, settings, ui};

/// What the banner offers for a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub entry: IndexEntry,
    /// "*.rs" or the file name ("Dockerfile"): what «Ignore» remembers.
    pub kind: String,
    /// The language's name ("Rust").
    pub language: String,
}

/// The suggestion for a file: it has no language, the catalog has a compatible plugin for it, the
/// plugin isn't installed (an installed one turned off is the user's choice), and the kind of file
/// isn't ignored.
pub fn suggestion_for(path: &Path, workspace: &Workspace, cx: &mut Context<Workspace>) -> Option<Suggestion> {
    if flux_syntax::language_for_path(path).is_some() {
        return None;
    }
    let Some(index) = plugin_catalog::index(cx) else {
        plugin_catalog::ensure_fresh(cx);
        return None;
    };
    let entry = catalog::suggest(&index, path)?;
    if workspace.plugins.read(cx).plugin(&entry.id).is_some() {
        return None;
    }
    let kind = catalog::suggestion_kind(entry, path)?;
    if settings::suggestion_ignored(&kind, cx) {
        return None;
    }
    let language = entry
        .language_for(path)
        .map_or_else(|| entry.name.clone(), |language| language.name.clone());
    Some(Suggestion {
        entry: entry.clone(),
        kind,
        language,
    })
}

/// The banner above `editor`, if its file has no language and the catalog has one for it.
pub fn banner(
    workspace: &Workspace,
    editor: &Entity<Editor>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    #[cfg(not(feature = "scenario"))]
    let _ = window;
    let path = editor.read(cx).document.path()?.to_path_buf();
    let suggestion = suggestion_for(&path, workspace, cx)?;
    // A scenario can't click the banner: `FLUX_SCENARIO_ACCEPT_SUGGESTION=1` installs the first
    // suggestion it shows.
    #[cfg(feature = "scenario")]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static ACCEPTED: AtomicBool = AtomicBool::new(false);
        if std::env::var_os("FLUX_SCENARIO_ACCEPT_SUGGESTION").is_some()
            && !ACCEPTED.swap(true, Ordering::Relaxed)
        {
            let entry = suggestion.entry.clone();
            cx.defer_in(window, move |_, window, cx| {
                plugin_catalog::install(entry, cx.weak_entity(), window, cx)
            });
        }
    }
    Some(render(suggestion, cx))
}

fn render(suggestion: Suggestion, cx: &mut Context<Workspace>) -> AnyElement {
    let ui = Theme::ui(cx);
    let progress = plugin_catalog::progress(&suggestion.entry.id, cx);
    let text = match &progress {
        Some(progress) => trf(
            "Installing “{0}”: {1}",
            &[
                &suggestion
                    .entry
                    .translate(crate::i18n::lang_code(), &suggestion.entry.name),
                &progress.label(),
            ],
        ),
        None => trf("Plugins supporting {0} files found.", &[&suggestion.kind]),
    };
    let mut links: Vec<AnyElement> = Vec::new();
    match progress {
        Some(progress) if !progress.installing() => {
            let id = suggestion.entry.id.clone();
            links.push(
                link("plugin-suggestion-cancel", tr("Cancel"), ui)
                    .on_click(move |_, _, cx| plugin_catalog::cancel(&id, cx))
                    .into_any_element(),
            );
        }
        Some(_) => {}
        None => {
            let entry = suggestion.entry.clone();
            links.push(
                link(
                    "plugin-suggestion-install",
                    trf("Install {0}", &[&suggestion.language]),
                    ui,
                )
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    plugin_catalog::install(entry.clone(), cx.weak_entity(), window, cx)
                }))
                .into_any_element(),
            );
            let kind = suggestion.kind.clone();
            let label = if kind.starts_with("*.") {
                tr("Ignore Extension")
            } else {
                tr("Ignore")
            };
            links.push(
                link("plugin-suggestion-ignore", label, ui)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        settings::ignore_suggestion(&kind, cx);
                        cx.notify()
                    }))
                    .into_any_element(),
            );
        }
    }
    div()
        .flex_none()
        .mx_2()
        .mt_1p5()
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_2p5()
        .rounded(px(ui::RADIUS_SM))
        .bg(UiColors::tint(ui.accent, 0.10))
        .border_1()
        .border_color(UiColors::tint(ui.accent, 0.22))
        .text_size(px(theme::TEXT_SM))
        .child(
            crate::icons::icon(IconName::Puzzle, ui.accent_text)
                .size(px(14.))
                .flex_none(),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(ui.foreground)
                .child(text),
        )
        .children(links)
        .into_any_element()
}

/// A link of the banner.
fn link(id: &'static str, label: impl Into<SharedString>, ui: UiColors) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .text_color(ui.accent_text)
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(label.into())
}
