//! Claude Code (the guide's fourth slide): is `claude` there and signed in; how to install it; the
//! sign-in right in the guide (`claude auth login` in a terminal drawn on the slide); Claude Code
//! in Flux on or off.
//!
//! The slide asks the same questions Flux asks for its windows ([`claude::check_cli`], in the
//! background): found, its version, the account. It asks again when the guide's window becomes
//! active (Claude Code installed in another terminal meanwhile), when the sign-in's terminal ends,
//! and every few seconds while a sign-in is awaited, as the console does. Once it is ready
//! after a sign-in or an install, every Flux window checks again too: its chat works without a
//! restart.

use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Context, Div, Entity,
    Focusable as _, FontWeight, Hsla, KeyBinding, NoAction, Render, SharedString, Subscription,
    Task, Window, WindowHandle, div, prelude::*, px,
};

use super::{CONTEXT, WelcomeGuide, heading};
use crate::claude::{self, CliState};
use crate::code_actions::ShowContextActions;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::settings;
use crate::settings_view::{self, Section};
use crate::start_screen::shorten_path;
use crate::terminal_group::ClosePane;
use crate::terminal_view::{SpawnSpec, TerminalLink, TerminalView, TerminalViewEvent};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};
use crate::workspace::{Workspace, tilde};

/// The right column: the card about `claude`, the sign-in's terminal, the switch.
const CARD_WIDTH: f32 = 380.;
/// The terminal `claude auth login` runs in: a few lines of its output.
const TERMINAL_HEIGHT: f32 = 140.;
/// The card's icon tile.
const TILE: f32 = 34.;
/// The label column of the card's facts (Executable, Version, Account).
const FACT_LABEL_WIDTH: f32 = 136.;
/// The executable's path, shortened in the middle to fit the card.
const PATH_CHARS: usize = 30;
/// How often and how long the sign-in is checked after Sign In, as the console does.
const SIGN_IN_POLL: Duration = Duration::from_secs(2);
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
/// Copy says «Copied» this long.
const COPIED_FOR: Duration = Duration::from_millis(1500);

/// The installer the slide offers: the official one (`claude::INSTALL_COMMANDS`).
fn install_command() -> &'static str {
    claude::INSTALL_COMMANDS[0].1
}

pub struct ClaudeStep {
    /// What the last check found; `Checking` only until the first answer — later checks keep the
    /// card as it is until they answer.
    state: CliState,
    /// A check has answered: a later Ready is news (a sign-in, an install) the windows hear of.
    answered: bool,
    /// `claude auth login` in a terminal on the slide, after Sign In.
    terminal: Option<Entity<TerminalView>>,
    /// A sign-in is awaited: the card says so and the check repeats.
    signing_in: bool,
    /// Signed in: the terminal goes at the next frame, giving the keyboard back to the guide.
    terminal_done: bool,
    /// When Copy was clicked: it says «Copied» for a moment.
    copied: Option<Instant>,
    _check: Option<Task<()>>,
    _sign_in: Option<Task<()>>,
    _terminal_events: Option<Subscription>,
    /// Sign In didn't run: the country check blocks Claude (part «Флаг страны»).
    region_refusal: Option<crate::region_guard::Refusal>,
    _subscriptions: Vec<Subscription>,
}

impl ClaudeStep {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        bind_keys(cx);
        // Installed or signed in from another terminal meanwhile: coming back to the guide shows it.
        let subscriptions = vec![cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && !matches!(this.state, CliState::Ready { .. }) {
                this.check(cx);
            }
        })];
        let mut step = Self {
            state: CliState::Checking,
            answered: false,
            terminal: None,
            signing_in: false,
            terminal_done: false,
            copied: None,
            _check: None,
            _sign_in: None,
            _terminal_events: None,
            region_refusal: None,
            _subscriptions: subscriptions,
        };
        step.check(cx);
        step
    }

    /// Asks again where `claude` is and whether it is signed in (in the background).
    fn check(&mut self, cx: &mut Context<Self>) {
        let preferred = settings::claude(cx).path;
        let check = cx.background_spawn(async move { claude::check_cli(preferred.as_deref()) });
        self._check = Some(cx.spawn(async move |this, cx| {
            let state = check.await;
            this.update(cx, |this, cx| this.set_state(state, cx)).ok();
        }));
    }

    fn set_state(&mut self, state: CliState, cx: &mut Context<Self>) {
        let ready = matches!(state, CliState::Ready { .. });
        let news = ready && self.answered && !matches!(self.state, CliState::Ready { .. });
        self.answered = true;
        if ready {
            // Signed in: the terminal has done its work (it goes when the slide is drawn next,
            // where the window is at hand to move the keyboard back to the guide).
            self.signing_in = false;
            self.terminal_done = self.terminal.is_some();
            self._sign_in = None;
        }
        if self.state != state {
            self.state = state;
            cx.notify();
        }
        if news {
            cx.defer(check_every_window);
        }
    }

    /// Sign In: `claude auth login` in a terminal on the slide — it opens the browser, and the user
    /// can answer it there if it asks — and the sign-in is checked until it is there.
    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cli) = self.state.cli().cloned() else {
            return self.check(cx);
        };
        // Part «Флаг страны»: signing in talks to Anthropic too.
        match crate::region_guard::gate(true, cx) {
            crate::region_guard::Gate::Go => self.region_refusal = None,
            crate::region_guard::Gate::Wait => {
                let this = cx.weak_entity();
                let handle = window.window_handle();
                return crate::region_guard::wait(cx, move |outcome, cx| {
                    handle
                        .update(cx, |_, window, cx| {
                            this.update(cx, |this, cx| match outcome {
                                Ok(()) => this.sign_in(window, cx),
                                Err(refusal) => {
                                    this.region_refusal = Some(refusal);
                                    cx.notify();
                                }
                            })
                        })
                        .ok();
                });
            }
            crate::region_guard::Gate::Refuse(refusal) => {
                self.region_refusal = Some(refusal);
                return cx.notify();
            }
        }
        let (program, args) = cli.login_command();
        let spec = SpawnSpec {
            command: Some((program.to_string_lossy().into_owned(), args)),
            cwd: std::env::var_os("HOME").map(PathBuf::from),
            title: Some("claude auth login".into()),
            keep_on_exit: true,
            ..SpawnSpec::default()
        };
        let terminal = match TerminalView::spawn_with(spec, None, window, cx) {
            Ok(terminal) => terminal,
            Err(error) => {
                self.state = CliState::Failed(error.to_string());
                cx.notify();
                return;
            }
        };
        self._terminal_events = Some(cx.subscribe(&terminal, |this, _, event, cx| match event {
            // The command is over (signed in, or given up): the card follows at once, and Sign In
            // is offered again if it didn't sign in.
            TerminalViewEvent::ProcessEnded(_) => {
                this.signing_in = false;
                this._sign_in = None;
                this.check(cx);
            }
            // ⌘-click on the sign-in address it prints.
            TerminalViewEvent::OpenLink(TerminalLink::Url(url)) => cx.open_url(url),
            _ => {}
        }));
        window.focus(&terminal.read(cx).focus_handle);
        self.terminal = Some(terminal);
        self.signing_in = true;
        self.watch_sign_in(cx);
        cx.notify();
    }

    /// The sign-in's terminal goes (signed in, or ⌘W — the close its note offers); the keyboard it
    /// had goes back to the guide (a focused element that isn't drawn cuts the guide's keys off).
    fn close_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_done = false;
        let Some(terminal) = self.terminal.take() else {
            return;
        };
        self._terminal_events = None;
        self.signing_in = false;
        self._sign_in = None;
        if terminal.read(cx).focus_handle.contains_focused(window, cx)
            && let Some(Some(guide)) = window.root::<WelcomeGuide>()
        {
            let handle = guide.read(cx).focus_handle(cx);
            window.focus(&handle);
        }
        cx.notify();
    }

    /// Checks the sign-in every few seconds until it is there, or for a few minutes.
    fn watch_sign_in(&mut self, cx: &mut Context<Self>) {
        let preferred = settings::claude(cx).path;
        self._sign_in = Some(cx.spawn(async move |this, cx| {
            let started = SystemTime::now();
            loop {
                cx.background_executor().timer(SIGN_IN_POLL).await;
                let preferred = preferred.clone();
                let state = cx
                    .background_spawn(async move { claude::check_cli(preferred.as_deref()) })
                    .await;
                let ready = matches!(state, CliState::Ready { .. });
                if ready
                    && this
                        .update(cx, |this, cx| this.set_state(state, cx))
                        .is_err()
                {
                    break;
                }
                let timed_out = started.elapsed().map_or(true, |spent| spent > SIGN_IN_WAIT);
                if ready || timed_out {
                    break;
                }
            }
        }));
    }

    fn copy_install_command(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(install_command().to_string()));
        self.copied = Some(Instant::now());
        cx.notify();
        // «Copied» goes back to «Copy».
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    // --- The card ---

    fn render_card(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let card = div()
            .flex()
            .flex_col()
            .px_4()
            .py(px(14.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(ui.island_border)
            .bg(ui.hover);
        if let Some(refusal) = &self.region_refusal {
            return card
                .child(head(
                    IconName::Warning,
                    ui.error,
                    tr("Claude is blocked").into(),
                    refusal.reason().into(),
                    ui,
                ))
                .child(
                    actions().child(
                        ui::text_button("welcome-claude-region", tr("Check Again"), false, ui)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.region_refusal = None;
                                crate::region_guard::check_again(cx);
                                this.sign_in(window, cx);
                            })),
                    ),
                );
        }
        match &self.state {
            CliState::Checking => card.child(
                div()
                    .h(px(TILE))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(busy_dot("welcome-claude-checking", ui))
                    .child(
                        div()
                            .text_color(ui.text_muted)
                            .child(tr("Looking for Claude Code…")),
                    ),
            ),
            CliState::Ready {
                cli,
                version,
                account,
            } => {
                let keys = ui::shortcut_for(&claude::ToggleClaude, cx).unwrap_or_default();
                let account = account
                    .clone()
                    .unwrap_or_else(|| tr("Signed in").to_string());
                card.child(head(
                    IconName::CheckCircle,
                    ui.success,
                    tr("Claude Code is ready").into(),
                    trf(
                        "The chat opens with {0} — on the right or as an editor tab",
                        &[&keys],
                    )
                    .into(),
                    ui,
                ))
                .child(facts(
                    vec![
                        (
                            tr("Executable"),
                            mono(shorten_path(&tilde(&cli.path), PATH_CHARS), ui),
                        ),
                        (tr("Version"), text(version.clone())),
                        (tr("Account"), text(account)),
                    ],
                    ui,
                ))
                .when(front_workspace(cx).is_some(), |card| {
                    card.child(
                        actions().child(
                            ui::text_button("welcome-claude-settings", tr("Settings…"), false, ui)
                                .on_click(|_, _, cx| open_claude_settings(cx)),
                        ),
                    )
                })
            }
            CliState::SignedOut { cli, version } => {
                let waiting = self.signing_in;
                card.child(head(
                    IconName::Warning,
                    ui.warning,
                    tr("Sign in to Claude").into(),
                    tr("With your Claude subscription: the sign-in opens the browser").into(),
                    ui,
                ))
                .child(facts(
                    vec![
                        (
                            tr("Executable"),
                            mono(shorten_path(&tilde(&cli.path), PATH_CHARS), ui),
                        ),
                        (
                            tr("Version"),
                            text(version.clone().unwrap_or_else(|| "—".to_string())),
                        ),
                        (
                            tr("Account"),
                            div()
                                .text_color(ui.warning)
                                .child(tr("Signed out"))
                                .into_any_element(),
                        ),
                    ],
                    ui,
                ))
                .child(
                    actions()
                        .when(!waiting, |row| {
                            row.child(
                                ui::primary_button(
                                    "welcome-claude-sign-in",
                                    tr("Sign In"),
                                    true,
                                    ui,
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.sign_in(window, cx)),
                                ),
                            )
                        })
                        .child(
                            ui::text_button("welcome-claude-check", tr("Check Again"), false, ui)
                                .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                        ),
                )
            }
            CliState::Missing => {
                let copied = self.copied.is_some_and(|at| at.elapsed() < COPIED_FOR);
                card.child(head(
                    IconName::Download,
                    ui.text_muted,
                    tr("Claude Code isn't installed").into(),
                    tr("Install it in a terminal — Flux finds it by itself").into(),
                    ui,
                ))
                // The whole command, as it is to be typed (it may wrap: it is long for the card).
                .child(
                    div()
                        .mt_3()
                        .px_3()
                        .py_2()
                        .rounded(px(RADIUS_MD))
                        .bg(ui.input_background)
                        .border_1()
                        .border_color(ui.input_border)
                        .font_family(theme::code_font())
                        .text_size(px(theme::TEXT_XS))
                        .line_height(px(17.))
                        .child(install_command()),
                )
                .child(
                    actions()
                        .child(
                            ui::text_button(
                                "welcome-claude-copy",
                                if copied { tr("Copied") } else { tr("Copy") },
                                false,
                                ui,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.copy_install_command(cx))),
                        )
                        .child(
                            ui::text_button("welcome-claude-check", tr("Check Again"), false, ui)
                                .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                        ),
                )
            }
            CliState::Failed(error) => card
                .child(head(
                    IconName::Warning,
                    ui.warning,
                    tr("Claude Code doesn't respond").into(),
                    SharedString::default(),
                    ui,
                ))
                .child(
                    div()
                        .mt_3()
                        .p_2()
                        .rounded(px(RADIUS_SM))
                        .bg(ui.input_background)
                        .border_1()
                        .border_color(ui.input_border)
                        .font_family(theme::code_font())
                        .text_size(px(theme::TEXT_XS))
                        .text_color(ui.text_muted)
                        .child(error.clone()),
                )
                .child(
                    actions().child(
                        ui::text_button("welcome-claude-check", tr("Check Again"), false, ui)
                            .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                    ),
                ),
        }
    }

    /// The terminal of the sign-in, under the card: its command, whether the sign-in is awaited.
    fn render_terminal(&self, terminal: &Entity<TerminalView>, ui: UiColors) -> Div {
        div()
            .mt_3()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(ui.island_border)
            .bg(ui.input_background)
            .overflow_hidden()
            .child(
                div()
                    .h(px(30.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(ui.divider)
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.text_muted)
                    .child(icon(IconName::Terminal, ui.text_muted).size(px(13.)))
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::TEXT_XS))
                            .child("claude auth login"),
                    )
                    .child(div().flex_1())
                    .when(self.signing_in, |header| {
                        header
                            .child(busy_dot("welcome-claude-signing-in", ui))
                            .child(tr("Waiting for the sign-in…"))
                    }),
            )
            .child(div().h(px(TERMINAL_HEIGHT)).child(terminal.clone()))
    }
}

impl Render for ClaudeStep {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.terminal_done {
            cx.defer_in(window, |this, window, cx| this.close_terminal(window, cx));
        }
        let ui = Theme::ui(cx);
        let fix = ui::shortcut_for(&ShowContextActions, cx).unwrap_or_default();
        let key_rows = [
            (
                ui::shortcut_for(&claude::ToggleClaude, cx),
                tr("The console, and back to the last chat").to_string(),
            ),
            (
                ui::shortcut_for(&claude::AddSelectionToClaude, cx),
                tr("The selection into the message").to_string(),
            ),
            (
                Some(fix.clone()),
                trf("“{0}” on a problem", &[&tr("Fix with Claude")]),
            ),
        ];
        let left = div()
            .flex_1()
            .min_w_0()
            .child(heading(
                tr("Setup · Claude Code"),
                tr("Claude in Its Own Chat"),
                trf(
                    "Flux works with your claude and your Claude subscription: sessions in the console on the left, each chat in an editor tab, edits as diffs with review and rollback, a fix for a problem with {0}.",
                    &[&fix],
                ),
                ui,
            ))
            .child(
                div()
                    .mt(px(22.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(key_rows.into_iter().map(|(keys, label)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .children(keys.map(|keys| ui::keys(&keys, ui)))
                            .child(label)
                    })),
            );
        let enabled = settings::claude(cx).enabled;
        let switch_row = div()
            .mt_4()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(tr("Enable Claude Code"))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(tr("The Claude console in the launchpad, ⌘Esc and ⌥⌘K.")),
                    ),
            )
            .child(
                ui::switch("welcome-claude-enabled", enabled, ui).on_click(|_, _, cx| {
                    settings::update_claude(cx, |claude| claude.enabled = !claude.enabled);
                    // The windows' launchpads and keys follow at once.
                    cx.refresh_windows();
                }),
            );
        let right = div()
            .w(px(CARD_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .child(self.render_card(cx))
            .children(
                self.terminal
                    .as_ref()
                    .map(|terminal| self.render_terminal(terminal, ui)),
            )
            .child(switch_row);
        div()
            .id("welcome-claude")
            .on_action(
                cx.listener(|this, _: &ClosePane, window, cx| this.close_terminal(window, cx)),
            )
            .size_full()
            .overflow_y_scroll()
            .p(px(32.))
            .flex()
            .gap(px(36.))
            .child(left)
            .child(right)
    }
}

/// The keys the guide takes for itself (→ ← ↵ Esc) go to the sign-in's terminal while it has the
/// keyboard: `claude auth login` may ask for a code and ↵.
fn bind_keys(cx: &mut App) {
    static BOUND: Once = Once::new();
    BOUND.call_once(|| {
        let terminal = format!("{CONTEXT} > Terminal");
        cx.bind_keys(
            ["enter", "left", "right", "escape"]
                .map(|key| KeyBinding::new(key, NoAction, Some(terminal.as_str()))),
        );
    });
}

/// The card's top: a tile with the state's icon, the title and a line under it.
fn head(name: IconName, color: Hsla, title: SharedString, line: SharedString, ui: UiColors) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_none()
                .size(px(TILE))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS_MD))
                .bg(UiColors::tint(color, 0.16))
                .child(icon(name, color).size(px(18.))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .when(!line.is_empty(), |column| {
                    column.child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(line),
                    )
                }),
        )
}

/// The facts under the card's top: a label and its value per row.
fn facts(rows: Vec<(&'static str, AnyElement)>, ui: UiColors) -> Div {
    div()
        .mt_3()
        .pt_1()
        .border_t_1()
        .border_color(ui.divider)
        .flex()
        .flex_col()
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .h(px(30.))
                .flex()
                .items_center()
                .text_size(px(theme::TEXT_SM + 0.5))
                .child(
                    div()
                        .w(px(FACT_LABEL_WIDTH))
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(ui.dim)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().child(value))
        }))
}

fn text(value: String) -> AnyElement {
    div().whitespace_nowrap().child(value).into_any_element()
}

fn mono(value: String, ui: UiColors) -> AnyElement {
    div()
        .whitespace_nowrap()
        .font_family(theme::code_font())
        .text_size(px(theme::TEXT_SM))
        .text_color(ui.foreground)
        .child(value)
        .into_any_element()
}

/// The card's buttons, in a row under it.
fn actions() -> Div {
    div().mt_3().flex().items_center().gap_2()
}

/// A pulsing dot: something is awaited.
fn busy_dot(id: &'static str, ui: UiColors) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(8.))
        .rounded(px(4.))
        .bg(ui.accent)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |dot, delta| dot.opacity(0.3 + 0.7 * (1. - (2. * delta - 1.).abs())),
        )
}

/// The Flux window in front (or the last one opened, when none is in front: a scenario).
pub(super) fn front_workspace(cx: &App) -> Option<WindowHandle<Workspace>> {
    cx.window_stack()
        .unwrap_or_default()
        .into_iter()
        .find_map(|window| window.downcast::<Workspace>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .rev()
                .find_map(|window| window.downcast::<Workspace>())
        })
}

/// Settings → Claude Code in the Flux window in front.
fn open_claude_settings(cx: &mut App) {
    let Some(handle) = front_workspace(cx) else {
        return;
    };
    // The click comes while the guide's window is being updated: the other window after it.
    cx.defer(move |cx| {
        handle
            .update(cx, |workspace, window, cx| {
                window.activate_window();
                settings_view::open(workspace, Section::Claude, window, cx);
            })
            .ok();
    });
}

/// Every Flux window looks for `claude` and its sign-in again: its chat works without a restart.
fn check_every_window(cx: &mut App) {
    for window in cx.windows() {
        if let Some(handle) = window.downcast::<Workspace>() {
            handle
                .update(cx, |workspace, _, cx| {
                    workspace.claude.update(cx, |store, cx| store.check(cx))
                })
                .ok();
        }
    }
}
