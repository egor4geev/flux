//! The first slide: the logo, a sentence about Flux, three of its ways of working with their keys,
//! and the interface language — chosen here, it applies at once (the guide, the menus and every
//! window are redrawn in it), so the rest of the guide reads in it.

use gpui::{
    AnyElement, BoxShadow, ClickEvent, Context, Div, FontWeight, SharedString, Window, div, img,
    point, prelude::*, px,
};

use super::WelcomeGuide;
use crate::appearance_settings;
use crate::i18n::{self, Lang, tr, trf};
use crate::icons::{self, IconName, icon};
use crate::settings::{self, LanguageChoice};
use crate::theme::{self, Theme, UiColors};
use crate::ui;

const LOGO_SIZE: f32 = 96.;
/// The sentence about Flux wraps at this width.
const LEAD_WIDTH: f32 = 540.;

pub fn render(
    _guide: &mut WelcomeGuide,
    _window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    // The ways of working, with the keys the keymap really has.
    let chips = [
        (
            ui::shortcut_for(&crate::file_finder::Toggle, cx),
            tr("Everything from the keyboard"),
        ),
        (
            ui::shortcut_for(&crate::git::Commit, cx),
            tr("Version control in windows"),
        ),
        (
            ui::shortcut_for(&crate::claude::ToggleClaude, cx),
            tr("Claude Code inside"),
        ),
    ];
    // The logo with a soft accent glow behind it: the shadow of a small disc hidden under the
    // logo's middle (gpui has no radial gradient).
    let glow = LOGO_SIZE * 0.4;
    let logo = div()
        .relative()
        .size(px(LOGO_SIZE))
        .child(
            div()
                .absolute()
                .top(px((LOGO_SIZE - glow) / 2.))
                .left(px((LOGO_SIZE - glow) / 2.))
                .size(px(glow))
                .rounded(px(glow / 2.))
                .shadow(vec![BoxShadow {
                    color: UiColors::tint(ui.accent, 0.5),
                    offset: point(px(0.), px(8.)),
                    blur_radius: px(80.),
                    spread_radius: px(26.),
                }]),
        )
        .child(img(icons::LOGO).absolute().inset_0().size(px(LOGO_SIZE)));
    div()
        .size_full()
        .px_10()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(logo)
        .child(
            div()
                .mt(px(22.))
                .text_size(px(32.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("Welcome to Flux")),
        )
        .child(
            div()
                .mt(px(10.))
                .max_w(px(LEAD_WIDTH))
                .text_center()
                .text_size(px(theme::TEXT_LG))
                .line_height(px(23.))
                .text_color(ui.text_muted)
                .child(tr(
                    "A fast native code editor for macOS — with code understanding, version control and Claude Code built in.",
                )),
        )
        .child(
            // One row (gpui wraps a centered flex row too early): the guide is never narrower.
            div()
                .mt(px(22.))
                .flex()
                .gap_2()
                .children(chips.into_iter().map(|(keys, label)| chip(keys, label, ui))),
        )
        .child(div().mt(px(34.)).child(language_choice(cx)))
        .into_any_element()
}

/// A way of working: its keys and a few words, in a pill.
fn chip(keys: Option<SharedString>, label: &'static str, ui: UiColors) -> Div {
    div()
        .flex_none()
        .h(px(30.))
        .pl(px(6.))
        .pr(px(12.))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(15.))
        .bg(ui.hover)
        .border_1()
        .border_color(ui.island_border)
        .text_size(px(theme::TEXT_MD - 0.5))
        .children(keys.map(|keys| ui::keys(&keys, ui)))
        .child(label)
}

/// The interface language as a segmented control: System (with the language it gives now),
/// English, Русский — as the steps of the scale on Settings → Interface look.
fn language_choice(cx: &mut Context<WelcomeGuide>) -> Div {
    let ui = Theme::ui(cx);
    let current = settings::appearance(cx).language;
    let choices = [
        (LanguageChoice::System, SharedString::from(tr("System"))),
        (LanguageChoice::English, i18n::lang_name(Lang::En).into()),
        (LanguageChoice::Russian, i18n::lang_name(Lang::Ru).into()),
    ];
    let buttons = choices
        .into_iter()
        .enumerate()
        .map(|(index, (choice, label))| {
            let selected = choice == current;
            div()
                .id(("welcome-language", index))
                .flex_none()
                .h(px(28.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(ui::RADIUS_SM))
                .cursor_pointer()
                .text_color(if selected {
                    ui.accent_text
                } else {
                    ui.text_muted
                })
                .when(selected, |button| {
                    button
                        .bg(ui.list_selected)
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .when(!selected, |button| {
                    button.hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                })
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    appearance_settings::set_language(choice, cx);
                    cx.notify();
                }))
                .child(label)
        });
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM + 0.5))
                .text_color(ui.dim)
                .child(icon(IconName::Globe, ui.dim).size(px(13.)))
                .child(tr("Interface language")),
        )
        .child(
            div()
                .flex()
                .gap_1()
                .p_1()
                .rounded(px(ui::RADIUS_MD))
                .border_1()
                .border_color(ui.island_border)
                .children(buttons),
        )
        // "System" says what it gives now.
        .when(current == LanguageChoice::System, |row| {
            row.child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(trf("Now: {0}", &[&i18n::lang_name(i18n::system_lang())])),
            )
        })
}
