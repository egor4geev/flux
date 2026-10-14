//! UI verification scenarios (the `scenario` feature, not part of the regular build).
//!
//! The agent can't press keys in the window: it has no Accessibility permission. So keystrokes are
//! fed from inside the app along the same path as real ones: `Window::dispatch_keystroke` goes
//! through the keymap and key contexts, and printable characters go through the input handler.
//!
//! - `FLUX_SCENARIO` — steps separated by spaces:
//!   - `cmd-s`, `enter`, `space`, `left` — a keystroke in gpui keymap syntax;
//!   - `type:text` — type the text character by character (a space is a separate `space` step);
//!   - `wait:500` — a pause in ms;
//!   - `action:notifications_panel::Toggle` — dispatch an action (one without data) by name on the
//!     focused element, as the command palette does;
//!   - `ui:<plugin>/<window>/<element>` — a click on an element of a plugin's tool window (the
//!     event goes to the plugin as a click would send it: the mouse can't be fed);
//!     `ui-submit:<plugin>/<window>/<element>=<text>` — ↵ in its text field with the text;
//!   - `shot:name` — print `SHOT name` and wait for the window to be captured from outside;
//!   - `open-project:/path` — open a project as a recent project on the start screen opens it
//!     (`OpenProject`: this window or a new one, as the settings say — or asking);
//!   - `window:2` — send the next steps to the second Flux window (in the order of `cx.windows()`).
//!
//!   `END` is printed after the last step. Steps go to the window opened last (or brought forward
//!   by opening its project again), unless `window:N` says otherwise.
//! - `FLUX_ANSWERS=0,1` — dialogs answer by themselves: button numbers in order (the index in
//!   `Dialog::buttons`); when the answers run out, the last button (usually Cancel). Each dialog
//!   prints `PROMPT question -> answer` and a `SHOT prompt-N` marker, and stays on screen with the
//!   chosen button focused for the screenshot.
//! - `FLUX_SCENARIO_BACKGROUND=1` — Flux doesn't make itself the active app ([`background`]), its
//!   window never becomes the key window ([`window_options`]) and lets the mouse through
//!   ([`let_the_mouse_through`]): the keys the user types, their clicks and scrolls keep going to
//!   the app they work in.
//! - `FLUX_SCENARIO_FINDER=1` — without a folder in the arguments, Flux starts as from the Dock or
//!   the Finder (no project from the current directory): it reopens the projects open when it quit
//!   (`FLUX_PROJECTS_FILE`).
//!
//! File picker dialogs (Cmd+O, "Save As") are system panels, and the auto-responder doesn't replace
//! them. The screenshot harness is `scripts/ui-scenario.sh`.
//!
//! The scenario window is above all other windows ([`window_options`]): gpui doesn't redraw a
//! window covered by another app, and the screenshots would come out stale while the author works
//! alongside.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AsyncApp, Keystroke, Modifiers, PromptButton, PromptHandle, PromptLevel,
    RenderablePromptHandle, Window, WindowKind, WindowOptions,
};

/// Before the first step: the window has opened and the files from the command line have been read.
const START_DELAY: Duration = Duration::from_millis(1500);
/// After a step: the window has time to redraw.
const STEP_PAUSE: Duration = Duration::from_millis(150);
/// At a `shot:` marker and at a dialog: gives time to capture the window from outside.
const SHOT_PAUSE: Duration = Duration::from_millis(1500);

static ANSWERS: Mutex<VecDeque<usize>> = Mutex::new(VecDeque::new());
static PROMPTS: AtomicUsize = AtomicUsize::new(0);
/// The window the steps go to: the one opened (or brought forward) last; see [`target`].
static TARGET: Mutex<Option<AnyWindowHandle>> = Mutex::new(None);

/// The next steps go to this window: a window just opened, or brought forward.
pub fn target(window: AnyWindowHandle) {
    *TARGET.lock().unwrap() = Some(window);
}

/// A window has just opened: the steps go to it, and in the background the mouse goes through it
/// ([`let_the_mouse_through`]).
pub fn window_opened(window: AnyWindowHandle) {
    target(window);
    if background() {
        let_the_mouse_through();
    }
}

/// Flux pretends to be launched from the Dock or the Finder (`FLUX_SCENARIO_FINDER`): no project
/// from the current directory.
pub fn launched_from_finder() -> bool {
    std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var_os("FLUX_SCENARIO_FINDER").is_some_and(|value| !value.is_empty())
}

/// With `FLUX_SCENARIO` the window is a pop-up panel above all other windows: the app the author is
/// working in can't cover it, and frames get drawn.
/// The scenario runs in the background (`FLUX_SCENARIO_BACKGROUND`): Flux doesn't make itself the
/// active app, so the keys the user types keep going to the app they work in (the harness doesn't
/// activate it either). The window still floats above the others and draws.
pub fn background() -> bool {
    std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var_os("FLUX_SCENARIO_BACKGROUND").is_some_and(|value| !value.is_empty())
}

pub fn window_options(options: WindowOptions) -> WindowOptions {
    if std::env::var_os("FLUX_SCENARIO").is_none() {
        return options;
    }
    let background = background();
    if background {
        keep_panels_off_the_keyboard();
    }
    WindowOptions {
        kind: WindowKind::PopUp,
        // In the background the window doesn't become key ([`keep_panels_off_the_keyboard`]).
        focus: options.focus && !background,
        ..options
    }
}

/// A pop-up window of gpui is a non-activating panel (`GPUIPanel`, an `NSPanel`): once it is the
/// key window, it gets the keys the user types even though another app is active — as Spotlight
/// does. In the background a scenario's panels never become key: `activate_window` (a question
/// before closing, a window brought forward) only orders them to the front, and the keys keep
/// going to the app the user works in.
#[allow(unexpected_cfgs)] // `sel!` of objc 0.2 checks a `cargo-clippy` feature.
fn keep_panels_off_the_keyboard() {
    use std::os::raw::c_char;
    use std::sync::Once;

    use objc::runtime::{BOOL, Class, Imp, NO, Object, Sel};
    use objc::{sel, sel_impl};

    unsafe extern "C" {
        fn class_replaceMethod(
            class: *const Class,
            name: Sel,
            imp: Imp,
            types: *const c_char,
        ) -> Option<Imp>;
    }

    extern "C" fn cannot_become_key(_: &Object, _: Sel) -> BOOL {
        NO
    }

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // gpui registers its classes when the program loads.
        let Some(panel) = Class::get("GPUIPanel") else {
            eprintln!("flux: scenario: no GPUIPanel class, the window may take the keyboard");
            return;
        };
        // SAFETY: `canBecomeKeyWindow` is a method of the class with this signature
        // (`- (BOOL)canBecomeKeyWindow`); only its implementation is replaced.
        unsafe {
            let imp = std::mem::transmute::<extern "C" fn(&Object, Sel) -> BOOL, Imp>(
                cannot_become_key,
            );
            class_replaceMethod(panel, sel!(canBecomeKeyWindow), imp, c"B@:".as_ptr());
        }
    });
}

/// In the background the scenario's panels let the mouse through to the windows under them: the
/// user's clicks and scrolls go to the app they work in instead of a window floating above it (a
/// scenario can't feed the mouse anyway).
#[allow(unexpected_cfgs)] // `msg_send!` of objc 0.2 checks a `cargo-clippy` feature.
fn let_the_mouse_through() {
    use objc::runtime::{BOOL, Class, Object, YES};
    use objc::{class, msg_send, sel, sel_impl};

    let Some(panel) = Class::get("GPUIPanel") else {
        return;
    };
    // SAFETY: plain AppKit calls on the app's own windows, on the main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut Object = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        for index in 0..count {
            let window: *mut Object = msg_send![windows, objectAtIndex: index];
            let is_panel: BOOL = msg_send![window, isKindOfClass: panel];
            if is_panel {
                let _: () = msg_send![window, setIgnoresMouseEvents: YES];
            }
        }
    }
}

pub fn run(window: AnyWindowHandle, cx: &mut App) {
    if background() {
        let_the_mouse_through();
    }
    if let Ok(answers) = std::env::var("FLUX_ANSWERS") {
        let answers = answers
            .split(',')
            .filter_map(|a| a.trim().parse::<usize>().ok());
        ANSWERS.lock().unwrap().extend(answers);
        cx.set_prompt_builder(auto_prompt);
    }
    let Ok(scenario) = std::env::var("FLUX_SCENARIO") else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(START_DELAY).await;
        for step in scenario.split_whitespace() {
            let target = current_target(window, cx);
            let pause = run_step(step, target, cx);
            cx.background_executor().timer(pause).await;
        }
        println!("END");
    })
    .detach();
}

/// The window the next step goes to: the [`target`] while it is open, else the first window left
/// (or the one the scenario started with).
fn current_target(start: AnyWindowHandle, cx: &mut AsyncApp) -> AnyWindowHandle {
    let windows = cx.update(|cx| cx.windows()).unwrap_or_default();
    let target = TARGET.lock().unwrap().unwrap_or(start);
    if windows.contains(&target) {
        return target;
    }
    windows.first().copied().unwrap_or(start)
}

/// Executes a step and returns the pause to take after it.
fn run_step(step: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    if let Some(name) = step.strip_prefix("shot:") {
        println!("SHOT {name}");
        return SHOT_PAUSE;
    }
    if let Some(ms) = step.strip_prefix("wait:") {
        return Duration::from_millis(ms.parse().unwrap_or(0));
    }
    if let Some(path) = step.strip_prefix("open-project:") {
        let path = std::path::PathBuf::from(path);
        let dispatched = window
            .update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(crate::workspace::OpenProject(path)), cx)
            })
            .is_ok();
        println!(
            "OPEN-PROJECT {} -> {}",
            &step["open-project:".len()..],
            if dispatched { "dispatched" } else { "no window" }
        );
        return STEP_PAUSE;
    }
    if let Some(number) = step.strip_prefix("window:") {
        let windows = cx.update(|cx| cx.windows()).unwrap_or_default();
        let chosen = number
            .parse::<usize>()
            .ok()
            .and_then(|number| windows.get(number.checked_sub(1)?).copied());
        if let Some(chosen) = chosen {
            target(chosen);
        }
        println!(
            "WINDOW {number} of {} -> {}",
            windows.len(),
            if chosen.is_some() { "chosen" } else { "none" }
        );
        return STEP_PAUSE;
    }
    if let Some(target) = step.strip_prefix("ui:") {
        return plugin_ui(target, None, window, cx);
    }
    if let Some(target) = step.strip_prefix("ui-submit:") {
        let (target, text) = target.split_once('=').unwrap_or((target, ""));
        return plugin_ui(target, Some(text.to_string()), window, cx);
    }
    if let Some(name) = step.strip_prefix("action:") {
        let dispatched = window
            .update(cx, |_, window, cx| match cx.build_action(name, None) {
                Ok(action) => {
                    window.dispatch_action(action, cx);
                    true
                }
                Err(_) => false,
            })
            .unwrap_or(false);
        println!(
            "ACTION {name} -> {}",
            if dispatched { "dispatched" } else { "unknown" }
        );
        return STEP_PAUSE;
    }
    let keystrokes = match step.strip_prefix("type:") {
        Some(text) => text.chars().map(typed).collect(),
        None => match Keystroke::parse(step) {
            Ok(keystroke) => vec![keystroke],
            Err(err) => {
                println!("BAD STEP {step}: {err}");
                return STEP_PAUSE;
            }
        },
    };
    let mut handled = true;
    for keystroke in keystrokes {
        handled &= window
            .update(cx, |_, window, cx| window.dispatch_keystroke(keystroke, cx))
            .unwrap_or(false);
    }
    println!(
        "KEY {step} -> {}",
        if handled { "handled" } else { "ignored" }
    );
    STEP_PAUSE
}

/// A click on a plugin's tool window element (or ↵ in its field with `text`), delivered to the
/// plugin as the view would deliver it.
fn plugin_ui(
    target: &str,
    text: Option<String>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
) -> Duration {
    use flux_plugin::api::events::{Event, UiEvent, UiInput};
    let mut parts = target.splitn(3, '/');
    let (Some(plugin), Some(tool), Some(element)) = (parts.next(), parts.next(), parts.next())
    else {
        println!("BAD STEP ui:{target}: <plugin>/<window>/<element>");
        return STEP_PAUSE;
    };
    let event = match text {
        Some(text) => UiEvent::Submitted(text),
        None => UiEvent::Clicked,
    };
    let input = UiInput {
        window: tool.to_string(),
        element: element.to_string(),
        event,
    };
    let sent = window
        .downcast::<crate::workspace::Workspace>()
        .and_then(|workspace| {
            workspace
                .update(cx, |workspace, _, cx| {
                    workspace
                        .plugins
                        .update(cx, |store, _| store.send_to(plugin, Event::Ui(input)))
                })
                .ok()
        })
        .is_some();
    println!("UI {target} -> {}", if sent { "sent" } else { "no window" });
    STEP_PAUSE
}

/// A keystroke that types the character `c`, like typing on the keyboard without modifiers.
fn typed(c: char) -> Keystroke {
    Keystroke {
        modifiers: Modifiers::default(),
        key: c.to_lowercase().to_string(),
        key_char: Some(c.to_string()),
    }
}

/// The Flux dialog, answered by itself: the screenshot shows the question with the chosen button
/// focused.
fn auto_prompt(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    buttons: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let mut choose = |dialog: &crate::dialog::Dialog| {
        let last = dialog.buttons.len().saturating_sub(1);
        let answer = ANSWERS
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(last)
            .min(last);
        let label = dialog
            .buttons
            .get(answer)
            .map_or("?", |button| button.label.as_ref());
        let number = PROMPTS.fetch_add(1, Ordering::Relaxed) + 1;
        println!("PROMPT {} -> {label}", dialog.title);
        println!("SHOT prompt-{number}");
        (answer, SHOT_PAUSE)
    };
    crate::dialog::build_with(
        level,
        message,
        detail,
        buttons,
        handle,
        Some(&mut choose),
        window,
        cx,
    )
}
