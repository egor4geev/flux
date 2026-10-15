//! Settings → Updates (part 10.8; JetBrains IDEs: Appearance & Behavior → System Settings →
//! Updates): this version and its channel, Check Now with its answer right on the page, whether
//! Flux checks by itself, and the channel — Stable or Beta.

use flux_update::Channel;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FontWeight, IntoElement, Render,
    SharedString, Subscription, Window, div, prelude::*, px,
};

use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::settings;
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::updates::{self, Outcome, Phase, Updates};

/// The page, made when Settings first show it.
pub struct UpdatesSettingsPage {
    updates: Option<Entity<Updates>>,
    _subscription: Option<Subscription>,
}

impl UpdatesSettingsPage {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let updates = updates::updates(cx);
        let subscription = updates
            .as_ref()
            .map(|updates| cx.observe(updates, |_, _, cx| cx.notify()));
        Self {
            updates,
            _subscription: subscription,
        }
    }

    fn render_status(&self, cx: &Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let current = flux_update::current_version();
        let channel = Channel::of(&current);
        let identity = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui.foreground)
                    .child(format!("Flux {current}")),
            )
            .child(ui::badge(
                channel_label(channel),
                channel_color(channel, ui),
            ));
        let Some(updates) = &self.updates else {
            return div().p_3().child(identity);
        };
        let updates = updates.read(cx);
        let (line, color, buttons) = self.status_line(updates, ui, cx);
        let checked = match updates.last_check() {
            Some(time) => {
                let now = crate::push_dialog::now_seconds();
                if now - (time as i64) < 60 {
                    tr("Last checked just now").to_string()
                } else {
                    trf(
                        "Last checked {0} ago",
                        &[&crate::push_dialog::age(now, time as i64)],
                    )
                }
            }
            None => tr("Not checked yet").to_string(),
        };
        let busy = updates.checking() || !matches!(updates.phase(), Phase::Idle);
        div()
            .flex()
            .items_center()
            .gap_3()
            .p_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(identity)
                    .child(div().text_color(color).child(line))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(checked),
                    ),
            )
            .child(
                div().flex().items_center().gap_2().children(buttons).child(
                    ui::text_button("updates-check-now", tr("Check Now"), false, ui)
                        .when(busy, |button| button.opacity(0.5))
                        .when(!busy, |button| {
                            button.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(updates) = &this.updates {
                                    updates.update(cx, |updates, cx| updates.check_now(cx));
                                }
                            }))
                        }),
                ),
            )
    }

    /// What the page says about updates now, its color, and the buttons that go with it.
    fn status_line(
        &self,
        updates: &Updates,
        ui: UiColors,
        cx: &Context<Self>,
    ) -> (String, gpui::Hsla, Vec<AnyElement>) {
        match updates.phase() {
            Phase::Downloading {
                version,
                done,
                total,
                ..
            } => {
                let percent = (done * 100).checked_div(*total).unwrap_or(0);
                return (
                    trf("Downloading Flux {0}… {1}%", &[version, &percent]),
                    ui.accent_text,
                    vec![
                        ui::text_button("updates-cancel", tr("Cancel"), false, ui)
                            .on_click(|_, _, cx| updates::cancel_update(cx))
                            .into_any_element(),
                    ],
                );
            }
            Phase::Preparing { version } => {
                return (
                    trf("Checking the download of Flux {0}…", &[version]),
                    ui.accent_text,
                    Vec::new(),
                );
            }
            Phase::Ready(prepared) => {
                return (
                    trf(
                        "Flux {0} is ready: it installs when Flux restarts",
                        &[&prepared.version],
                    ),
                    ui.success,
                    vec![
                        ui::primary_button("updates-restart", tr("Restart Now"), true, ui)
                            .on_click(|_, _, cx| updates::restart_to_update(cx))
                            .into_any_element(),
                    ],
                );
            }
            Phase::Idle => {}
        }
        if updates.checking() {
            return (
                tr("Checking for updates…").to_string(),
                ui.text_muted,
                Vec::new(),
            );
        }
        match updates.outcome() {
            Some(Outcome::Found(manifest)) => {
                let version = manifest.version.clone();
                let mut line = trf("Flux {0} is available", &[&version]);
                if updates.skipped(&version) {
                    line = trf("Flux {0} is available (skipped)", &[&version]);
                }
                let notes = manifest.clone();
                (
                    line,
                    ui.accent_text,
                    vec![
                        ui::text_button("updates-whats-new", tr("What's New"), false, ui)
                            .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                                show_notes(notes.clone(), window, cx)
                            }))
                            .into_any_element(),
                        ui::primary_button("updates-install", tr("Update and Restart"), true, ui)
                            .on_click(|_, _, cx| updates::install_update(cx))
                            .into_any_element(),
                    ],
                )
            }
            Some(Outcome::Latest) => (
                tr("You have the latest version").to_string(),
                ui.success,
                Vec::new(),
            ),
            Some(Outcome::Failed(reason)) => (
                trf("Couldn't check for updates: {0}", &[reason]),
                ui.warning,
                Vec::new(),
            ),
            None => (
                tr("Flux looks for a new version on its own").to_string(),
                ui.text_muted,
                Vec::new(),
            ),
        }
    }
}

/// The release notes in place of the Settings (one dialog at a time).
fn show_notes(manifest: flux_update::Manifest, window: &mut Window, cx: &mut App) {
    let Some(handle) = window
        .window_handle()
        .downcast::<crate::workspace::Workspace>()
    else {
        return;
    };
    cx.defer(move |cx| {
        handle
            .update(cx, |workspace, window, cx| {
                crate::update_notes::open(workspace, manifest, window, cx)
            })
            .ok();
    });
}

pub fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => tr("Stable"),
        Channel::Beta => tr("Beta"),
    }
}

pub fn channel_color(channel: Channel, ui: UiColors) -> gpui::Hsla {
    match channel {
        Channel::Stable => ui.success,
        Channel::Beta => ui.warning,
    }
}

/// A row with a title, a line of detail under it, and a switch on the right.
fn switch_row(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    ui: UiColors,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded(px(ui::RADIUS_MD))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
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
        .child(ui::switch(
            SharedString::from(format!("{id}-switch")),
            on,
            ui,
        ))
}

fn card(ui: UiColors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_1()
        .rounded(px(ui::RADIUS_MD))
        .border_1()
        .border_color(ui.island_border)
}

impl Render for UpdatesSettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let current = settings::updates(cx);
        let automatic = current.check_automatically;
        let channels = [
            (
                Channel::Stable,
                tr("Stable"),
                tr("Released versions, tested in betas first"),
            ),
            (
                Channel::Beta,
                tr("Beta"),
                tr("Earlier builds of the next version: new features sooner, a few rough edges"),
            ),
        ];
        let rows = channels
            .into_iter()
            .enumerate()
            .map(|(index, (channel, title, detail))| {
                let selected = current.channel == channel;
                div()
                    .id(("update-channel", index))
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(ui::RADIUS_MD))
                    .cursor_pointer()
                    .hover(move |style| style.bg(ui.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if settings::updates(cx).channel == channel {
                            return;
                        }
                        settings::update_updates(cx, |updates| updates.channel = channel);
                        if let Some(updates) = &this.updates {
                            updates.update(cx, |updates, cx| updates.channel_changed(cx));
                        }
                        cx.notify();
                    }))
                    .child(div().pt(px(2.)).child(ui::radio(
                        ("update-channel-radio", index),
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
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .text_color(ui.dim)
                                    .child(detail),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(theme::TEXT_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr("Updates")),
            )
            .child(div().text_color(ui.text_muted).child(tr(
                "Flux looks for new versions on GitHub. An update downloads in the background and installs when Flux restarts.",
            )));
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(header)
            .child(card(ui).child(self.render_status(cx)))
            .child(
                card(ui).child(
                    switch_row(
                        "updates-automatic",
                        tr("Check for updates automatically"),
                        tr("A few seconds after Flux starts, then once a day"),
                        automatic,
                        ui,
                    )
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        settings::update_updates(cx, |updates| {
                            updates.check_automatically = !automatic
                        });
                        cx.notify();
                    })),
                ),
            )
            .child(
                card(ui)
                    .child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .child(ui::section_label(tr("Update Channel"), ui)),
                    )
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_1p5()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(icon(IconName::Info, ui.dim).size(px(12.)))
                    .child(div().flex_1().min_w_0().child(tr(
                        "A build of a beta starts on the Beta channel. Switching to Stable waits for the next stable version.",
                    ))),
            )
    }
}
