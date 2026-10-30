//! The country check in sight (part «Флаг страны», wiki ADR-041): the flag in the status bar and
//! its popover, the notifications of a block and of its end. The guard itself —
//! [`crate::region_guard`]; the console's banner ([`crate::claude_console`]) and Settings → Claude
//! Code → Country Check ([`crate::settings_view`]) use the marks and words of this module.
//!
//! The status bar's item is a pill at its right end: the flag and the country's code, muted while
//! Claude may go, red while it is blocked (a warning sign and "?" when the country can't be told),
//! a pulsing globe while Flux asks. Its tooltip says where, when it was asked and what that means
//! for Claude. A press opens the popover above it, as the language servers' item does: the big
//! flag and the country's name, the address, what it means for Claude, when it was checked, Check
//! Again (↵) and Settings… (⌘,); Esc closes. A new answer (Check Again) fades its words in, the
//! state's color glides to its new tone and the popover's height to its new text
//! (`crate::motion`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Action, Animation, AnimationExt as _, AnyElement, App, Bounds, Context, Corner, DismissEvent,
    EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton, Pixels,
    Render, Subscription, Task, Window, actions, div, point, prelude::*, px,
};

use crate::claude;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::lsp_status::{Crossfade, key_of};
use crate::motion::{self, TransitionExt as _};
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::region_guard::{self, CheckCountry, Refusal, RegionEvent, Verdict};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Workspace;

actions!(region_popover, [Dismiss]);

const CONTEXT: &str = "RegionPopover";
const WIDTH: f32 = 320.;
/// Between the status item and the popover above it.
const ANCHOR_GAP: f32 = 6.;
/// The popover's flag, and the sign in its place while the country isn't known.
const FLAG_SIZE: f32 = 30.;
/// The status item: its height and the flag's size in it.
const ITEM_HEIGHT: f32 = 20.;
const ITEM_FLAG: f32 = 14.;
/// "Checked 5 s ago" counts on while the popover is open.
const TICK: Duration = Duration::from_secs(1);
/// One breath of the pulse while Flux asks.
const PULSE: Duration = Duration::from_millis(1100);

pub fn init(cx: &mut App) {
    let popover = Some(CONTEXT);
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("escape", Dismiss, popover),
            KeyBinding::new("enter", CheckCountry, popover),
            KeyBinding::new("cmd-,", claude::OpenSettings, popover),
        ],
    );
}

// --- Words and marks ---

/// How long ago: "just now", "12 s ago", "3 min ago", "2 h ago".
pub(crate) fn ago(at: Instant) -> String {
    let seconds = at.elapsed().as_secs();
    match seconds {
        0..5 => tr("just now").to_string(),
        5..60 => trf("{0} s ago", &[&seconds]),
        60..3600 => trf("{0} min ago", &[&(seconds / 60)]),
        _ => trf("{0} h ago", &[&(seconds / 3600)]),
    }
}

/// "Checked 12 s ago" — `None` before the first answer.
pub(crate) fn checked(verdict: &Verdict) -> Option<String> {
    verdict
        .checked_at()
        .map(|at| trf("Checked {0}", &[&ago(at)]))
}

/// What the verdict means for Claude, in a few words.
pub(crate) fn state(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Off => tr("The country check is off"),
        Verdict::Checking => tr("Checking the country…"),
        Verdict::Allowed(_) => tr("Claude's requests are allowed"),
        Verdict::Blocked(_) | Verdict::Unknown(_) => tr("Claude is blocked"),
    }
}

/// Why, under [`state`]: a sentence or two.
pub(crate) fn detail(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Off => tr("Claude's requests go without a check.").to_string(),
        Verdict::Checking => {
            tr("Claude waits for the answer: nothing new starts or is sent meanwhile.").to_string()
        }
        Verdict::Allowed(check) => trf(
            "{0} isn't on your list of blocked countries.",
            &[&country_title(&check.location.country)],
        ),
        Verdict::Blocked(check) => trf(
            "{0} is on your list of blocked countries. Flux stopped Claude and sends nothing until the country changes.",
            &[&country_title(&check.location.country)],
        ),
        Verdict::Unknown(_) => tr(
            "Flux couldn't find out the country of the IP address: the network may be down. Nothing goes to Claude until the country is known.",
        )
        .to_string(),
    }
}

/// The country's name ("Finland"), or its code when the system has none.
fn country_title(code: &str) -> String {
    region_guard::country_name(code).unwrap_or_else(|| code.to_string())
}

/// "Finland (FI) · 203.0.113.7".
pub(crate) fn place(verdict: &Verdict) -> Option<String> {
    let location = verdict.location()?;
    let label = region_guard::country_label(&location.country);
    Some(match &location.ip {
        Some(ip) => format!("{label} · {ip}"),
        None => label,
    })
}

/// The tooltip of a flag: where, when it was asked, what it means for Claude.
pub(crate) fn tooltip_text(verdict: &Verdict) -> String {
    let first = match verdict {
        Verdict::Unknown(_) => tr("The country of the IP address is unknown").to_string(),
        Verdict::Checking => tr("Checking the country of the IP address…").to_string(),
        _ => place(verdict).unwrap_or_default(),
    };
    let meaning = match verdict {
        Verdict::Blocked(_) => tr("Claude is blocked: the country is on your list").to_string(),
        Verdict::Unknown(_) => tr("Claude is blocked until the country is known").to_string(),
        Verdict::Checking => tr("Claude waits for the answer").to_string(),
        other => state(other).to_string(),
    };
    [Some(first), checked(verdict), Some(meaning)]
        .into_iter()
        .flatten()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The color of the verdict's words: red while Claude is blocked.
pub(crate) fn tone(verdict: &Verdict, ui: UiColors) -> Hsla {
    match verdict {
        Verdict::Blocked(_) | Verdict::Unknown(_) => ui.error,
        Verdict::Allowed(_) => ui.success,
        Verdict::Off | Verdict::Checking => ui.dim,
    }
}

/// The verdict's mark, `size` points: the country's flag, a warning sign when it couldn't be told,
/// a pulsing globe while Flux asks (`id` — the pulse's). `None` — the check is off.
pub(crate) fn mark(
    verdict: &Verdict,
    size: f32,
    id: &'static str,
    ui: UiColors,
) -> Option<AnyElement> {
    match verdict {
        Verdict::Off => None,
        Verdict::Checking => Some(pulse(id, icon(IconName::Globe, ui.dim).size(px(size)))),
        Verdict::Allowed(check) | Verdict::Blocked(check) => {
            Some(flag(&check.location.country, size))
        }
        Verdict::Unknown(_) => Some(
            icon(IconName::Warning, ui.error)
                .size(px(size))
                .into_any_element(),
        ),
    }
}

/// A country's flag as an emoji of `size` points.
pub(crate) fn flag(code: &str, size: f32) -> AnyElement {
    div()
        .flex_none()
        .text_size(px(size))
        .line_height(px(size + 2.))
        .child(region_guard::flag(code))
        .into_any_element()
}

/// A slow pulse of `element` while it is drawn (still when motion is off).
pub(crate) fn pulse<E: IntoElement + Styled + 'static>(id: &'static str, element: E) -> AnyElement {
    if !crate::motion::enabled() {
        return element.into_any_element();
    }
    element
        .with_animation(id, Animation::new(PULSE).repeat(), |element, delta| {
            element.opacity(0.35 + 0.65 * (1. - (2. * delta - 1.).abs()))
        })
        .into_any_element()
}

// --- The status item ---

/// The status bar's item: the flag and the country's code, the check's state; `None` — the check
/// is off.
pub fn status_item(cx: &App) -> Option<AnyElement> {
    let verdict = region_guard::verdict(cx);
    if verdict == Verdict::Off {
        return None;
    }
    let ui = Theme::ui(cx);
    let blocks = verdict.blocks();
    let color = if blocks { ui.error } else { ui.text_muted };
    let code = match &verdict {
        Verdict::Allowed(check) | Verdict::Blocked(check) => check.location.country.clone(),
        Verdict::Unknown(_) => "?".to_string(),
        Verdict::Off | Verdict::Checking => "…".to_string(),
    };
    let size = match verdict {
        Verdict::Allowed(_) | Verdict::Blocked(_) => ITEM_FLAG,
        _ => 13.,
    };
    let hover = if blocks {
        UiColors::tint(ui.error, 0.24)
    } else {
        ui.hover
    };
    // The popover's own "press outside" closes it first: the capture phase sees it still open, and
    // the press only closes it.
    let was_open = Rc::new(Cell::new(false));
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let item = div()
        .id("region-status")
        .flex_none()
        .h(px(ITEM_HEIGHT))
        .px(px(7.))
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(px(ITEM_HEIGHT / 2.))
        .when(blocks, |item| {
            item.bg(UiColors::tint(ui.error, 0.14))
                .font_weight(FontWeight::MEDIUM)
        })
        .text_color(color)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .tooltip(|window, cx| {
            ui::tooltip(tooltip_text(&region_guard::verdict(cx)), None)(window, cx)
        })
        .capture_any_mouse_down({
            let was_open = was_open.clone();
            move |_, window, cx| was_open.set(popover_open(window, cx))
        })
        .on_mouse_down(MouseButton::Left, {
            let bounds = bounds.clone();
            move |_, window, cx| {
                cx.stop_propagation();
                if !was_open.get() {
                    open_popover(bounds.get(), window, cx);
                }
            }
        })
        .children(mark(&verdict, size, "region-status-pulse", ui))
        .child(code);
    Some(
        div()
            .flex_none()
            .on_children_prepainted(move |children, _, _| bounds.set(children.first().copied()))
            .child(item)
            .into_any_element(),
    )
}

/// The window's popover is the country's.
fn popover_open(window: &Window, cx: &App) -> bool {
    window
        .root::<Workspace>()
        .flatten()
        .is_some_and(|workspace| workspace.read(cx).modal_is::<RegionPopover>())
}

/// Opens the popover above the status item (`item` — where it was drawn), its right edge at the
/// item's.
fn open_popover(item: Option<Bounds<Pixels>>, window: &mut Window, cx: &mut App) {
    let Some(workspace) = window.root::<Workspace>().flatten() else {
        return;
    };
    workspace.update(cx, |workspace, cx| {
        workspace.toggle_modal(window, cx, RegionPopover::new);
        match item {
            Some(item) => workspace.anchor_modal_at(
                point(item.right(), item.top() - px(ANCHOR_GAP)),
                Corner::BottomRight,
            ),
            None => workspace.anchor_modal(crate::popup::window_middle(window, WIDTH)),
        }
    });
}

// --- The popover ---

pub struct RegionPopover {
    focus_handle: FocusHandle,
    /// The country and the address, and what the answer means: each fades in anew as it changes.
    identity: Crossfade,
    meaning: Crossfade,
    _subscription: Option<Subscription>,
    _tick: Task<()>,
}

impl EventEmitter<DismissEvent> for RegionPopover {}

impl Focusable for RegionPopover {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RegionPopover {
    fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription =
            region_guard::global(cx).map(|guard| cx.observe(&guard, |_, _, cx| cx.notify()));
        // An answer that only refreshes the time doesn't notify: the clock redraws it.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            identity: Crossfade::default(),
            meaning: Crossfade::default(),
            _subscription: subscription,
            _tick: tick,
        }
    }

    fn check_again(&mut self, cx: &mut Context<Self>) {
        region_guard::check_again(cx);
        cx.notify();
    }
}

impl Render for RegionPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let verdict = region_guard::verdict(cx);
        let asking = region_guard::global(cx).is_some_and(|guard| guard.read(cx).is_checking());
        let (title, subtitle) = match &verdict {
            Verdict::Allowed(check) | Verdict::Blocked(check) => {
                let location = &check.location;
                let mut subtitle = location.country.clone();
                if let Some(ip) = &location.ip {
                    subtitle = format!("{subtitle} · {ip}");
                }
                (country_title(&location.country), subtitle)
            }
            Verdict::Unknown(_) => (
                tr("Country unknown").to_string(),
                tr("The check got no answer").to_string(),
            ),
            Verdict::Checking => (
                tr("Checking…").to_string(),
                tr("Asking a neutral service where the network sees this Mac").to_string(),
            ),
            Verdict::Off => (
                tr("Country Check").to_string(),
                tr("Off in Settings").to_string(),
            ),
        };
        let identity = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .size(px(FLAG_SIZE + 10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(mark(&verdict, FLAG_SIZE, "region-popover-pulse", ui)),
            )
            .child(
                self.identity.apply(
                    "region-identity",
                    key_of((&title, &subtitle)),
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_LG))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.dim)
                                .child(subtitle),
                        ),
                ),
            );
        let state_icon = match &verdict {
            Verdict::Allowed(_) => icon(IconName::CheckCircle, ui.success),
            Verdict::Blocked(_) | Verdict::Unknown(_) => icon(IconName::Error, ui.error),
            Verdict::Checking => icon(IconName::Clock, ui.dim),
            Verdict::Off => icon(IconName::Info, ui.dim),
        };
        let when = match (checked(&verdict), asking) {
            (_, true) if verdict != Verdict::Checking => Some(tr("Checking now…").to_string()),
            (checked, _) => checked,
        };
        let meaning_key = key_of((state(&verdict), detail(&verdict)));
        let meaning = div()
            .flex()
            .gap_2()
            .child(
                div()
                    .flex_none()
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .child(state_icon.size(px(14.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    // Its color comes from around the fade, gliding to the new tone.
                    .child(div().font_weight(FontWeight::MEDIUM).child(state(&verdict)))
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(detail(&verdict)),
                    )
                    .children(when.map(|when| {
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(when)
                    })),
            );
        // While Claude is blocked, asking again is the way out: the main button.
        let check_label = if asking {
            tr("Checking…")
        } else {
            tr("Check Again")
        };
        let check_keys = ui::shortcut_in(&CheckCountry, &self.focus_handle, window, cx);
        let check = if verdict.blocks() {
            ui::primary_button("region-check-again", check_label, !asking, ui)
        } else {
            ui::text_button("region-check-again", check_label, false, ui)
                .when(asking, |button| button.opacity(0.6))
        }
        .tooltip(ui::tooltip(
            tr("Ask the country of the IP address again"),
            check_keys,
        ))
        .when(!asking, |button| {
            button.on_click(cx.listener(|this, _, _, cx| this.check_again(cx)))
        });
        let settings_keys = ui::shortcut_in(&claude::OpenSettings, &self.focus_handle, window, cx);
        let settings = ui::text_button("region-settings", tr("Settings…"), false, ui)
            .tooltip(ui::tooltip(tr("Settings → Claude Code"), settings_keys))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(claude::OpenSettings), cx));
        crate::popup::panel(ui)
            .id("region-popover")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_action(cx.listener(|this, _: &CheckCountry, _, cx| this.check_again(cx)))
            .child(div().px_4().pt_4().pb_3().child(identity))
            .child(ui::divider(ui).mx_4())
            .child(motion::glide_height(
                "region-meaning-height",
                div().px_4().py_3().child(
                    div()
                        .child(self.meaning.apply("region-meaning", meaning_key, meaning))
                        .with_transition(
                            "region-tone",
                            tone(&verdict, ui),
                            motion::FADE,
                            |meaning, color| meaning.text_color(color),
                        ),
                ),
            ))
            .child(
                div()
                    .px_4()
                    .pb_4()
                    .pt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(check)
                    .child(settings),
            )
    }
}

// --- Notifications ---

/// The guard told something: a block or its end — a notification in this window.
pub fn on_event(workspace: &mut Workspace, event: &RegionEvent, cx: &mut Context<Workspace>) {
    // The cards of an earlier block (and of what it refused) give way to the news.
    let earlier = [CheckCountry.name()];
    match event {
        RegionEvent::Changed => {}
        RegionEvent::Blocked(refusal) => {
            workspace.dismiss_notifications(&earlier, cx);
            workspace.notify(stopped(refusal), cx);
        }
        RegionEvent::Unblocked => {
            workspace.dismiss_notifications(&earlier, cx);
            let verdict = region_guard::verdict(cx);
            let title = match verdict.location() {
                Some(location) => trf(
                    "Claude is available again · {0}",
                    &[&region_guard::country_label(&location.country)],
                ),
                None => tr("Claude is available again").to_string(),
            };
            workspace.notify(
                Notification::info(title)
                    .group(NotificationGroup::Claude)
                    .transient(),
                cx,
            );
        }
    }
}

/// Claude became blocked and everything of it stopped: why, Check Again and the settings.
fn stopped(refusal: &Refusal) -> Notification {
    let until = match refusal.country {
        Some(_) => tr("Nothing goes to Claude until the country changes."),
        None => tr("Nothing goes to Claude until the country is known."),
    };
    Notification::error(tr("Flux stopped Claude"))
        .body(format!("{} {until}", refusal.reason()))
        .action(tr("Check Again"), CheckCountry)
        .action(tr("Settings…"), claude::OpenSettings)
        .group(NotificationGroup::Claude)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_claude::region::Location;
    use region_guard::{Check, Failure};

    fn check(country: &str, ip: Option<&str>) -> Check {
        Check {
            location: Location {
                country: country.into(),
                ip: ip.map(str::to_string),
            },
            at: Instant::now(),
        }
    }

    #[test]
    fn the_tooltip_says_where_when_and_what_for_claude() {
        let allowed = Verdict::Allowed(check("FI", Some("203.0.113.7")));
        assert_eq!(
            tooltip_text(&allowed),
            "Finland (FI) · 203.0.113.7\nChecked just now\nClaude's requests are allowed"
        );
        let blocked = Verdict::Blocked(check("RU", None));
        assert_eq!(
            tooltip_text(&blocked),
            "Russia (RU)\nChecked just now\nClaude is blocked: the country is on your list"
        );
        let unknown = Verdict::Unknown(Failure {
            error: "offline".into(),
            at: Instant::now(),
        });
        assert!(tooltip_text(&unknown).ends_with("Claude is blocked until the country is known"));
        assert_eq!(
            tooltip_text(&Verdict::Checking),
            "Checking the country of the IP address…\nClaude waits for the answer"
        );
    }

    #[test]
    fn times_read_as_ago() {
        let now = Instant::now();
        assert_eq!(ago(now), "just now");
        if let Some(earlier) = now.checked_sub(Duration::from_secs(42)) {
            assert_eq!(ago(earlier), "42 s ago");
        }
        if let Some(earlier) = now.checked_sub(Duration::from_secs(185)) {
            assert_eq!(ago(earlier), "3 min ago");
        }
    }

    #[test]
    fn a_block_tells_why_and_offers_a_way_out() {
        let card = stopped(&Refusal {
            country: Some("RU".into()),
        });
        assert!(card.offers(&["region::CheckCountry"]));
        assert!(card.offers(&["claude::OpenSettings"]));
    }
}
