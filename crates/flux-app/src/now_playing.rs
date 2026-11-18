//! What plays with sound across Flux (FluxGram part 4): the voice message of a plugin's
//! conversation, wherever its window is — a line in every window's status bar (an icon, who,
//! where it is; a click pauses it or goes on) — and the voice messages' speed, kept for the whole
//! app. The conversation holds the player; the app keeps only a weak handle to it, so a closed
//! conversation takes its line with it.

use gpui::{
    AnyElement, App, ClickEvent, Context, EntityId, Global, SharedString, WeakEntity, div,
    prelude::*, px,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::plugin_conversation::Conversation;
use crate::theme::{self, Theme};
use crate::ui;
use crate::workspace::Workspace;

#[derive(Default)]
struct Playing {
    /// The conversation whose voice message plays (or is paused).
    conversation: Option<WeakEntity<Conversation>>,
    /// The voice messages' speed (1×, 1.5×, 2×); 0 — not chosen yet (1×).
    speed: f32,
}

impl Global for Playing {}

/// The conversation's voice message plays now: the status bar's line is its.
pub fn set(conversation: WeakEntity<Conversation>, cx: &mut App) {
    cx.default_global::<Playing>().conversation = Some(conversation);
    cx.refresh_windows();
}

/// The conversation's voice message stopped: the line goes, if it was its.
pub fn clear(conversation: EntityId, cx: &mut App) {
    let playing = cx.default_global::<Playing>();
    if playing
        .conversation
        .as_ref()
        .is_some_and(|weak| weak.entity_id() == conversation)
    {
        playing.conversation = None;
        cx.refresh_windows();
    }
}

/// The line changed (a second went by, paused, played): the windows draw it again.
pub fn refresh(cx: &mut App) {
    if cx
        .try_global::<Playing>()
        .is_some_and(|playing| playing.conversation.is_some())
    {
        cx.refresh_windows();
    }
}

/// The voice messages' speed.
pub fn speed(cx: &App) -> f32 {
    cx.try_global::<Playing>()
        .map(|playing| playing.speed)
        .filter(|speed| *speed > 0.)
        .unwrap_or(1.)
}

pub fn set_speed(speed: f32, cx: &mut App) {
    cx.default_global::<Playing>().speed = speed;
}

/// The status bar's line while a voice message plays anywhere: ⏸ (▶ when paused), who and where
/// it is; a click pauses it or goes on.
pub fn status_item(cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let weak = cx.try_global::<Playing>()?.conversation.clone()?;
    let conversation = weak.upgrade()?;
    let now = conversation.read(cx).now_playing()?;
    let ui = Theme::ui(cx);
    let seconds = now.position.as_secs();
    let label: SharedString = format!("{} · {}:{:02}", now.who, seconds / 60, seconds % 60).into();
    let glyph = match now.playing {
        true => div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(div().w(px(2.)).h(px(9.)).rounded(px(1.)).bg(ui.accent_text))
            .child(div().w(px(2.)).h(px(9.)).rounded(px(1.)).bg(ui.accent_text))
            .into_any_element(),
        false => icon(IconName::Play, ui.accent_text)
            .size(px(11.))
            .into_any_element(),
    };
    Some(
        div()
            .flex_none()
            .pr_1()
            .child(
                div()
                    .id("status-playing")
                    .h(px(20.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(px(ui::RADIUS_SM))
                    .cursor_pointer()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .hover(move |style| style.bg(ui.hover))
                    .tooltip(ui::tooltip(
                        if now.playing { tr("Pause") } else { tr("Play") },
                        None,
                    ))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        weak.update(cx, |conversation, cx| conversation.toggle_now_playing(cx))
                            .ok();
                    })
                    .child(div().w(px(11.)).flex().justify_center().child(glyph))
                    .child(div().max_w(px(260.)).truncate().child(label)),
            )
            .into_any_element(),
    )
}
