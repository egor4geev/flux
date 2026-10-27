//! The welcome guide (part 10.7): a window of its own, slide after slide. First the setup — the
//! interface language ([`intro`]), the look ([`appearance`]), the languages one writes in
//! ([`languages`]), Claude Code ([`claude`]) — then five slides about working in Flux, each with a
//! picture Flux draws itself in the chosen theme and a live task: press the keys, and the picture
//! does what Flux would ([`tour`], [`scenes`]); the keys to remember ([`shortcuts`]); and "All set"
//! with the projects to open ([`finish`]). The dots at the bottom group the slides by chapter: Setup,
//! Tour, Start. Every choice applies at once, to the guide itself as well.
//!
//! The guide opens by itself on the first launch (no `settings.json` yet): alone when Flux starts
//! from the Dock — the start screen comes when it closes — or over the project's window a moment
//! after it, when Flux starts with a project. Later: Help → Welcome Guide and the palette; Help →
//! Keyboard Shortcuts opens it on the keys. After an update to a new minor version the same window
//! shows What's New once ([`whats_new`]). It is a window like the project windows, but not one of
//! them: ⌘` doesn't go to it, it isn't reopened on startup, quitting doesn't ask about it.

mod appearance;
mod claude;
mod finish;
mod intro;
mod languages;
mod scenes;
mod scenes_vcs;
mod shortcuts;
mod tour;
mod whats_new;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    Action, AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, BoxShadow, ClickEvent,
    Context, DisplayId, Div, ElementId, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    Pixels, Render, SharedString, Size, Stateful, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, div, point, prelude::*, px, size,
};

use crate::i18n::tr;
use crate::icons::{IconName, icon};
use crate::notifications::Notification;
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::{self, Workspace};

// The Help menu's actions: in its namespace, so the palette shows them as "Help: …" with the
// menu's own titles (`command_palette::own_title`).
actions!(
    help,
    [
        /// Help → Welcome Guide, the palette: the guide on its first slide (the open one comes
        /// forward).
        OpenGuide,
        /// Help → Keyboard Shortcuts: the guide on the keys to remember.
        ShowShortcuts,
        /// Help → What's New: what this version of Flux brought.
        ShowWhatsNew,
    ]
);

actions!(
    welcome,
    [
        /// The next slide (→, ↵).
        Next,
        /// The previous slide (←).
        Previous,
        /// Closes the guide (Esc) — unless something on the slide has the focus and takes Esc.
        Close,
        /// Closes the guide whatever has the focus (⌘W).
        CloseWindow,
    ]
);

/// A tour slide's live task: its keys were pressed in the guide (bound in the guide's own key
/// context, [`CONTEXT`], by [`tour::bind_keys`]).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = welcome, no_json)]
pub struct TryTask(pub Tour);

/// The guide's key context: its own keys, the live tasks' keys.
pub const CONTEXT: &str = "WelcomeGuide";

/// The size of the guide's window at 100 %; a larger interface scale makes it larger (up to the
/// screen).
pub const WIDTH: f32 = 1000.;
pub const HEIGHT: f32 = 660.;
/// The bar at the bottom: Back, the dots, Skip and Next.
pub const BAR_HEIGHT: f32 = 56.;
/// The window keeps this far from the edges of the screen's free part (the menu bar, the Dock).
const SCREEN_MARGIN: f32 = 16.;
/// A slide comes in this far from the side it comes from, fading in, over this long.
const SLIDE_SHIFT: f32 = 24.;
const TRANSITION: Duration = Duration::from_millis(200);
/// On the first launch with a project, the guide waits for the project's window to draw (as
/// JetBrains IDEs wait before their tour).
const GUIDE_DELAY: Duration = Duration::from_millis(1000);

/// A slide of the tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tour {
    /// Everything from the keyboard: ⌘P, ⇧⌘F, ⇧⌘P, ⌃`.
    Search,
    /// The code understood: ⌘B, ⌥F7, ⇧F6, ⌥↵.
    Code,
    /// Version control: ⌘K, ⇧⌘K, ⌘T, ⇧⌘B.
    Git,
    /// Claude Code beside the code: ⌘Esc, ⌥⌘K.
    Claude,
    /// The tool windows: ⌘1, ⌘0, ⌥F12, ⌘9.
    Windows,
}

impl Tour {
    pub const ALL: [Tour; 5] = [
        Tour::Search,
        Tour::Code,
        Tour::Git,
        Tour::Claude,
        Tour::Windows,
    ];
}

/// A slide of the guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slide {
    /// The greeting and the interface language.
    Welcome,
    /// The theme, the look of the window, the scale.
    Appearance,
    /// The languages: their plugins from the catalog.
    Languages,
    /// Claude Code: found, signed in.
    Claude,
    Tour(Tour),
    /// The keys to remember.
    Shortcuts,
    /// "All set": open a folder, the recent projects.
    Finish,
}

/// The guide's slides, in order.
pub const SLIDES: [Slide; 11] = [
    Slide::Welcome,
    Slide::Appearance,
    Slide::Languages,
    Slide::Claude,
    Slide::Tour(Tour::Search),
    Slide::Tour(Tour::Code),
    Slide::Tour(Tour::Git),
    Slide::Tour(Tour::Claude),
    Slide::Tour(Tour::Windows),
    Slide::Shortcuts,
    Slide::Finish,
];

/// A chapter of the guide: its slides share a group of dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chapter {
    Setup,
    Tour,
    Start,
}

impl Chapter {
    pub const ALL: [Chapter; 3] = [Chapter::Setup, Chapter::Tour, Chapter::Start];

    pub fn title(self) -> &'static str {
        match self {
            Chapter::Setup => tr("Setup"),
            Chapter::Tour => tr("Tour"),
            Chapter::Start => tr("Start"),
        }
    }

    /// The chapter's first slide: where its name in the bar and Skip lead.
    pub fn first_slide(self) -> Slide {
        SLIDES
            .into_iter()
            .find(|slide| slide.chapter() == self)
            .expect("every chapter has slides")
    }

    /// The chapter after this one.
    pub fn next(self) -> Option<Chapter> {
        match self {
            Chapter::Setup => Some(Chapter::Tour),
            Chapter::Tour => Some(Chapter::Start),
            Chapter::Start => None,
        }
    }
}

impl Slide {
    pub fn chapter(self) -> Chapter {
        match self {
            Slide::Welcome | Slide::Appearance | Slide::Languages | Slide::Claude => Chapter::Setup,
            Slide::Tour(_) | Slide::Shortcuts => Chapter::Tour,
            Slide::Finish => Chapter::Start,
        }
    }

    /// The slide's place in [`SLIDES`].
    pub fn index(self) -> usize {
        SLIDES
            .iter()
            .position(|slide| *slide == self)
            .expect("every slide is in SLIDES")
    }

    /// Its name for scenarios (`FLUX_SCENARIO_WELCOME=tour-git`).
    pub fn id(self) -> &'static str {
        match self {
            Slide::Welcome => "welcome",
            Slide::Appearance => "appearance",
            Slide::Languages => "languages",
            Slide::Claude => "claude",
            Slide::Tour(Tour::Search) => "tour-search",
            Slide::Tour(Tour::Code) => "tour-code",
            Slide::Tour(Tour::Git) => "tour-git",
            Slide::Tour(Tour::Claude) => "tour-claude",
            Slide::Tour(Tour::Windows) => "tour-windows",
            Slide::Shortcuts => "shortcuts",
            Slide::Finish => "finish",
        }
    }

    #[cfg(feature = "scenario")]
    pub fn from_id(id: &str) -> Option<Slide> {
        SLIDES.into_iter().find(|slide| slide.id() == id)
    }
}

/// What the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Guide,
    /// What's New after an update, or from Help ([`whats_new`]).
    WhatsNew,
}

/// Where the guide opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    Slide(Slide),
    WhatsNew,
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("right", Next, Some(CONTEXT)),
        KeyBinding::new("enter", Next, Some(CONTEXT)),
        KeyBinding::new("left", Previous, Some(CONTEXT)),
        KeyBinding::new("escape", Close, Some(CONTEXT)),
        KeyBinding::new("cmd-w", CloseWindow, Some(CONTEXT)),
        // On the first launch the guide may be Flux's only window: ⌘Q works in it too.
        KeyBinding::new("cmd-q", workspace::Quit, Some(CONTEXT)),
    ]);
    tour::bind_keys(cx);
    // Global: from the menu bar and the palette of any window, and with no window at all. After
    // the action's own update, when the window it came from can be read (its screen).
    cx.on_action(|_: &OpenGuide, cx| {
        cx.defer(|cx| {
            open(Start::Slide(Slide::Welcome), cx);
        })
    });
    cx.on_action(|_: &ShowShortcuts, cx| {
        cx.defer(|cx| {
            if let Some(handle) = open(Start::Slide(Slide::Shortcuts), cx) {
                handle
                    .update(cx, |guide, _, cx| {
                        guide.reference = true;
                        cx.notify();
                    })
                    .ok();
            }
        })
    });
    cx.on_action(|_: &ShowWhatsNew, cx| {
        cx.defer(|cx| {
            open(Start::WhatsNew, cx);
        })
    });
}

/// The windows Flux starts with (part 10.1), and the guide: on the first launch from the Dock (no
/// project on the command line) the guide alone — the start screen opens when it closes; on the
/// first launch with a project, the project's window and the guide over it a moment later; after an
/// update, What's New. A scenario opens the guide at once with `FLUX_SCENARIO_WELCOME=<slide>` (or
/// `whats-new`). Returns the window in front.
pub fn startup(
    root: Option<PathBuf>,
    files: Vec<PathBuf>,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    let first_launch = crate::settings::first_run();
    // The version running now is written down every time: What's New compares with it.
    let updated = whats_new::after_update(cx);
    #[cfg(feature = "scenario")]
    let scenario = std::env::var("FLUX_SCENARIO_WELCOME")
        .ok()
        .map(|id| match id.as_str() {
            "whats-new" => Start::WhatsNew,
            id => Start::Slide(Slide::from_id(id).unwrap_or(Slide::Welcome)),
        });
    #[cfg(not(feature = "scenario"))]
    let scenario: Option<Start> = None;
    if first_launch
        && scenario.is_none()
        && root.is_none()
        && files.is_empty()
        && let Some(guide) = open_window(Start::Slide(Slide::Welcome), true, cx)
    {
        return Some(guide.into());
    }
    let window = workspace::open_startup_windows(root, files, cx)?;
    // Over the project's window once it has drawn and settled in front (a scenario's sooner: its
    // steps start 1.5 s after the launch).
    let (start, delay) = match scenario {
        Some(start) => (Some(start), Duration::from_millis(300)),
        None if first_launch => (Some(Start::Slide(Slide::Welcome)), GUIDE_DELAY),
        None if updated => (Some(Start::WhatsNew), GUIDE_DELAY),
        None => (None, GUIDE_DELAY),
    };
    if let Some(start) = start {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(|cx| open_window(start, first_launch && scenario.is_none(), cx))
                .ok();
        })
        .detach();
    }
    Some(window.into())
}

/// Opens the guide where `start` says, or brings the open one forward there.
pub fn open(start: Start, cx: &mut App) -> Option<WindowHandle<WelcomeGuide>> {
    open_window(start, false, cx)
}

/// The guide's window, if it is open.
fn guide_window(cx: &App) -> Option<WindowHandle<WelcomeGuide>> {
    cx.windows()
        .into_iter()
        .find_map(|window| window.downcast::<WelcomeGuide>())
}

/// Opens the guide (or brings it forward); `first_launch` — it opens by itself on the first launch:
/// `settings.json` is written at once (the guide has been shown — a quit or a crash doesn't bring
/// it back), and closing it early leaves a hint where to find it.
fn open_window(
    start: Start,
    first_launch: bool,
    cx: &mut App,
) -> Option<WindowHandle<WelcomeGuide>> {
    if let Some(handle) = guide_window(cx) {
        handle
            .update(cx, |guide, window, cx| {
                guide.go(start, window, cx);
                window.activate_window();
            })
            .ok();
        return Some(handle);
    }
    if first_launch {
        crate::settings::save_file(cx);
    }
    let scale = crate::settings::ui_scale(cx);
    // On the screen of the window in front (the one Help was chosen in), in the middle of its free
    // part.
    let display = front_display(cx).or_else(|| cx.primary_display().map(|display| display.id()));
    let room = free_room(display, cx);
    let size = window_size(scale, room.size);
    let bounds = Bounds {
        origin: room.origin
            + point(
                (room.size.width - size.width) / 2.,
                (room.size.height - size.height) / 2.,
            ),
        size,
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(window_title(start).into()),
            appears_transparent: true,
            traffic_light_position: Some(crate::ui_scale::traffic_lights(scale)),
        }),
        display_id: display,
        is_resizable: false,
        ..Default::default()
    };
    #[cfg(feature = "scenario")]
    let options = crate::scenario::window_options(options);
    let handle = cx
        .open_window(options, |window, cx| {
            ui::apply_window_style(window, cx);
            crate::ui_scale::apply(window, cx);
            cx.new(|cx| WelcomeGuide::new(start, first_launch, window, cx))
        })
        .map_err(|error| eprintln!("flux: can't open the welcome guide: {error}"))
        .ok()?;
    #[cfg(feature = "scenario")]
    crate::scenario::window_opened(handle.into());
    Some(handle)
}

/// The OS title of the window (the Window menu, Mission Control).
fn window_title(start: Start) -> &'static str {
    match start {
        Start::Slide(_) => tr("Welcome to Flux"),
        Start::WhatsNew => tr("What's New in Flux"),
    }
}

/// The screen of the window in front, or of the first Flux window.
fn front_display(cx: &mut App) -> Option<DisplayId> {
    let window = cx.active_window().or_else(|| {
        cx.windows()
            .into_iter()
            .find(|window| window.downcast::<Workspace>().is_some())
    })?;
    window
        .update(cx, |_, window, cx| {
            window.display(cx).map(|display| display.id())
        })
        .ok()
        .flatten()
}

/// The free part of a screen (without the menu bar and the Dock), from its top left corner, in
/// points.
fn free_room(display: Option<DisplayId>, cx: &App) -> Bounds<Pixels> {
    #[cfg(target_os = "macos")]
    if let Some((x, y, width, height)) = appkit::visible_frame(display.map(u32::from)) {
        return Bounds {
            origin: point(px(x as f32), px(y as f32)),
            size: size(px(width as f32), px(height as f32)),
        };
    }
    // Without AppKit's answer: the whole screen under a menu bar.
    let screen = display
        .and_then(|id| cx.find_display(id))
        .or_else(|| cx.primary_display())
        .map(|display| display.bounds().size)
        .unwrap_or(size(px(1440.), px(900.)));
    Bounds {
        origin: point(px(0.), px(ui::TITLE_BAR_HEIGHT)),
        size: size(screen.width, screen.height - px(ui::TITLE_BAR_HEIGHT)),
    }
}

/// The window's size in points at interface `scale`: the guide's size times the scale, but within
/// the screen's free part.
fn window_size(scale: f32, room: Size<Pixels>) -> Size<Pixels> {
    let fit =
        |want: f32, room: Pixels| px(want.min(f32::from(room) - 2. * SCREEN_MARGIN).max(320.));
    size(
        fit(WIDTH * scale, room.width),
        fit(HEIGHT * scale, room.height),
    )
}

/// How long a slide takes to come in. A scenario slows it down to catch the middle in a shot
/// (`FLUX_SCENARIO_TRANSITION_MS`).
fn transition_time() -> Duration {
    #[cfg(feature = "scenario")]
    if let Some(ms) = std::env::var("FLUX_SCENARIO_TRANSITION_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    TRANSITION
}

/// Animations are off — the system's "Reduce motion" or Flux's own setting (`crate::motion`): the
/// guide's transitions and the pictures' shows jump to their end instead of moving.
pub fn reduce_motion() -> bool {
    !crate::motion::enabled()
}

pub struct WelcomeGuide {
    focus_handle: FocusHandle,
    pub(crate) mode: Mode,
    pub(crate) slide: Slide,
    /// When the slide last changed: the transition runs from then.
    pub(crate) changed_at: Instant,
    /// The side the slide came in from: 1 — the right (forward), -1 — the left (back).
    direction: f32,
    /// The current slide moves in (no "Reduce motion" when it came).
    moving: bool,
    /// The Languages slide: the catalog's languages, the chosen ones, their installs.
    pub(crate) languages: Entity<languages::LanguagesStep>,
    /// The Claude Code slide: the command line tool, the sign-in.
    pub(crate) claude: Entity<claude::ClaudeStep>,
    /// The tour's live tasks: done or not, the pictures' little shows.
    pub(crate) tasks: tour::Tasks,
    /// "All set": the recent projects.
    pub(crate) finish: finish::Finish,
    /// What's New: the notes of this version.
    pub(crate) notes: Option<whats_new::Notes>,
    /// The guide opened by itself on the first launch: closing it before "All set" leaves a hint.
    first_launch: bool,
    /// Opened by Help → Keyboard Shortcuts: the keys are a reference sheet, not a step of the
    /// guide — the bar offers the guide and Close, until another slide shows.
    reference: bool,
    /// The guide is going away: [`WelcomeGuide::leave`] has run.
    left: bool,
    /// The interface scale the window's size was made for.
    sized_for: f32,
    /// The window's OS title as last set (it follows the mode and the interface language).
    title: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl WelcomeGuide {
    fn new(start: Start, first_launch: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let languages = cx.new(|cx| languages::LanguagesStep::new(window, cx));
        let claude = cx.new(|cx| claude::ClaudeStep::new(window, cx));
        // The theme, the look and the language change from this window and others: it redraws.
        let subscriptions = vec![
            cx.observe_global::<Theme>(|_, cx| cx.notify()),
            cx.observe_global::<crate::settings::Settings>(|_, cx| cx.notify()),
        ];
        // The red button closes the guide as Esc does: the start screen if nothing else is open.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            this.update(cx, |guide, cx| guide.leave(cx)).ok();
            true
        });
        let mut guide = Self {
            focus_handle,
            mode: Mode::Guide,
            slide: Slide::Welcome,
            changed_at: Instant::now(),
            direction: 1.,
            moving: false,
            languages,
            claude,
            tasks: tour::Tasks::default(),
            finish: finish::Finish::load(cx),
            notes: None,
            first_launch,
            reference: false,
            left: false,
            sized_for: window.ui_scale(),
            title: window_title(start).into(),
            _subscriptions: subscriptions,
        };
        guide.go(start, window, cx);
        guide
    }

    /// Shows the slide (or What's New) `start` says.
    pub fn go(&mut self, start: Start, window: &mut Window, cx: &mut Context<Self>) {
        match start {
            Start::Slide(slide) => {
                self.mode = Mode::Guide;
                self.reference = false;
                self.show(slide, window, cx);
            }
            Start::WhatsNew => {
                self.mode = Mode::WhatsNew;
                self.notes = whats_new::Notes::current();
                self.changed_at = Instant::now();
                self.moving = false;
                window.focus(&self.focus_handle);
                cx.notify();
            }
        }
    }

    /// Shows a slide of the guide: it comes in from the side it is on.
    pub fn show(&mut self, slide: Slide, window: &mut Window, cx: &mut Context<Self>) {
        if slide != Slide::Shortcuts {
            self.reference = false;
        }
        if self.slide != slide {
            self.direction = if slide.index() > self.slide.index() {
                1.
            } else {
                -1.
            };
            self.slide = slide;
            self.changed_at = Instant::now();
            self.moving = !reduce_motion();
        }
        if let Slide::Tour(tour) = slide {
            self.tasks.shown(tour);
        }
        window.focus(&self.focus_handle);
        // The render asks for the transition's frames (`request_animation_frame` works only while
        // a view draws).
        cx.notify();
    }

    /// Continue: the next slide (the chosen languages start installing on the way); after the last
    /// one — and in What's New — the guide closes.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::WhatsNew || self.reference {
            return self.close(window, cx);
        }
        if self.slide == Slide::Languages {
            self.languages
                .update(cx, |step, cx| step.install_chosen(window, cx));
        }
        match SLIDES.get(self.slide.index() + 1) {
            Some(next) => self.show(*next, window, cx),
            None => self.close(window, cx),
        }
    }

    fn previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::WhatsNew || self.reference {
            return;
        }
        if let Some(index) = self.slide.index().checked_sub(1) {
            self.show(SLIDES[index], window, cx);
        }
    }

    /// Skip: the next chapter's first slide (Setup → the tour, the tour → "All set").
    fn skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chapter) = self.slide.chapter().next() {
            self.show(chapter.first_slide(), window, cx);
        }
    }

    /// Closes the guide (Esc, ⌘W, "All set"'s buttons, Continue in What's New). Alone on the
    /// first launch, it leaves the start screen behind.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave(cx);
        window.remove_window();
    }

    /// The guide is going away. Alone on the first launch, it opens the start screen (Flux starts
    /// there); any other last window going away quits Flux as usual. Closed before "All set" after
    /// it opened by itself, it leaves word where it lives (in Help).
    fn leave(&mut self, cx: &mut App) {
        if std::mem::replace(&mut self.left, true) {
            return;
        }
        let windows: Vec<WindowHandle<Workspace>> = cx
            .windows()
            .into_iter()
            .filter_map(|window| window.downcast::<Workspace>())
            .collect();
        let opened = if windows.is_empty() && self.first_launch {
            workspace::open_startup_windows(None, Vec::new(), cx)
        } else {
            None
        };
        let early = self.first_launch && self.mode == Mode::Guide && self.slide != Slide::Finish;
        if !early {
            return;
        }
        let front = cx
            .active_window()
            .and_then(|window| window.downcast::<Workspace>())
            .filter(|window| windows.contains(window));
        let Some(target) = opened.or(front).or_else(|| windows.first().copied()) else {
            return;
        };
        let hint = Notification::info(tr("The guide is always at hand"))
            .body(tr("Help → Welcome Guide"))
            .action(tr("Open Guide"), OpenGuide);
        target
            .update(cx, |workspace, _, cx| workspace.notify(hint, cx))
            .ok();
    }

    /// The guide's own keys (↵, ← →, Esc) are its: the focus is on the guide itself, not on
    /// something on a slide that takes keys (a terminal) — then the key goes on to that.
    fn has_keys(&self, window: &Window, cx: &mut Context<Self>) -> bool {
        let own = self.focus_handle.is_focused(window);
        if !own {
            cx.propagate();
        }
        own
    }

    fn try_task(&mut self, tour: Tour, cx: &mut Context<Self>) {
        if self.mode == Mode::Guide && self.slide == Slide::Tour(tour) {
            self.tasks.try_task(tour);
            cx.notify();
        }
    }

    /// How far the current slide's transition is: 0 — it starts, 1 — in place (eased out).
    fn transition(&self) -> f32 {
        if !self.moving {
            return 1.;
        }
        let t = self.changed_at.elapsed().as_secs_f32() / transition_time().as_secs_f32();
        if t >= 1. { 1. } else { 1. - (1. - t).powi(3) }
    }

    /// The interface scale changed while the guide is open: the window takes the guide's size at
    /// the new scale (within the screen), around the same middle.
    fn follow_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scale = window.ui_scale();
        if (scale - self.sized_for).abs() < 0.001 {
            return;
        }
        self.sized_for = scale;
        let display = window.display(cx).map(|display| display.id());
        let size = window_size(scale, free_room(display, cx).size);
        let title = self.title.clone();
        // From a task of its own, outside any update of the app: AppKit resizes the window at once
        // and tells gpui, which then updates the window (as gpui's own `Window::resize` does).
        cx.spawn(async move |_, _| {
            #[cfg(target_os = "macos")]
            appkit::resize_centered(&title, f32::from(size.width), f32::from(size.height));
            #[cfg(not(target_os = "macos"))]
            let _ = (title, size);
        })
        .detach();
    }

    /// The window's title: the guide, the keys as a reference sheet, What's New.
    fn title(&self) -> &'static str {
        match self.mode {
            Mode::Guide if self.reference => tr("Keyboard Shortcuts"),
            Mode::Guide => tr("Welcome to Flux"),
            Mode::WhatsNew => tr("What's New in Flux"),
        }
    }

    /// The window's OS title in the current mode and language.
    fn follow_title(&mut self, window: &mut Window) {
        let title = self.title();
        if self.title.as_ref() != title {
            self.title = title.into();
            window.set_window_title(title);
        }
    }

    /// The slide's content.
    fn render_slide(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.mode == Mode::WhatsNew {
            return whats_new::render(self, window, cx);
        }
        match self.slide {
            Slide::Welcome => intro::render(self, window, cx),
            Slide::Appearance => appearance::render(self, window, cx),
            Slide::Languages => self.languages.clone().into_any_element(),
            Slide::Claude => self.claude.clone().into_any_element(),
            Slide::Tour(tour) => tour::render(tour, self, window, cx),
            Slide::Shortcuts => shortcuts::render(self, window, cx),
            Slide::Finish => finish::render(self, window, cx),
        }
    }

    /// The label of the main button at the bottom: Get Started, Continue, or what the slide says
    /// (Install 4 and Continue); none on "All set" (its own buttons lead on).
    fn next_label(&self, cx: &App) -> Option<SharedString> {
        match self.slide {
            Slide::Welcome => Some(tr("Get Started").into()),
            Slide::Languages => Some(
                self.languages
                    .read(cx)
                    .next_label()
                    .unwrap_or_else(|| tr("Continue").into()),
            ),
            Slide::Finish => None,
            _ => Some(tr("Continue").into()),
        }
    }

    /// The dots at the bottom, grouped by chapter, with the chapters' names: the current chapter
    /// bright, the current slide's dot long, the slides passed tinted. A dot shows its slide, a
    /// chapter's name its first slide.
    fn render_dots(&self, ui: UiColors, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.slide.index();
        div()
            .flex()
            .items_center()
            .gap_1()
            .children(Chapter::ALL.into_iter().map(|chapter| {
                let here = self.slide.chapter() == chapter;
                let dots = SLIDES
                    .into_iter()
                    .enumerate()
                    .filter(|(_, slide)| slide.chapter() == chapter)
                    .map(|(index, slide)| {
                        let on = index == current;
                        // A dot's own room is larger than the dot: easy to hit.
                        div()
                            .id(SharedString::from(format!("welcome-dot-{}", slide.id())))
                            .h(px(16.))
                            .flex()
                            .items_center()
                            .px(px(2.))
                            .cursor_pointer()
                            .group("welcome-dot")
                            .child(
                                div()
                                    .h(px(7.))
                                    .w(px(if on { 22. } else { 7. }))
                                    .rounded(px(4.))
                                    .bg(if on {
                                        ui.accent
                                    } else if index < current {
                                        UiColors::tint(ui.accent, 0.55)
                                    } else {
                                        UiColors::tint(ui.text_muted, 0.35)
                                    })
                                    .when(!on, |dot| {
                                        dot.group_hover("welcome-dot", move |style| {
                                            style.bg(UiColors::tint(ui.accent, 0.8))
                                        })
                                    }),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.show(slide, window, cx)
                            }))
                    });
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("welcome-chapter-{chapter:?}")))
                            .ml_2p5()
                            .mr_1()
                            .px_1()
                            .rounded(px(ui::RADIUS_XS))
                            .cursor_pointer()
                            .text_size(px(theme::TEXT_XS + 0.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if here { ui.foreground } else { ui.dim })
                            .hover(move |style| style.text_color(ui.foreground))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.show(chapter.first_slide(), window, cx)
                            }))
                            .child(chapter.title()),
                    )
                    .children(dots)
            }))
    }

    /// The bar of the keys opened as a reference sheet (Help → Keyboard Shortcuts): the whole
    /// guide on the left, Close on the right.
    fn render_reference_bar(&self, ui: UiColors, cx: &mut Context<Self>) -> Div {
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(BAR_HEIGHT))
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .child(
                ghost_button(
                    "welcome-whole-guide",
                    Some(IconName::Logo),
                    tr("Welcome Guide"),
                    ui,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.show(Slide::Welcome, window, cx)
                })),
            )
            .child(
                cta_button("welcome-close", tr("Close"), None, ui).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx)),
                ),
            )
    }

    /// The bar on the frame under the slide: Back, the dots, Skip and the main button.
    fn render_bar(&self, ui: UiColors, cx: &mut Context<Self>) -> AnyElement {
        if self.reference {
            return self.render_reference_bar(ui, cx).into_any_element();
        }
        let first = self.slide.index() == 0;
        let skip = match self.slide.chapter() {
            Chapter::Setup => Some(tr("Skip Setup")),
            Chapter::Tour => Some(tr("Skip Tour")),
            Chapter::Start => None,
        };
        let back = (!first).then(|| {
            ghost_button("welcome-back", Some(IconName::ArrowLeft), tr("Back"), ui)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.previous(window, cx)))
        });
        let next = self.next_label(cx).map(|label| {
            cta_button("welcome-next", label, Some(IconName::ArrowRight), ui)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.next(window, cx)))
        });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(BAR_HEIGHT))
            .px_4()
            .flex()
            .items_center()
            .child(
                // Both sides take the same room: the dots stay in the middle of the window.
                div().flex_1().flex().items_center().children(back),
            )
            .child(self.render_dots(ui, cx))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .children(skip.map(|label| {
                        ghost_button("welcome-skip", None, label, ui).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.skip(window, cx)),
                        )
                    }))
                    .children(next),
            )
            .into_any_element()
    }
}

impl Focusable for WelcomeGuide {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WelcomeGuide {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let style = ui::style(cx);
        let flat = style == crate::settings::UiStyle::Flat;
        self.follow_scale(window, cx);
        self.follow_title(window);
        let transition = self.transition();
        if transition < 1. || self.tasks.animating() {
            window.request_animation_frame();
        }
        let content = self.render_slide(window, cx);
        let gap = ui::island_gap(style);
        // A window the screen made smaller than the guide scrolls its slide instead of squeezing it.
        let bar = match self.mode {
            Mode::Guide => BAR_HEIGHT,
            Mode::WhatsNew => gap,
        };
        let room = window.viewport_size().height - px(ui::TITLE_BAR_HEIGHT) - px(bar);
        let height = room.max(px(HEIGHT - ui::TITLE_BAR_HEIGHT - bar));
        let slide = div()
            .id("welcome-slide")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(height)
                    .left(px((1. - transition) * SLIDE_SHIFT * self.direction))
                    .opacity(transition)
                    .child(content),
            );
        let title = self.title();
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            // ↵, ← → and Esc belong to the guide only while it has the focus itself: typed into
            // something on a slide (the sign-in's terminal), they go on to it.
            .on_action(cx.listener(|this, _: &Next, window, cx| {
                if this.has_keys(window, cx) {
                    this.next(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Previous, window, cx| {
                if this.has_keys(window, cx) {
                    this.previous(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Close, window, cx| {
                if this.has_keys(window, cx) {
                    this.close(window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, action: &TryTask, _, cx| this.try_task(action.0, cx)))
            .size_full()
            .relative()
            .bg(ui.frame)
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_MD))
            .text_color(ui.foreground)
            .when(!flat, |root| root.child(ui::frame_glow(ui)))
            .child(
                div()
                    .h(px(ui::TITLE_BAR_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_size(px(theme::TEXT_SM + 0.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui.text_muted)
                    .child(icon(IconName::Logo, ui.accent_text).size(px(13.)))
                    .child(title),
            )
            .child(
                ui::island(ui, style)
                    .absolute()
                    .top(px(ui::TITLE_BAR_HEIGHT))
                    .left(px(gap))
                    .right(px(gap))
                    // What's New draws its own bar inside the island.
                    .bottom(px(match self.mode {
                        Mode::Guide => BAR_HEIGHT,
                        Mode::WhatsNew => gap,
                    }))
                    .overflow_hidden()
                    // Flat: the panel between lines, as the main window's panels are.
                    .when(flat, |island| {
                        island
                            .border_t_1()
                            .border_b_1()
                            .border_color(ui.island_border)
                    })
                    .child(slide),
            )
            .when(self.mode == Mode::Guide, |root| {
                root.child(self.render_bar(ui, cx))
            })
    }
}

/// A slide's heading, the same on every setup and tour slide: the eyebrow (the chapter and the
/// step), the title, a sentence or two.
pub fn heading(
    eyebrow: impl Into<SharedString>,
    title: impl Into<SharedString>,
    lead: impl Into<SharedString>,
    ui: UiColors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui.accent_text)
                .child(eyebrow.into().to_uppercase()),
        )
        .child(
            div()
                .text_size(px(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .text_size(px(14.))
                .line_height(px(21.))
                .text_color(ui.text_muted)
                .child(lead.into()),
        )
}

/// The guide's main button: solid accent, larger than a window's `ui::primary_button` — the one
/// thing to press on a slide (Continue, Get Started, Open Folder…). `icon` goes after the label.
pub fn cta_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon_name: Option<IconName>,
    ui: UiColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(32.))
        .px(px(16.))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(ui::RADIUS_MD))
        .bg(ui.accent)
        .text_color(ui.on_accent)
        .text_size(px(theme::TEXT_MD))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .shadow(vec![BoxShadow {
            color: UiColors::tint(ui.accent, 0.35),
            offset: point(px(0.), px(4.)),
            blur_radius: px(14.),
            spread_radius: px(-4.),
        }])
        .hover(move |style| style.opacity(0.9))
        .active(move |style| style.opacity(0.8))
        .child(label.into())
        .children(icon_name.map(|name| icon(name, ui.on_accent).size(px(14.))))
}

/// A quiet button of the guide's bar: text without a border (Back, Skip); `icon` goes before it.
pub fn ghost_button(
    id: impl Into<ElementId>,
    icon_name: Option<IconName>,
    label: impl Into<SharedString>,
    ui: UiColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(30.))
        .px(px(12.))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(ui::RADIUS_MD))
        .text_color(ui.text_muted)
        .text_size(px(theme::TEXT_MD))
        .cursor_pointer()
        .group("welcome-ghost")
        .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
        .active(move |style| style.bg(ui.pressed))
        .children(icon_name.map(|name| {
            icon(name, ui.text_muted)
                .size(px(14.))
                .group_hover("welcome-ghost", move |style| {
                    style.text_color(ui.foreground)
                })
        }))
        .child(label.into())
}

/// AppKit: the screens' free parts, "Reduce motion", and moving the guide's window.
#[cfg(target_os = "macos")]
// `msg_send!` of objc 0.2 checks a `cargo-clippy` feature this crate doesn't declare.
#[allow(unexpected_cfgs)]
mod appkit {
    use std::ffi::{CStr, c_char};

    use objc::runtime::{Object, YES};
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
    struct Size {
        width: f64,
        height: f64,
    }
    unsafe impl objc::Encode for Size {
        fn encode() -> objc::Encoding {
            unsafe { objc::Encoding::from_str("{CGSize=dd}") }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    unsafe impl objc::Encode for Rect {
        fn encode() -> objc::Encoding {
            unsafe { objc::Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
        }
    }

    /// The screen with this display id (`NSScreenNumber`), else the main one.
    ///
    /// SAFETY: on the main thread.
    unsafe fn screen(display: Option<u32>) -> *mut Object {
        unsafe {
            if let Some(display) = display {
                let screens: *mut Object = msg_send![class!(NSScreen), screens];
                let count: usize = msg_send![screens, count];
                let key: *mut Object =
                    msg_send![class!(NSString), stringWithUTF8String: c"NSScreenNumber".as_ptr()];
                for index in 0..count {
                    let screen: *mut Object = msg_send![screens, objectAtIndex: index];
                    let description: *mut Object = msg_send![screen, deviceDescription];
                    let number: *mut Object = msg_send![description, objectForKey: key];
                    if number.is_null() {
                        continue;
                    }
                    let id: u32 = msg_send![number, unsignedIntValue];
                    if id == display {
                        return screen;
                    }
                }
            }
            msg_send![class!(NSScreen), mainScreen]
        }
    }

    /// The free part of a screen (without the menu bar and the Dock) from the screen's top left
    /// corner, in points: x, y, width, height.
    pub fn visible_frame(display: Option<u32>) -> Option<(f64, f64, f64, f64)> {
        // SAFETY: AppKit's screens and their frames, on the main thread; a nil screen is checked.
        unsafe {
            let screen = screen(display);
            if screen.is_null() {
                return None;
            }
            let frame: Rect = msg_send![screen, frame];
            let visible: Rect = msg_send![screen, visibleFrame];
            // AppKit counts from the bottom left corner, y going up.
            let x = visible.origin.x - frame.origin.x;
            let y = (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height);
            Some((x, y, visible.size.width, visible.size.height))
        }
    }

    /// The app's window with this title: its content becomes `width` × `height` points, its middle
    /// stays where it was, and it stays within its screen's free part.
    pub fn resize_centered(title: &str, width: f32, height: f32) {
        // SAFETY: AppKit's own windows, their titles and frames, on the main thread; nil and null
        // results are checked.
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let windows: *mut Object = msg_send![app, windows];
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let window: *mut Object = msg_send![windows, objectAtIndex: index];
                let name: *mut Object = msg_send![window, title];
                if name.is_null() {
                    continue;
                }
                let utf8: *const c_char = msg_send![name, UTF8String];
                if utf8.is_null() || CStr::from_ptr(utf8).to_str() != Ok(title) {
                    continue;
                }
                let frame: Rect = msg_send![window, frame];
                let content: Rect = msg_send![window, contentRectForFrameRect: frame];
                let wanted = Rect {
                    origin: content.origin,
                    size: Size {
                        width: f64::from(width),
                        height: f64::from(height),
                    },
                };
                let mut next: Rect = msg_send![window, frameRectForContentRect: wanted];
                next.origin.x = frame.origin.x + (frame.size.width - next.size.width) / 2.;
                next.origin.y = frame.origin.y + (frame.size.height - next.size.height) / 2.;
                let screen: *mut Object = msg_send![window, screen];
                if !screen.is_null() {
                    let room: Rect = msg_send![screen, visibleFrame];
                    let right = room.origin.x + room.size.width - next.size.width;
                    let top = room.origin.y + room.size.height - next.size.height;
                    next.origin.x = next.origin.x.min(right).max(room.origin.x);
                    next.origin.y = next.origin.y.min(top).max(room.origin.y);
                }
                let _: () = msg_send![window, setFrame: next display: YES];
                return;
            }
        }
    }
}
