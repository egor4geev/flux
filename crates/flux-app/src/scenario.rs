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
//!   - `move:x,y` — the mouse moves to a point of the Flux window opened last (points from its top
//!     left corner, as `click:`); `drag:x1,y1,x2,y2` — the left button goes down at the first point,
//!     drags to the second and goes up there (the editor's scrollbar, part 10.5);
//!   - `action:notifications_panel::Toggle` — dispatch an action (one without data) by name on the
//!     focused element, as the command palette does;
//!   - `ui:<plugin>/<window>/<element>` — a click on an element of a plugin's tool window (the
//!     event goes to the plugin as a click would send it: the mouse can't be fed); a tab of a
//!     window with tabs (API 0.5) — `ui:<plugin>/<window>/<key>/<element>`;
//!     `ui-submit:<plugin>/<window>/<element>=<text>` — ↵ in its text field with the text;
//!   - `shot:name` — print `SHOT name` and wait for the window to be captured from outside;
//!   - `open-project:/path` — open a project as a recent project on the start screen opens it
//!     (`OpenProject`: this window or a new one, as the settings say — or asking);
//!   - `window:2` — send the next steps to the second Flux window (in the order of `cx.windows()`);
//!   - `region:RU` — the country check answers this country from now on (`fail` — it fails) and is
//!     asked at once (part «Флаг страны»; without the step a scenario is in Finland, unless
//!     `FLUX_REGION_FILE` says otherwise);
//!   - `app-terminate` — macOS asks Flux to quit, as Quit in the Dock or logging out does
//!     (`-[NSApplication terminate:]` from a run loop timer, outside gpui's tasks — the path of the
//!     system's Apple Event; part 10.5);
//!   - `click:x,y` — a left click at a point of the Flux window opened last, in points from its top
//!     left corner (a shot's pixels halved): mouse events posted to Flux's own event queue, so
//!     the window server and the background mode don't stand in the way (part 10.5);
//!     `right-click:x,y` — the same with the right button: a context menu (FluxGram part 5) —
//!     gpui's events to the scenario's window (AppKit's synthesized events carry no button);
//!     `shift-click:x,y` — a left click with ⇧ held, the same way (selecting a run of messages);
//!     `press:x,y` and `release:x,y` — the left button only goes down, or only up, there: a click
//!     held for a moment with `wait:` between them, as a hand clicks — a link of a text (gpui's
//!     `InteractiveText`) takes the release only after a frame drawn since the press (FluxGram
//!     part 5);
//!   - `drag-files:x,y=/a,/b` — files from Finder dragged into the window to `x, y` (window pixels,
//!     as `click:`), and held there: gpui's `FileDropEvent::Entered` and `Pending`;
//!     `drop-files:x,y` — dropped there: the mouse up that `FileDropEvent::Submit` becomes, without
//!     its activating Flux (in the background the keyboard stays with the user's app), then
//!     `Exited`, as AppKit ends a drag;
//!   - `pinch:x,y=0.5` — a pinch on the trackpad at `x, y` (window points, as `click:`) that makes
//!     the picture 50 % larger (`-0.5` — half as large): gpui's `PinchEvent`s of a gesture, its
//!     magnification in steps a frame apart.
//!   - `scroll:x,y=-600*40` — the wheel at `x, y` (window points, as `click:`): 40 events of 600
//!     pixels each (negative — towards the start, up), a frame apart, as a fast trackpad flick
//!     (FluxGram part 1: the conversation's scrolling is measured with it);
//!   - `mark:name` — a line `MARK name` in the output and in the frame log (`FLUX_SCENARIO_FRAMES`):
//!     the phases of a measurement (idle, scrolling) split the log.
//!   - `hub:<call>` — a call of Claude for remote work (API 0.6, [`crate::claude_hub`]) as a plugin
//!     makes it; the reply is printed as `HUB <call> <reply>` (one that takes a while — the limits'
//!     probe, a rollback, a new session's window — when it comes): `hub:sessions`,
//!     `hub:transcript:<n>:<count>`, `hub:send:<n>:<text>`, `hub:answer:<n>:allow[:<choice>]`,
//!     `…:deny[:<text>]`, `…:1,2/3` (the options of each question, from 1), `…:plan-edits`,
//!     `…:plan-ask`, `…:keep:<text>`, `hub:interrupt:<n>`, `hub:mode:<n>:ask|edits|plan|bypass`,
//!     `hub:rename:<n>:<title>`, `hub:close:<n>`,
//!     `hub:new:<project folder>:<prompt>`, `hub:diff:<n>[:<turn>]`,
//!     `hub:check-rollback:<n>[:<turn>]`, `hub:rollback:<n>[:<turn>]`, `hub:limits`,
//!     `hub:region[:again]`, `hub:projects`, `hub:awake:off|always|power`, `hub:system`,
//!     `hub:action:<n>:<plugin>/<action>` (the plugin's action at the session in the console, as
//!     its menu runs it — «Continue in Telegram»). `<n>` —
//!     the n-th session of `hub:sessions` (from 1) or an id; `+` in a text is a space.
//!     `FLUX_HUB_LOG=<file>` — every event the hub posts goes there as a line.
//!
//!   `FLUX_SCENARIO_PASTEBOARD=<name>` (gpui, Flux's patch) — the clipboard is the macOS pasteboard
//!   of that name instead of the general one: a scenario copies and pastes without touching the
//!   user's clipboard; a helper puts files on it as Finder does, or reads what Flux put there.
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
//! - `FLUX_SCENARIO_TITLE_BAR=windows` — the title bars are Windows' (part 10.9): the menu button
//!   (`action:workspace::ShowMainMenu` opens Windows' menu), the window buttons, and closing the
//!   last window quits ([`windows_title_bar`]); a headless shot then has no traffic lights painted
//!   in. macOS still has its own buttons where the menu button is: a `click:` there closes.
//! - `FLUX_SCENARIO_HEADLESS=1` (the harness sets it when the screen is locked) — Flux draws the
//!   frames itself on a timer and saves each `shot:` as `<FLUX_SCENARIO_OUT>/<name>-<window id>.png`,
//!   rendered offscreen ([`headless`]): a locked screen stops gpui's display link and the window
//!   server won't capture windows. A window is drawn only when something changed in it, as with the
//!   display link.
//! - `FLUX_SCENARIO_FRAMES=<file>` — every frame a window draws (the display link's or the headless
//!   timer's) goes there as a line `<ms since start>\t<window>\t<draw µs>`, and `mark:` steps add
//!   `MARK <name>` lines: frames drawn while idle and the time a frame takes to lay out and paint,
//!   measured (FluxGram part 1; gpui's `observe_frames`, Flux's patch).
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
/// Headless: a frame every 16 ms, as the display would drive them; a shot a moment after its marker,
/// as the harness takes it (the frame has finished drawing).
const HEADLESS_FRAME: Duration = Duration::from_millis(16);
const HEADLESS_SHOT_DELAY: Duration = Duration::from_millis(500);

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

/// The title bars are Windows' (`FLUX_SCENARIO_TITLE_BAR=windows`): what the author sees in a
/// Windows VM, shot on a Mac ([`crate::title_bar::os`]).
pub fn windows_title_bar() -> bool {
    std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var("FLUX_SCENARIO_TITLE_BAR").is_ok_and(|value| value == "windows")
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
            let imp =
                std::mem::transmute::<extern "C" fn(&Object, Sel) -> BOOL, Imp>(cannot_become_key);
            class_replaceMethod(panel, sel!(canBecomeKeyWindow), imp, c"B@:".as_ptr());
        }
    });
}

/// `click:x,y`: a left mouse down and up at `x, y` (points from the top left corner of the
/// visible window opened last), posted as [`post_mouse`] posts its events.
fn post_click(x: f64, y: f64) -> bool {
    const LEFT_MOUSE_DOWN: usize = 1;
    const LEFT_MOUSE_UP: usize = 2;
    post_mouse(&[(LEFT_MOUSE_DOWN, x, y), (LEFT_MOUSE_UP, x, y)])
}

/// `app-terminate`: `terminate:` as macOS sends it for Quit in the Dock — from the run loop, not
/// from a task on the main dispatch queue (gpui's), where AppKit's wait for Flux's answer would stop
/// gpui.
#[allow(unexpected_cfgs)] // `msg_send!` of objc 0.2 checks a `cargo-clippy` feature.
fn request_terminate() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let nil: *mut Object = std::ptr::null_mut();
        let _: () =
            msg_send![app, performSelector: sel!(terminate:) withObject: nil afterDelay: 0.0f64];
    }
}

/// In the background the scenario's panels let the mouse through to the windows under them: the
/// user's clicks and scrolls go to the app they work in instead of a window floating above it (a
/// scenario can't feed the mouse anyway; `click:` posts its clicks to Flux's own queue).
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

/// `click:`, `move:` and `drag:`: mouse events of these types (`NSEventType`) at these points (from
/// the top left corner of the visible window opened last), posted to the app's event queue
/// in order — AppKit hands them to the window by its number, as it does the window server's events,
/// so the window server and the background mode don't stand in the way.
#[allow(unexpected_cfgs)] // `msg_send!` of objc 0.2 checks a `cargo-clippy` feature.
fn post_mouse(events: &[(usize, f64, f64)]) -> bool {
    use objc::runtime::{NO, Object};
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    unsafe impl objc::Encode for Point {
        fn encode() -> objc::Encoding {
            unsafe { objc::Encoding::from_str("{CGPoint=dd}") }
        }
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Point,
    }
    unsafe impl objc::Encode for Rect {
        fn encode() -> objc::Encoding {
            unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
        }
    }

    // SAFETY: AppKit's own windows and events, on the main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let windows: *mut Object = msg_send![app, windows];
        let count: usize = msg_send![windows, count];
        // The window opened last: the one the steps go to. Not the one in front by `orderedIndex`
        // — in the background the panels don't come in front, and a window opened over another
        // (the welcome guide over the project, part 10.7) would lose its clicks to it.
        let mut front: Option<(isize, *mut Object)> = None;
        for index in 0..count {
            let window: *mut Object = msg_send![windows, objectAtIndex: index];
            let visible: objc::runtime::BOOL = msg_send![window, isVisible];
            let number: isize = msg_send![window, windowNumber];
            if visible != NO && front.is_none_or(|(best, _)| number > best) {
                front = Some((number, window));
            }
        }
        let Some((_, window)) = front else {
            return false;
        };
        let number: isize = msg_send![window, windowNumber];
        let view: *mut Object = msg_send![window, contentView];
        let frame: Rect = msg_send![view, frame];
        let nil: *mut Object = std::ptr::null_mut();
        for &(kind, x, y) in events {
            // AppKit's window coordinates start at the bottom left.
            let location = Point {
                x,
                y: frame.size.y - y,
            };
            let event: *mut Object = msg_send![class!(NSEvent),
                mouseEventWithType: kind
                location: location
                modifierFlags: 0usize
                timestamp: 0.0f64
                windowNumber: number
                context: nil
                eventNumber: 0isize
                clickCount: 1isize
                pressure: 1.0f32];
            let _: () = msg_send![app, postEvent: event atStart: NO];
        }
    }
    true
}

pub fn run(window: AnyWindowHandle, cx: &mut App) {
    if background() {
        let_the_mouse_through();
    }
    if std::env::var_os("FLUX_SCENARIO_FRAMES").is_some() {
        // Every drawn frame, whoever asked for it: the display link of a window on screen, the
        // headless timer.
        gpui::observe_frames(|window, took| {
            frame_log(&format!("{}\t{}", window.as_u64(), took.as_micros()))
        });
    }
    if headless() {
        pump_frames(cx);
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

/// The frames and the screenshots are Flux's own (`FLUX_SCENARIO_HEADLESS`): the screen is locked
/// (the author is away) or the windows must not depend on the window server. gpui (Flux's patch,
/// `vendor/gpui/FLUX-PATCH.md`) draws a frame on request and renders it offscreen.
pub fn headless() -> bool {
    std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var_os("FLUX_SCENARIO_HEADLESS").is_some_and(|value| !value.is_empty())
}

/// Headless: drives the frames of every window, as the display link would (it stops for an
/// occluded window, and a locked screen occludes all of them).
fn pump_frames(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(HEADLESS_FRAME).await;
            let Ok(windows) = cx.update(|cx| cx.windows()) else {
                break;
            };
            for window in windows {
                window
                    .update(cx, |_, window, cx| window.headless_frame(cx))
                    .ok();
            }
        }
    })
    .detach();
}

/// A line of the frame log (`FLUX_SCENARIO_FRAMES`), after the milliseconds since the first one.
fn frame_log(line: &str) {
    use std::io::Write;
    static LOG: Mutex<Option<(std::fs::File, std::time::Instant)>> = Mutex::new(None);
    let Some(path) = std::env::var_os("FLUX_SCENARIO_FRAMES") else {
        return;
    };
    let mut log = LOG.lock().unwrap();
    if log.is_none() {
        match std::fs::File::create(&path) {
            Ok(file) => *log = Some((file, std::time::Instant::now())),
            Err(err) => {
                eprintln!("flux: scenario: the frame log: {err}");
                return;
            }
        }
    }
    if let Some((file, start)) = log.as_mut() {
        let _ = writeln!(file, "{}\t{line}", start.elapsed().as_millis());
    }
}

/// Headless: a moment later, every window rendered offscreen into
/// `<FLUX_SCENARIO_OUT>/<name>-<window id>.png`, over a backdrop standing in for the blurred desktop.
fn capture_later(name: String, cx: &AsyncApp) {
    if !headless() {
        return;
    }
    let Some(dir) = std::env::var_os("FLUX_SCENARIO_OUT") else {
        eprintln!("flux: scenario: FLUX_SCENARIO_OUT isn't set, shot {name} not saved");
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(HEADLESS_SHOT_DELAY).await;
        let windows = cx.update(|cx| cx.windows()).unwrap_or_default();
        for window in windows {
            let image = window
                .update(cx, |_, window, cx| {
                    let dark =
                        crate::theme::Theme::get(cx).appearance == crate::theme::Appearance::Dark;
                    let width = f32::from(window.bounds().size.width);
                    let ui_scale = window.ui_scale();
                    window.render_to_image(cx).map(|image| {
                        let device = image.width() as f32 / width.max(1.);
                        let mut image = over_backdrop(image, dark);
                        if !windows_title_bar() {
                            paint_traffic_lights(&mut image, device, ui_scale);
                        }
                        image
                    })
                })
                .ok()
                .flatten();
            let Some(image) = image else {
                eprintln!("flux: scenario: shot {name}: the window wasn't rendered");
                continue;
            };
            let file = format!("{name}-{}.png", window.window_id().as_u64());
            let path = std::path::Path::new(&dir).join(file);
            if let Err(err) = image.save(&path) {
                eprintln!("flux: scenario: can't save {}: {err}", path.display());
            }
        }
    })
    .detach();
}

/// The offscreen render has premultiplied alpha and nothing behind the glass: it is put over a
/// plain backdrop of the theme's appearance.
fn over_backdrop(mut image: image::RgbaImage, dark: bool) -> image::RgbaImage {
    let backdrop: [f32; 3] = if dark {
        [30., 32., 38.]
    } else {
        [214., 218., 226.]
    };
    for pixel in image.pixels_mut() {
        let alpha = f32::from(pixel[3]) / 255.;
        for (channel, back) in pixel.0.iter_mut().take(3).zip(backdrop) {
            *channel = (f32::from(*channel) + back * (1. - alpha))
                .round()
                .min(255.) as u8;
        }
        pixel[3] = 255;
    }
    image
}

/// macOS draws the traffic lights itself, so the offscreen render hasn't got them: they are painted
/// where Flux puts them ([`crate::ui_scale::traffic_lights`], kept inside the system's title bar as
/// gpui keeps them), in the system's colors — a shot looks like the window (README screenshots).
fn paint_traffic_lights(image: &mut image::RgbaImage, device: f32, ui_scale: f32) {
    /// The button's frame, the circle in it and the step between buttons, in points (macOS 26).
    const FRAME: f32 = 14.;
    const DIAMETER: f32 = 13.5;
    const STEP: f32 = 23.;
    const SYSTEM_TITLE_BAR: f32 = 28.;
    const COLORS: [[f32; 3]; 3] = [[255., 95., 87.], [254., 188., 46.], [40., 200., 64.]];
    let origin = crate::ui_scale::traffic_lights(ui_scale);
    let left = f32::from(origin.x);
    let top = f32::from(origin.y).min(SYSTEM_TITLE_BAR - FRAME);
    let radius = DIAMETER / 2. * device;
    let (width, height) = image.dimensions();
    for (index, color) in COLORS.iter().enumerate() {
        let center_x = (left + FRAME / 2. + STEP * index as f32) * device;
        let center_y = (top + FRAME / 2.) * device;
        let rows = (center_y - radius - 1.).max(0.) as u32..(center_y + radius + 2.) as u32;
        for y in rows.filter(|y| *y < height) {
            let columns = (center_x - radius - 1.).max(0.) as u32..(center_x + radius + 2.) as u32;
            for x in columns.filter(|x| *x < width) {
                let distance = ((x as f32 + 0.5 - center_x).powi(2)
                    + (y as f32 + 0.5 - center_y).powi(2))
                .sqrt();
                let coverage = (radius + 0.5 - distance).clamp(0., 1.);
                if coverage == 0. {
                    continue;
                }
                let pixel = image.get_pixel_mut(x, y);
                for (channel, value) in pixel.0.iter_mut().take(3).zip(color) {
                    *channel =
                        (f32::from(*channel) * (1. - coverage) + value * coverage).round() as u8;
                }
            }
        }
    }
}

/// Executes a step and returns the pause to take after it.
fn run_step(step: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    if let Some(name) = step.strip_prefix("shot:") {
        println!("SHOT {name}");
        capture_later(name.to_string(), cx);
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
            if dispatched {
                "dispatched"
            } else {
                "no window"
            }
        );
        return STEP_PAUSE;
    }
    if let Some(code) = step.strip_prefix("region:") {
        cx.update(|cx| crate::region_guard::simulate(code, cx)).ok();
        println!("REGION {code}");
        // The answer comes from a background thread.
        return STEP_PAUSE * 2;
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
    if let Some(point) = step.strip_prefix("right-click:") {
        return right_click(
            point,
            gpui::MouseButton::Right,
            Modifiers::default(),
            window,
            cx,
        );
    }
    if let Some(point) = step.strip_prefix("shift-click:") {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        return right_click(point, gpui::MouseButton::Left, shift, window, cx);
    }
    if let Some(point) = step.strip_prefix("click:") {
        let point = point
            .split_once(',')
            .and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?)));
        let clicked = point.is_some_and(|(x, y)| post_click(x, y));
        println!(
            "CLICK {point:?} -> {}",
            if clicked { "posted" } else { "no window" }
        );
        return STEP_PAUSE;
    }
    if let Some(drag) = step.strip_prefix("drag-files:") {
        return drag_files(drag, window, cx);
    }
    if let Some(point) = step.strip_prefix("drop-files:") {
        return drop_files(point, window, cx);
    }
    if let Some(pinch) = step.strip_prefix("pinch:") {
        return pinch_at(pinch, window, cx);
    }
    if let Some(scroll) = step.strip_prefix("scroll:") {
        return scroll_at(scroll, window, cx);
    }
    if let Some(name) = step.strip_prefix("mark:") {
        println!("MARK {name}");
        frame_log(&format!("MARK {name}"));
        return STEP_PAUSE;
    }
    if step == "app-terminate" {
        request_terminate();
        println!("TERMINATE requested");
        return STEP_PAUSE;
    }
    if let Some(call) = step.strip_prefix("hub:") {
        return hub_step(call, cx);
    }
    if let Some(target) = step.strip_prefix("ui:") {
        // The frame log shows what the click brought: the frames after it until the next mark.
        frame_log(&format!("MARK ui:{target}"));
        return plugin_ui(target, None, window, cx);
    }
    if let Some(target) = step.strip_prefix("ui-submit:") {
        let (target, text) = target.split_once('=').unwrap_or((target, ""));
        return plugin_ui(target, Some(text.to_string()), window, cx);
    }
    if let Some(points) = step
        .strip_prefix("move:")
        .or_else(|| step.strip_prefix("drag:"))
        .or_else(|| step.strip_prefix("press:"))
        .or_else(|| step.strip_prefix("release:"))
    {
        const LEFT_MOUSE_DOWN: usize = 1;
        const LEFT_MOUSE_UP: usize = 2;
        const MOUSE_MOVED: usize = 5;
        const LEFT_MOUSE_DRAGGED: usize = 6;
        let numbers: Vec<f64> = points.split(',').filter_map(|n| n.parse().ok()).collect();
        let events: Vec<(usize, f64, f64)> = match numbers[..] {
            [x, y] if step.starts_with("move:") => vec![(MOUSE_MOVED, x, y)],
            [x, y] if step.starts_with("press:") => vec![(LEFT_MOUSE_DOWN, x, y)],
            [x, y] if step.starts_with("release:") => vec![(LEFT_MOUSE_UP, x, y)],
            [x1, y1, x2, y2] => {
                // The drag in steps, as a hand moves it.
                let steps = 8;
                let mut events = vec![(LEFT_MOUSE_DOWN, x1, y1)];
                events.extend((1..=steps).map(|i| {
                    let t = i as f64 / steps as f64;
                    (LEFT_MOUSE_DRAGGED, x1 + (x2 - x1) * t, y1 + (y2 - y1) * t)
                }));
                events.push((LEFT_MOUSE_UP, x2, y2));
                events
            }
            _ => Vec::new(),
        };
        let posted = !events.is_empty() && post_mouse(&events);
        println!(
            "MOUSE {step} -> {}",
            if posted { "posted" } else { "bad step" }
        );
        return STEP_PAUSE;
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

/// `x,y` in window pixels.
fn window_point(point: &str) -> Option<gpui::Point<gpui::Pixels>> {
    let (x, y) = point.split_once(',')?;
    Some(gpui::point(
        gpui::px(x.parse().ok()?),
        gpui::px(y.parse().ok()?),
    ))
}

/// `drag-files:x,y=/a,/b`: files from Finder enter the window and are held at `x, y`.
fn drag_files(step: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    use gpui::{ExternalPaths, FileDropEvent, PlatformInput};
    let (point, paths) = step.split_once('=').unwrap_or((step, ""));
    let Some(position) = window_point(point) else {
        println!("BAD STEP drag-files:{step}: x,y=/path,/path");
        return STEP_PAUSE;
    };
    let paths = ExternalPaths::new(paths.split(',').map(std::path::PathBuf::from));
    let dispatched = window
        .update(cx, |_, window, cx| {
            let entered = FileDropEvent::Entered { position, paths };
            window.dispatch_event(PlatformInput::FileDrop(entered), cx);
            let pending = FileDropEvent::Pending { position };
            window.dispatch_event(PlatformInput::FileDrop(pending), cx);
        })
        .is_ok();
    println!(
        "DRAG-FILES {step} -> {}",
        if dispatched { "held" } else { "no window" }
    );
    STEP_PAUSE
}

/// `drop-files:x,y`: the files held by `drag-files:` are dropped at `x, y`. gpui turns
/// `FileDropEvent::Submit` into this mouse up after activating the app; the step leaves the app
/// as it is.
fn drop_files(point: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    use gpui::{FileDropEvent, MouseButton, MouseUpEvent, PlatformInput};
    let Some(position) = window_point(point) else {
        println!("BAD STEP drop-files:{point}: x,y");
        return STEP_PAUSE;
    };
    let dispatched = window
        .update(cx, |_, window, cx| {
            let up = MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            };
            window.dispatch_event(PlatformInput::MouseUp(up), cx);
            window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Exited), cx);
        })
        .is_ok();
    println!(
        "DROP-FILES {point} -> {}",
        if dispatched { "dropped" } else { "no window" }
    );
    STEP_PAUSE
}

/// `right-click:x,y`: the mouse moves to `x, y` (window points, as `click:`) and its right button
/// goes down and up there, as gpui's events to the scenario's window. Not through AppKit's queue
/// as `click:`: an `NSEvent` made by `mouseEventWithType:` has no button number, which gpui reads
/// the button from — a right mouse down came as the left button's (a click, the hover bar), never
/// a menu. `shift-click:` comes the same way with the left button and ⇧.
fn right_click(
    step: &str,
    button: gpui::MouseButton,
    modifiers: Modifiers,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
) -> Duration {
    use gpui::{MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput};
    let Some(point) = window_point(step) else {
        println!("BAD STEP right-click:{step}: x,y");
        return STEP_PAUSE;
    };
    let sent = window.update(cx, |_, window, cx| {
        let position = point * (1. / window.ui_scale());
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button,
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button,
                position,
                modifiers,
                click_count: 1,
            }),
            cx,
        );
    });
    println!(
        "RIGHT-CLICK {step} -> {}",
        if sent.is_ok() { "sent" } else { "no window" }
    );
    STEP_PAUSE
}

/// `scroll:x,y=-600*40`: the wheel at `x, y` — that many events of that many pixels, a frame
/// apart (negative: up, towards the start), as a fast flick on the trackpad.
fn scroll_at(step: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    use gpui::{PlatformInput, ScrollDelta, ScrollWheelEvent, TouchPhase};
    let (point, amount) = step.split_once('=').unwrap_or((step, ""));
    let (delta, count) = amount.split_once('*').unwrap_or((amount, "1"));
    let (Some(point), Ok(delta), Ok(count)) = (
        window_point(point),
        delta.parse::<f32>(),
        count.parse::<u32>(),
    ) else {
        println!("BAD STEP scroll:{step}: x,y=-600*40");
        return STEP_PAUSE;
    };
    let step = step.to_string();
    cx.spawn(async move |cx| {
        for index in 0..count {
            let phase = match index {
                0 => TouchPhase::Started,
                index if index + 1 == count => TouchPhase::Ended,
                _ => TouchPhase::Moved,
            };
            let sent = window.update(cx, |_, window, cx| {
                let event = ScrollWheelEvent {
                    position: point * (1. / window.ui_scale()),
                    // gpui's wheel: a positive delta scrolls towards the start.
                    delta: ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-delta))),
                    modifiers: Modifiers::default(),
                    touch_phase: phase,
                };
                window.dispatch_event(PlatformInput::ScrollWheel(event), cx);
            });
            if sent.is_err() {
                println!("SCROLL {step} -> no window");
                return;
            }
            cx.background_executor().timer(HEADLESS_FRAME).await;
        }
        // A list doesn't stop the wheel's propagation: the frame log tells whether it scrolled.
        println!("SCROLL {step} -> sent");
    })
    .detach();
    STEP_PAUSE + HEADLESS_FRAME * count * 2
}

/// `pinch:x,y=0.5`: a pinch at `x, y` that magnifies by 0.5 in all — its start, the magnification
/// in steps of equal ratio a frame apart (the picture redraws between them, as under the
/// fingers), its end.
fn pinch_at(step: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Duration {
    use gpui::{PinchEvent, PlatformInput, TouchPhase};
    const PINCH_STEPS: usize = 8;
    let (point, magnification) = step.split_once('=').unwrap_or((step, ""));
    let magnification = magnification
        .parse::<f32>()
        .ok()
        .filter(|magnification| magnification.is_finite() && *magnification > -1.);
    let (Some(point), Some(magnification)) = (window_point(point), magnification) else {
        println!("BAD STEP pinch:{step}: x,y=0.5");
        return STEP_PAUSE;
    };
    let delta = (1. + magnification).powf(1. / PINCH_STEPS as f32) - 1.;
    let events: Vec<(TouchPhase, f32)> = std::iter::once((TouchPhase::Started, 0.))
        .chain(std::iter::repeat_n((TouchPhase::Moved, delta), PINCH_STEPS))
        .chain(std::iter::once((TouchPhase::Ended, 0.)))
        .collect();
    let count = events.len() as u32;
    let step = step.to_string();
    cx.spawn(async move |cx| {
        let mut handled = Some(true);
        for (phase, delta) in events {
            let result = window.update(cx, |_, window, cx| {
                // Window points, as the platform gives them: the window's pixels at its scale.
                let event = PinchEvent {
                    position: point * (1. / window.ui_scale()),
                    delta,
                    modifiers: Modifiers::default(),
                    phase,
                };
                !window
                    .dispatch_event(PlatformInput::Pinch(event), cx)
                    .propagate
            });
            handled = handled.zip(result.ok()).map(|(all, this)| all && this);
            cx.background_executor().timer(HEADLESS_FRAME).await;
        }
        let outcome = match handled {
            Some(true) => "handled",
            Some(false) => "ignored",
            None => "no window",
        };
        println!("PINCH {step} -> {outcome}");
    })
    .detach();
    // The events and the frames between them, with room (a debug build draws a frame slower): the
    // next step comes after the pinch.
    STEP_PAUSE + HEADLESS_FRAME * count * 6
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
    let sent = window
        .downcast::<crate::workspace::Workspace>()
        .and_then(|workspace| {
            workspace
                .update(cx, |workspace, _, cx| {
                    workspace.plugins.update(cx, |store, _| {
                        // A window with tabs (API 0.5): `<plugin>/<window>/<key>/<element>` names
                        // an element of the tab `<window>/<key>`.
                        let tabbed = store.plugin(plugin).is_some_and(|state| {
                            state
                                .entry
                                .manifest
                                .tool_window(tool)
                                .is_some_and(|spec| spec.tabs)
                        });
                        let (window, element) = match element.split_once('/') {
                            Some((key, element)) if tabbed => (format!("{tool}/{key}"), element),
                            _ => (tool.to_string(), element),
                        };
                        let input = UiInput {
                            window,
                            element: element.to_string(),
                            event,
                        };
                        store.send_to(plugin, Event::Ui(input))
                    })
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
    let mut shot = None;
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
        shot = Some(format!("prompt-{number}"));
        (answer, SHOT_PAUSE)
    };
    let prompt = crate::dialog::build_with(
        level,
        message,
        detail,
        buttons,
        handle,
        Some(&mut choose),
        window,
        cx,
    );
    if let Some(name) = shot {
        cx.spawn(async move |cx| capture_later(name, cx)).detach();
    }
    prompt
}

/// A `hub:` step: the call as a plugin makes it; the reply printed as `HUB <call> <reply>` — at
/// once, or when it comes (within half a minute).
fn hub_step(step: &str, cx: &mut AsyncApp) -> Duration {
    use flux_plugin::claude_remote::{
        AwakeMode, NewSession, RemoteAnswer, RemoteCall, RemoteMode, RemoteReply, RequestKind,
    };
    let words = |text: &str| text.replace('+', " ");
    let parts: Vec<&str> = step.splitn(3, ':').collect();
    let name = parts[0].to_string();
    let arg = |index: usize| parts.get(index).copied().unwrap_or("");
    // The n-th session of `hub:sessions`, or an id; with its request.
    let session = |n: &str,
                   cx: &mut AsyncApp|
     -> Option<(String, Option<flux_plugin::claude_remote::RemoteRequest>)> {
        let replies = cx
            .update(|cx| crate::claude_hub::scenario_call(RemoteCall::Sessions, cx))
            .ok()?;
        let Ok(RemoteReply::Sessions(sessions)) = replies.try_recv() else {
            return None;
        };
        match n.parse::<usize>() {
            Ok(index) => sessions
                .get(index.checked_sub(1)?)
                .map(|session| (session.id.clone(), session.request.clone())),
            Err(_) => Some((
                n.to_string(),
                sessions
                    .iter()
                    .find(|session| session.id == n)
                    .and_then(|session| session.request.clone()),
            )),
        }
    };
    let (id, request) = match name.as_str() {
        "sessions" | "limits" | "region" | "projects" | "awake" | "system" | "new" => {
            (String::new(), None)
        }
        _ => match session(arg(1), cx) {
            Some(found) => found,
            None => {
                println!("HUB {step} -> no such session");
                return STEP_PAUSE;
            }
        },
    };
    let rest = arg(2);
    if name == "action" {
        let (plugin, action) = rest.split_once('/').unwrap_or((rest, ""));
        crate::claude_hub::run_action(plugin, id, action.to_string());
        println!("HUB {step} -> sent");
        return STEP_PAUSE;
    }
    let turn = rest.parse::<u32>().ok();
    let call = match name.as_str() {
        "sessions" => RemoteCall::Sessions,
        "transcript" => RemoteCall::Transcript {
            session: id,
            count: rest.parse().unwrap_or(10),
        },
        "send" => RemoteCall::Send {
            session: id,
            text: words(rest),
            images: Vec::new(),
        },
        "interrupt" => RemoteCall::Interrupt { session: id },
        "rename" => RemoteCall::Rename {
            session: id,
            title: words(rest),
        },
        "close" => RemoteCall::Close { session: id },
        "mode" => RemoteCall::SetMode {
            session: id,
            mode: match rest {
                "ask" => RemoteMode::Ask,
                "edits" => RemoteMode::AcceptEdits,
                "plan" => RemoteMode::Plan,
                "bypass" => RemoteMode::Bypass,
                _ => RemoteMode::Other,
            },
        },
        "answer" => {
            let Some(request) = request else {
                println!("HUB {step} -> no request");
                return STEP_PAUSE;
            };
            let (kind, text) = rest.split_once(':').unwrap_or((rest, ""));
            let answer = match kind {
                "allow" => RemoteAnswer::Allow {
                    choice: (!text.is_empty()).then(|| text.to_string()),
                },
                "deny" => RemoteAnswer::Deny {
                    message: words(text),
                },
                "plan-edits" => RemoteAnswer::ApprovePlan { accept_edits: true },
                "plan-ask" => RemoteAnswer::ApprovePlan {
                    accept_edits: false,
                },
                "keep" => RemoteAnswer::KeepPlanning(words(text)),
                numbers => {
                    let RequestKind::Questions(questions) = &request.kind else {
                        println!("HUB {step} -> not questions");
                        return STEP_PAUSE;
                    };
                    RemoteAnswer::Answers(
                        numbers
                            .split('/')
                            .zip(questions)
                            .map(|(picked, question)| {
                                picked
                                    .split(',')
                                    .filter_map(|n| n.parse::<usize>().ok()?.checked_sub(1))
                                    .filter_map(|n| question.options.get(n))
                                    .map(|(label, _)| label.clone())
                                    .collect()
                            })
                            .collect(),
                    )
                }
            };
            RemoteCall::Answer {
                session: id,
                request: request.id,
                answer,
            }
        }
        "new" => {
            let (project, prompt) = step["new:".len()..]
                .rsplit_once(':')
                .unwrap_or((&step["new:".len()..], ""));
            RemoteCall::NewSession(NewSession {
                project: project.to_string(),
                title: None,
                prompt: (!prompt.is_empty()).then(|| words(prompt)),
                attachments: Vec::new(),
                images: Vec::new(),
                mode: RemoteMode::Ask,
            })
        }
        "diff" => RemoteCall::TurnDiff { session: id, turn },
        "check-rollback" => RemoteCall::CheckRollback { session: id, turn },
        "rollback" => RemoteCall::Rollback { session: id, turn },
        "limits" => RemoteCall::Limits,
        "region" => RemoteCall::Region {
            again: arg(1) == "again",
        },
        "projects" => RemoteCall::Projects,
        "awake" => RemoteCall::SetAwake(match arg(1) {
            "always" => AwakeMode::Always,
            "power" => AwakeMode::OnPower,
            _ => AwakeMode::Off,
        }),
        "system" => RemoteCall::System,
        _ => {
            println!("HUB {step} -> unknown call");
            return STEP_PAUSE;
        }
    };
    let Ok(replies) = cx.update(|cx| crate::claude_hub::scenario_call(call, cx)) else {
        return STEP_PAUSE;
    };
    let step = step.to_string();
    cx.spawn(async move |cx| {
        for _ in 0..300 {
            if let Ok(reply) = replies.try_recv() {
                println!("HUB {step} {reply:?}");
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }
        println!("HUB {step} -> no reply");
    })
    .detach();
    STEP_PAUSE
}
