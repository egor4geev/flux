//! Settings (⌘, or the gear at the bottom of the launchpad): an overlay window with sections on the
//! left. The first section — Language Servers: automatic installation on or off, and every server
//! Flux knows with where it comes from; those Flux installed can be updated and deleted, missing
//! ones installed.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

use flux_lsp::config::{self, ServerConfig};
use flux_lsp::install::{self, Source};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding,
    Render, Task, WeakEntity, Window, actions, div, prelude::*, px,
};

use crate::i18n::{tr, trf};
use crate::icons::{IconName, file_icon, icon};
use crate::lsp::LspStore;
use crate::settings;
use crate::theme::{self, Theme};
use crate::ui;
use crate::workspace::{Workspace, tilde};

actions!(settings, [Toggle, Dismiss]);

const WIDTH: f32 = 760.;
const HEIGHT: f32 = 520.;
const SIDEBAR_WIDTH: f32 = 180.;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", Toggle, Some("Workspace")),
        KeyBinding::new("escape", Dismiss, Some("Settings")),
    ]);
}

/// Opens Settings, or closes them if open (the gear, ⌘,).
pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let store = workspace.lsp.downgrade();
    workspace.toggle_modal(window, cx, move |_, cx| SettingsView::new(store, cx));
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    /// The window's language servers: restarted after an install or update, stopped on delete.
    store: WeakEntity<LspStore>,
    servers: Vec<ServerRow>,
    /// Operations in progress or failed, by server name.
    jobs: HashMap<String, Job>,
    _tasks: Vec<Task<()>>,
}

struct ServerRow {
    config: ServerConfig,
    /// Where it comes from; `None` until the background check is done.
    source: Option<Source>,
    /// Whether Flux can install it here.
    installable: Result<(), String>,
}

#[derive(Debug, Clone)]
enum Job {
    Working {
        what: Operation,
        progress: Option<install::Progress>,
    },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Operation {
    Install,
    Update,
    Delete,
}

impl EventEmitter<DismissEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsView {
    fn new(store: WeakEntity<LspStore>, cx: &mut Context<Self>) -> Self {
        let servers = config::default_servers()
            .into_iter()
            .map(|config| ServerRow {
                config,
                source: None,
                installable: Ok(()),
            })
            .collect();
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            store,
            servers,
            jobs: HashMap::new(),
            _tasks: Vec::new(),
        };
        view.refresh(cx);
        view
    }

    /// Where each server comes from: file checks (and `rustup which`) in the background.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let configs: Vec<ServerConfig> =
            self.servers.iter().map(|row| row.config.clone()).collect();
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    configs
                        .iter()
                        .map(|config| (install::source(config), install::check(config)))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                for (row, (source, installable)) in this.servers.iter_mut().zip(found) {
                    row.source = Some(source);
                    row.installable = installable;
                }
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
    }

    fn set_auto_install(&mut self, on: bool, cx: &mut Context<Self>) {
        settings::set_auto_install_servers(on, cx);
        if on {
            self.store
                .update(cx, |store, cx| store.retry_not_installed(cx))
                .ok();
        }
        cx.notify();
    }

    /// Install, update, or delete in the background; then the window's servers follow.
    fn run(&mut self, index: usize, what: Operation, cx: &mut Context<Self>) {
        let config = self.servers[index].config.clone();
        let name = config.name.clone();
        if matches!(self.jobs.get(&name), Some(Job::Working { .. })) {
            return;
        }
        self.jobs.insert(
            name.clone(),
            Job::Working {
                what,
                progress: None,
            },
        );
        if what == Operation::Delete {
            // Not running from a deleted directory.
            self.store
                .update(cx, |store, cx| store.stop_named(&name, cx))
                .ok();
        }
        let (sender, mut receiver) = mpsc::unbounded();
        let work = cx.background_spawn(async move {
            let progress = move |progress: install::Progress| {
                sender.unbounded_send(progress).ok();
            };
            let cancel = AtomicBool::new(false);
            match what {
                Operation::Install => install::install(&config, &progress, &cancel),
                Operation::Update => install::update(&config, &progress, &cancel),
                Operation::Delete => install::uninstall(&config),
            }
        });
        let task = cx.spawn(async move |this, cx| {
            while let Some(mut progress) = receiver.next().await {
                while let Ok(newer) = receiver.try_recv() {
                    progress = newer;
                }
                let shown = this.update(cx, |this, cx| {
                    if let Some(Job::Working {
                        progress: shown, ..
                    }) = this.jobs.get_mut(&name)
                    {
                        *shown = Some(progress);
                        cx.notify();
                    }
                });
                if shown.is_err() {
                    return;
                }
            }
            let result = work.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.jobs.remove(&name);
                        if what != Operation::Delete {
                            this.store
                                .update(cx, |store, cx| store.restart_named(&name, cx))
                                .ok();
                        }
                    }
                    Err(reason) => {
                        this.jobs.insert(name.clone(), Job::Failed(reason));
                    }
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn render_server(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ui = Theme::ui(cx);
        let row = &self.servers[index];
        let config = &row.config;
        let languages = config
            .extensions
            .iter()
            .map(|extension| format!(".{extension}"))
            .collect::<Vec<_>>()
            .join(" ");
        let file = file_icon(
            &format!("x.{}", config.extensions.first().map_or("", String::as_str)),
            &ui,
        );
        let job = self.jobs.get(&config.name);
        let (status, status_color): (String, _) = match (job, &row.source) {
            (Some(Job::Working { what, progress }), _) => {
                let mut text = match what {
                    Operation::Install => tr("Installing…").to_string(),
                    Operation::Update => tr("Updating…").to_string(),
                    Operation::Delete => tr("Deleting…").to_string(),
                };
                if let Some(fraction) = progress.as_ref().and_then(|progress| progress.fraction) {
                    text = format!("{text} {}%", (fraction.clamp(0., 1.) * 100.).round() as u32);
                }
                (text, ui.accent_text)
            }
            (Some(Job::Failed(reason)), _) => {
                let first = reason.lines().next().unwrap_or_default();
                (trf("Failed: {0}", &[&first]), ui.error)
            }
            (None, None) => (tr("Checking…").to_string(), ui.dim),
            (None, Some(Source::System(path))) => {
                (trf("On this Mac · {0}", &[&tilde(path)]), ui.text_muted)
            }
            (None, Some(Source::Flux { version })) => (
                match version {
                    Some(version) => trf("Installed by Flux · {0}", &[version]),
                    None => tr("Installed by Flux").to_string(),
                },
                ui.success,
            ),
            (None, Some(Source::Missing)) => match &row.installable {
                Ok(()) => (tr("Not installed").to_string(), ui.dim),
                Err(reason) => (
                    trf("Not installed: {0}", &[&crate::lsp::install_reason(reason)]),
                    ui.dim,
                ),
            },
        };
        let idle = !matches!(job, Some(Job::Working { .. }));
        let buttons: Vec<gpui::AnyElement> = match &row.source {
            _ if !idle => Vec::new(),
            Some(Source::Flux { .. }) => vec![
                ui::text_button(("update", index), tr("Update"), false, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Update, cx)),
                    )
                    .into_any_element(),
                ui::text_button(("delete", index), tr("Delete"), true, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Delete, cx)),
                    )
                    .into_any_element(),
            ],
            Some(Source::Missing) if row.installable.is_ok() => vec![
                ui::text_button(("install", index), tr("Install"), false, ui)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.run(index, Operation::Install, cx)),
                    )
                    .into_any_element(),
            ],
            _ => Vec::new(),
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(ui::RADIUS_MD))
            .hover(move |style| style.bg(ui.hover))
            .child(file.render().size(px(16.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(config.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_XS))
                                    .text_color(ui.dim)
                                    .truncate()
                                    .child(languages),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(status_color)
                            .truncate()
                            .child(status),
                    ),
            )
            .child(div().flex_none().flex().gap_1p5().children(buttons))
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let auto = settings::auto_install_servers(cx);
        let servers_dir = dirs_label();
        let rows: Vec<_> = (0..self.servers.len())
            .map(|index| self.render_server(index, cx))
            .collect();
        let section = div()
            .h(px(30.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(ui::RADIUS_SM))
            .bg(ui.list_selected)
            .text_color(ui.foreground)
            .child(icon(IconName::Command, ui.accent_text).size(px(14.)))
            .child(tr("Language Servers"));
        ui::popover(ui)
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex()
            .flex_col()
            .text_size(px(theme::TEXT_MD))
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(ui.divider)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon(IconName::Settings, ui.text_muted).size(px(15.)))
                            .child(tr("Settings")),
                    )
                    .child(
                        ui::icon_button("settings-close", IconName::Close, ui)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_none()
                            .w(px(SIDEBAR_WIDTH))
                            .p_2()
                            .border_r_1()
                            .border_color(ui.divider)
                            .child(section),
                    )
                    .child(
                        div()
                            .id("settings-content")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(theme::TEXT_LG))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(tr("Language Servers")),
                                    )
                                    .child(div().text_color(ui.text_muted).child(tr(
                                        "Flux starts the server for the language of an open file: errors, completion, go to definition. Servers installed on this Mac are used as they are; those Flux installed can be updated or deleted here.",
                                    ))),
                            )
                            .child(
                                div()
                                    .id("auto-install")
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .p_3()
                                    .rounded(px(ui::RADIUS_MD))
                                    .border_1()
                                    .border_color(ui.island_border)
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_auto_install(!auto, cx)
                                    }))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap_0p5()
                                            .child(tr("Install missing servers automatically"))
                                            .child(
                                                div()
                                                    .text_size(px(theme::TEXT_SM))
                                                    .text_color(ui.dim)
                                                    .child(tr(
                                                        "When a file needs a server that isn't on this Mac, Flux downloads it to:",
                                                    ))
                                                    .child(
                                                        div()
                                                            .font_family(theme::code_font())
                                                            .text_size(px(theme::TEXT_XS))
                                                            .child(servers_dir.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(ui::switch("auto-install-switch", auto, ui)),
                            )
                            .child(ui::divider(ui))
                            .child(div().flex().flex_col().gap_0p5().children(rows)),
                    ),
            )
    }
}

/// Where installed servers live, for the description.
fn dirs_label() -> String {
    std::env::var_os("FLUX_SERVERS_DIR")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                std::path::PathBuf::from(home).join("Library/Application Support/flux/servers")
            })
        })
        .map(|dir| tilde(&dir))
        .unwrap_or_default()
}

/// Shown on the launchpad: whether Settings are open in this window.
pub fn is_open(workspace: &Workspace) -> bool {
    workspace.modal_is::<SettingsView>()
}
