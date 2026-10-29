//! What's New of a version found by the update check (part 10.8): a dialog with the release's
//! notes (Markdown of the release, drawn by [`crate::markdown`]), its date and channel, a link to
//! its page on GitHub, and Update and Restart (↵), Skip This Version, Later (Esc). While the update
//! downloads, the dialog shows its progress instead of the buttons.

use flux_update::{Channel, Manifest};
use gpui::{
    App, ClickEvent, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, FontWeight,
    KeyBinding, Render, Subscription, Window, actions, div, prelude::*, px, relative,
};

use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::markdown::{self, Block};
use crate::motion::TransitionExt;
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::update_settings::{channel_color, channel_label};
use crate::updates::{self, Phase};
use crate::workspace::Workspace;

actions!(release_notes, [Confirm, Dismiss]);

const WIDTH: f32 = 600.;
const HEIGHT: f32 = 560.;
/// The tile with the spark, left of the version.
const TILE: f32 = 44.;

pub fn init(cx: &mut App) {
    let context = Some("ReleaseNotes");
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// Opens the notes of `manifest` over the window (they stay if already open).
pub fn open(
    workspace: &mut Workspace,
    manifest: Manifest,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.modal_is::<ReleaseNotes>() {
        return;
    }
    workspace.toggle_dialog(window, cx, |window, cx| {
        ReleaseNotes::new(manifest, window, cx)
    });
}

pub struct ReleaseNotes {
    focus_handle: FocusHandle,
    manifest: Manifest,
    blocks: Vec<Block>,
    _subscription: Option<Subscription>,
}

impl ReleaseNotes {
    fn new(manifest: Manifest, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut blocks = markdown::parse(&manifest.notes);
        if blocks.is_empty() {
            blocks = markdown::plain(tr("This release has no notes."));
        }
        let subscription =
            updates::updates(cx).map(|updates| cx.observe(&updates, |_, _, cx| cx.notify()));
        Self {
            focus_handle: cx.focus_handle(),
            manifest,
            blocks,
            _subscription: subscription,
        }
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        cx.defer(updates::install_update);
    }

    fn skip(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        cx.defer(updates::skip_update);
    }

    /// The phase of this version's update, if it is under way.
    fn phase(&self, cx: &App) -> Phase {
        let Some(updates) = updates::updates(cx) else {
            return Phase::Idle;
        };
        let phase = updates.read(cx).phase().clone();
        let version = match &phase {
            Phase::Idle => return Phase::Idle,
            Phase::Downloading { version, .. } | Phase::Preparing { version } => version,
            Phase::Ready(prepared) => &prepared.version,
        };
        if *version == self.manifest.version {
            phase
        } else {
            Phase::Idle
        }
    }

    fn render_footer(&self, ui: UiColors, cx: &mut Context<Self>) -> gpui::Div {
        let footer = div()
            .flex_none()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(ui.divider);
        let page = updates::release_page(&self.manifest);
        let link = div()
            .id("release-notes-page")
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(theme::TEXT_SM))
            .text_color(ui.accent_text)
            .cursor_pointer()
            .hover(|style| style.underline())
            .tooltip(ui::tooltip(page.clone(), None))
            .on_click(move |_, _, cx| cx.open_url(&page))
            .child(tr("Release page"))
            .child(icon(IconName::Globe, ui.accent_text).size(px(11.)));
        let footer = footer.child(link).child(div().flex_1());
        match self.phase(cx) {
            Phase::Downloading { done, total, .. } => {
                let fraction = if total > 0 {
                    (done as f32 / total as f32).clamp(0., 1.)
                } else {
                    0.
                };
                footer
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(200.))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.text_muted)
                                    .child(trf(
                                        "Downloading… {0}%",
                                        &[&((fraction * 100.).floor() as u32)],
                                    )),
                            )
                            // The download comes in steps: the bar glides from one to the next
                            // (`crate::motion`).
                            .child(
                                div().h(px(4.)).rounded(px(2.)).bg(ui.input_border).child(
                                    div()
                                        .h_full()
                                        .rounded(px(2.))
                                        .bg(ui.accent)
                                        .with_transition(
                                            "release-notes-download-fill",
                                            fraction,
                                            crate::motion::SMOOTH,
                                            |fill, fraction| fill.w(relative(fraction)),
                                        ),
                                ),
                            ),
                    )
                    .child(
                        ui::text_button("release-notes-cancel", tr("Cancel"), false, ui)
                            .on_click(|_, _, cx| updates::cancel_update(cx)),
                    )
            }
            Phase::Preparing { .. } => footer.child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(tr("Checking the update…")),
            ),
            Phase::Ready(_) => footer
                .child(
                    ui::text_button("release-notes-later", tr("Later"), false, ui)
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(DismissEvent))),
                )
                .child(
                    ui::primary_button("release-notes-restart", tr("Restart Now"), true, ui)
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                            cx.emit(DismissEvent);
                            cx.defer(updates::restart_to_update);
                        })),
                ),
            Phase::Idle => footer
                .child(
                    ui::text_button("release-notes-skip", tr("Skip This Version"), false, ui)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.skip(cx))),
                )
                .child(
                    ui::text_button("release-notes-later", tr("Later"), false, ui)
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(DismissEvent))),
                )
                .child(
                    ui::primary_button("release-notes-install", tr("Update and Restart"), true, ui)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.install(cx))),
                ),
        }
    }
}

impl EventEmitter<DismissEvent> for ReleaseNotes {}

impl Focusable for ReleaseNotes {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ReleaseNotes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let channel = Channel::of(&self.manifest.version);
        let current = flux_update::current_version();
        let tile = div()
            .flex_none()
            .size(px(TILE))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(ui::RADIUS_LG))
            .bg(UiColors::tint(ui.accent, 0.16))
            .border_1()
            .border_color(UiColors::tint(ui.accent, 0.3))
            .child(icon(IconName::Logo, ui.accent_text).size(px(24.)));
        let header = div()
            .flex_none()
            .px_4()
            .pt_4()
            .pb_3()
            .flex()
            .items_start()
            .gap_3()
            .border_b_1()
            .border_color(ui.divider)
            .child(tile)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_XL))
                                    .font_weight(FontWeight::BOLD)
                                    .child(trf("Flux {0} is available", &[&self.manifest.version])),
                            )
                            .child(ui::badge(
                                channel_label(channel),
                                channel_color(channel, ui),
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(trf(
                                "Released {0} · you have {1}",
                                &[&updates::release_date(&self.manifest.date), &current],
                            )),
                    ),
            )
            .child(
                ui::icon_button("release-notes-close", IconName::Close, ui)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(DismissEvent))),
            );
        let notes = div()
            .id("release-notes-text")
            .flex_shrink()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .child(markdown::render(
                &self.blocks,
                ui.foreground,
                Theme::get(cx),
            ));
        let footer = self.render_footer(ui, cx);
        ui::popover(ui)
            .key_context("ReleaseNotes")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Confirm, _, cx| {
                if matches!(this.phase(cx), Phase::Idle) {
                    this.install(cx)
                }
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .w(px(WIDTH))
            .max_h(px(ui::dialog_height(HEIGHT, window)))
            .flex()
            .flex_col()
            .child(header)
            .child(notes)
            .child(footer)
    }
}
