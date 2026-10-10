//! The theme is data: UI colors and highlight scope styles. It is set globally (`cx.set_global`)
//! and read via [`Theme::get`] and [`Theme::ui`]. Font metrics, tab width, and blinking are
//! constants: they are future settings, not part of the theme.
//!
//! UI colors are the design system's tokens (wiki: "Design System"): glass surfaces (window frame,
//! islands, popovers), text at three levels, accent, states, and a palette of shades that carry
//! meaning (file types, categories, counters). Components are in `ui.rs`.
//!
//! Stage 8.3: themes are data in files (`docs/themes.md`), brought by plugins (`[[themes]]`): the
//! bundled `flux.themes` has Flux Night and Flux Day. [`load_plugin_themes`] reads the themes of the
//! turned-on plugins, [`apply`] puts the chosen one (Settings → Appearance → Theme, or the light /
//! dark one when it follows macOS) into the global [`Theme`]; [`preview`] shows another one for a
//! moment (Quick Switch). Flux Night from the code is the fallback when the chosen theme is gone.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use flux_plugin::registry::PluginEntry;
use flux_syntax::Highlight;
use gpui::{App, Global, Hsla, SharedString, rgb, rgba};

use crate::settings;

/// The UI font is the system font (SF Pro on macOS).
pub const UI_FONT: &str = ".SystemUIFont";
/// The code font is the first installed one from the list ([`init_fonts`]).
const CODE_FONTS: [&str; 4] = [
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "SF Mono",
    "Menlo",
];
pub const FONT_SIZE: f32 = 14.;
pub const LINE_HEIGHT: f32 = 21.;
pub const TAB_WIDTH: usize = 4;
/// Padding between the gutter and the text.
pub const TEXT_PADDING: f32 = 8.;
/// How many lines to keep between the cursor and the window edge when auto-scrolling.
pub const SCROLL_MARGIN_LINES: usize = 3;
/// Cursor blink period; `None` means the cursor does not blink.
pub const CURSOR_BLINK: Option<Duration> = Some(Duration::from_millis(500));

/// Terminal text: the code font, a little smaller and denser than the editor's.
pub const TERMINAL_FONT_SIZE: f32 = 13.;
pub const TERMINAL_LINE_HEIGHT: f32 = 18.;

/// UI font sizes (for the [`UI_FONT`] font).
pub const TEXT_XS: f32 = 11.;
pub const TEXT_SM: f32 = 12.;
pub const TEXT_MD: f32 = 13.;
pub const TEXT_LG: f32 = 15.;
pub const TEXT_XL: f32 = 20.;

static CODE_FONT: OnceLock<&'static str> = OnceLock::new();

/// Picks the code font: the first installed one from [`CODE_FONTS`]. Without this check, gpui would
/// silently substitute a proportional system font.
pub fn init_fonts(cx: &App) {
    let installed = cx.text_system().all_font_names();
    let family = CODE_FONTS
        .into_iter()
        .find(|family| installed.iter().any(|name| name == family))
        .unwrap_or("Menlo");
    CODE_FONT.set(family).ok();
}

/// The code font: editor, search fields, result rows.
pub fn code_font() -> &'static str {
    CODE_FONT.get().copied().unwrap_or("Menlo")
}

/// The UI color tokens, each by its name in a theme file: [`UiColors`] and the table of tokens
/// come from one list, so a theme file can name every token and nothing else.
macro_rules! ui_colors {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        /// UI colors. `Copy`: read from the global theme as a single value.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct UiColors {
            $($(#[$doc])* pub $name: Hsla,)*

            // --- Git history (stage 6.3) ---
            /// The commit graph of the log: a lane's color by its number (wraps around).
            pub graph_lanes: [Hsla; 8],
        }

        /// The names of the tokens in a theme file's `[ui]`, besides `graph_lanes`.
        pub const UI_TOKENS: &[&str] = &[$(stringify!($name)),*];

        impl UiColors {
            /// The token named `name` in a theme file.
            fn token_mut(&mut self, name: &str) -> Option<&mut Hsla> {
                match name {
                    $(stringify!($name) => Some(&mut self.$name),)*
                    _ => None,
                }
            }

            /// Every token in one color: what a theme file fills in.
            fn filled(color: Hsla) -> Self {
                Self {
                    $($name: color,)*
                    graph_lanes: [color; 8],
                }
            }
        }
    };
}

ui_colors! {
    // --- Surfaces (glass). The alpha lets the blurred desktop show through. ---
    /// Window frame: the background under the islands, the title bar, and the status bar.
    frame,
    /// Colored glow of the frame (a gradient from the top-left corner).
    frame_glow,
    /// Island: a self-contained panel (tree, editor).
    island,
    island_border,
    /// Glass sheen along the top edge of an island and of a popover.
    sheen,
    /// Popovers: menus, pickers, project search, tooltips.
    elevated,
    elevated_border,
    shadow,
    /// The veil over the window under a question dialog.
    backdrop,
    /// Thin dividers inside the islands.
    divider,

    // --- Text ---
    foreground,
    /// Secondary text: paths, labels, inactive tabs.
    text_muted,
    /// Tertiary: line numbers, hints, placeholders.
    dim,
    text_disabled,

    // --- Interaction ---
    accent,
    /// Accent for text and icons on dark backgrounds (lighter than the main accent).
    accent_text,
    /// Accent background: an enabled toggle, an action icon.
    accent_soft,
    /// Text and icons on a solid accent fill: a checked box, a switch's knob.
    on_accent,
    hover,
    pressed,
    /// Selected list row while the list has focus.
    list_selected,
    /// Selected row without focus: the file of the active tab in the tree.
    list_selected_inactive,
    input_background,
    input_border,
    /// Border of a focused field and the ring around it.
    focus_border,
    focus_ring,
    /// The directory the dragged file is about to be dropped on (file tree).
    drop_target,
    /// A key in a shortcut hint.
    keycap,
    keycap_border,

    // --- States ---
    success,
    warning,
    error,
    info,
    /// Unsaved changes: the dot on a tab, the marker in the status bar.
    modified,

    // --- Editor ---
    current_line,
    selection,
    cursor,
    /// Matched characters in lists: fuzzy search, project search results.
    match_text,
    /// Background of the matches found in the text, and of the current match.
    search_match,
    search_match_active,

    // --- Version control: a file's change against HEAD (names in the tree, tabs, the commit
    // window), as JetBrains IDEs color them. ---
    vcs_modified,
    vcs_added,
    vcs_deleted,
    vcs_renamed,
    /// Not tracked: "Unversioned Files".
    vcs_untracked,
    vcs_conflict,

    // --- Diffs: markers in the editor's gutter (solid), blocks in the diff viewer (background),
    // changed words inside a modified block (a stronger background). Added is green, modified blue,
    // deleted gray, as in JetBrains IDEs. ---
    diff_added,
    diff_modified,
    diff_deleted,
    diff_added_bg,
    diff_modified_bg,
    diff_deleted_bg,
    diff_added_word,
    diff_modified_word,
    diff_deleted_word,
    /// The merge tool: a conflict (red, as in JetBrains IDEs), and a change already taken into the
    /// result (quieter than an open one).
    diff_conflict,
    diff_conflict_bg,
    diff_conflict_word,
    diff_resolved_bg,
    /// Annotations (blame): the gutter column's background for the newest commit of the file; older
    /// ones fade towards nothing.
    blame_recent,

    // --- Shade palette: meaning, not decoration (file types, categories, counters). ---
    blue,
    indigo,
    violet,
    pink,
    red,
    orange,
    amber,
    lime,
    green,
    teal,
    cyan,
    /// Directory icon.
    folder,
}

impl UiColors {
    /// Returns `color` with opacity `alpha`, for shade backgrounds (badges, icon tiles).
    pub fn tint(color: Hsla, alpha: f32) -> Hsla {
        Hsla { a: alpha, ..color }
    }
}

/// How to draw a highlight scope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntaxStyle {
    pub color: Hsla,
    pub bold: bool,
    pub italic: bool,
}

/// Terminal colors: the defaults and the 16 ANSI colors programs pick from (`ls`, `git`, prompts).
/// The 256-color table and true colors are computed, not themed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalColors {
    pub foreground: Hsla,
    /// The background programs assume (inverse video, answers to color queries). The terminal
    /// itself is transparent: cells on the default background show the island.
    pub background: Hsla,
    pub cursor: Hsla,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then the same eight bright.
    pub ansi: [Hsla; 16],
}

/// Which macOS appearance a theme is made for: the window's glass and the choice of a theme when it
/// follows the system.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// The name people choose it by, also its key in the settings: "Flux Night".
    pub name: SharedString,
    pub appearance: Appearance,
    pub ui: UiColors,
    pub terminal: TerminalColors,
    /// Highlight scopes: names like tree-sitter captures (`keyword`, `function.method`). A general
    /// name covers the specific ones: `function` also applies to `function.method` if the latter
    /// has no entry of its own.
    pub syntax: Vec<(String, SyntaxStyle)>,
}

impl Global for Theme {}

impl Theme {
    pub fn get(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    pub fn ui(cx: &App) -> UiColors {
        Self::get(cx).ui
    }

    /// Scope names in order, for `flux_syntax::HighlightMap`; the index in this list is the
    /// [`Highlight`].
    pub fn syntax_scopes(&self) -> Vec<&str> {
        self.syntax
            .iter()
            .map(|(scope, _)| scope.as_str())
            .collect()
    }

    pub fn syntax_style(&self, highlight: Highlight) -> Option<SyntaxStyle> {
        self.syntax.get(highlight.0).map(|(_, style)| *style)
    }

    /// The dark theme "Flux Night", the base of dark themes and the fallback: the bundled plugin's
    /// file (`plugins/themes/themes/flux-night.toml`), read once.
    pub fn flux_night() -> Self {
        static NIGHT: OnceLock<Theme> = OnceLock::new();
        NIGHT
            .get_or_init(|| {
                parse_base(FLUX_NIGHT_FILE).expect("the bundled Flux Night is a complete theme")
            })
            .clone()
    }

    /// The light theme "Flux Day", the base of light themes: the bundled plugin's file, read once.
    pub fn flux_day() -> Self {
        static DAY: OnceLock<Theme> = OnceLock::new();
        DAY.get_or_init(|| {
            parse_base(FLUX_DAY_FILE).expect("the bundled Flux Day is a complete theme")
        })
        .clone()
    }

    /// The base of a theme of this appearance: what a theme file doesn't name comes from it.
    pub fn base(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => Self::flux_night(),
            Appearance::Light => Self::flux_day(),
        }
    }

    /// The style of a highlight scope by name, falling back along the dots as highlighting does
    /// (`function.method` → `function`): a theme's preview of code.
    pub fn style_for(&self, scope: &str) -> Option<SyntaxStyle> {
        let mut name = scope;
        loop {
            if let Some((_, style)) = self.syntax.iter().find(|(known, _)| known == name) {
                return Some(*style);
            }
            name = name.rsplit_once('.')?.0;
        }
    }
}

/// The default dark theme, also the fallback.
pub const FLUX_NIGHT: &str = "Flux Night";
/// The default light theme.
pub const FLUX_DAY: &str = "Flux Day";

/// The files of the bundled themes (the plugin `flux.themes`): the bases of dark and light themes.
const FLUX_NIGHT_FILE: &str = include_str!("../../../plugins/themes/themes/flux-night.toml");
const FLUX_DAY_FILE: &str = include_str!("../../../plugins/themes/themes/flux-day.toml");

/// A theme file read, not yet applied to a base.
struct ThemeFile {
    name: String,
    appearance: Appearance,
    ui: toml::Table,
    syntax: toml::Table,
    terminal: toml::Table,
}

/// Reads a theme file (`docs/themes.md`): what it doesn't name comes from the base of its
/// appearance — Flux Night for a dark theme, Flux Day for a light one.
pub fn parse_theme(text: &str) -> Result<Theme, String> {
    let file = read_theme_file(text)?;
    let mut theme = Theme::base(file.appearance);
    apply_theme_file(file, &mut theme, false)?;
    Ok(theme)
}

/// A theme that names everything: the bundled bases.
fn parse_base(text: &str) -> Result<Theme, String> {
    let file = read_theme_file(text)?;
    let unset: Hsla = gpui::transparent_black();
    let mut theme = Theme {
        name: SharedString::default(),
        appearance: file.appearance,
        ui: UiColors::filled(unset),
        terminal: TerminalColors {
            foreground: unset,
            background: unset,
            cursor: unset,
            ansi: [unset; 16],
        },
        syntax: Vec::new(),
    };
    apply_theme_file(file, &mut theme, true)?;
    Ok(theme)
}

fn read_theme_file(text: &str) -> Result<ThemeFile, String> {
    let mut table: toml::Table = text
        .parse()
        .map_err(|err: toml::de::Error| err.message().trim().to_string())?;
    if let Some(key) = table
        .keys()
        .find(|key| !matches!(key.as_str(), "name" | "appearance" | "ui" | "syntax" | "terminal"))
    {
        return Err(format!(
            "unknown key `{key}` (name, appearance, [ui], [syntax], [terminal])"
        ));
    }
    let name = match table.remove("name") {
        Some(toml::Value::String(name)) if !name.trim().is_empty() => name,
        _ => return Err("`name` is missing: the name people choose the theme by".into()),
    };
    let appearance = match table.remove("appearance") {
        Some(toml::Value::String(appearance)) if appearance == "dark" => Appearance::Dark,
        Some(toml::Value::String(appearance)) if appearance == "light" => Appearance::Light,
        _ => return Err("`appearance` is \"dark\" or \"light\"".into()),
    };
    let mut section = |key: &str| match table.remove(key) {
        None => Ok(toml::Table::new()),
        Some(toml::Value::Table(section)) => Ok(section),
        Some(_) => Err(format!("`{key}` is a table: [{key}]")),
    };
    Ok(ThemeFile {
        name,
        appearance,
        ui: section("ui")?,
        syntax: section("syntax")?,
        terminal: section("terminal")?,
    })
}

/// Puts what the file names into `theme`. `complete`: the file must name every token, the whole
/// terminal and some scopes (a base theme).
fn apply_theme_file(file: ThemeFile, theme: &mut Theme, complete: bool) -> Result<(), String> {
    theme.name = file.name.into();
    theme.appearance = file.appearance;
    for (key, value) in &file.ui {
        if key == "graph_lanes" {
            theme.ui.graph_lanes = colors(value, "ui.graph_lanes")?;
            continue;
        }
        let slot = theme
            .ui
            .token_mut(key)
            .ok_or_else(|| format!("ui: unknown token `{key}`"))?;
        *slot = color(value, &format!("ui.{key}"))?;
    }
    for (scope, value) in &file.syntax {
        let valid = !scope.is_empty()
            && scope
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
        if !valid {
            return Err(format!(
                "syntax: `{scope}` — a highlight scope, like \"keyword\" or \"function.method\""
            ));
        }
        let style = syntax_style(value, &format!("syntax.{scope}"))?;
        match theme.syntax.iter_mut().find(|(known, _)| known == scope) {
            Some((_, slot)) => *slot = style,
            None => theme.syntax.push((scope.clone(), style)),
        }
    }
    for (key, value) in &file.terminal {
        let what = format!("terminal.{key}");
        match key.as_str() {
            "foreground" => theme.terminal.foreground = color(value, &what)?,
            "background" => theme.terminal.background = color(value, &what)?,
            "cursor" => theme.terminal.cursor = color(value, &what)?,
            "ansi" => theme.terminal.ansi = colors(value, &what)?,
            _ => {
                return Err(format!(
                    "terminal: unknown key `{key}` (foreground, background, cursor, ansi)"
                ));
            }
        }
    }
    if complete {
        let missing: Vec<&str> = UI_TOKENS
            .iter()
            .copied()
            .chain(["graph_lanes"])
            .filter(|token| !file.ui.contains_key(*token))
            .collect();
        if !missing.is_empty() {
            return Err(format!("ui: missing {}", missing.join(", ")));
        }
        for key in ["foreground", "background", "cursor", "ansi"] {
            if !file.terminal.contains_key(key) {
                return Err(format!("terminal: missing {key}"));
            }
        }
        if theme.syntax.is_empty() {
            return Err("syntax: no scopes".into());
        }
    }
    Ok(())
}

/// "#rrggbb" or "#rrggbbaa".
fn color(value: &toml::Value, what: &str) -> Result<Hsla, String> {
    let wrong = || format!("{what}: a color is \"#rrggbb\" or \"#rrggbbaa\"");
    let text = value.as_str().ok_or_else(wrong)?;
    let hex = text.strip_prefix('#').ok_or_else(wrong)?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(wrong());
    }
    let number = u32::from_str_radix(hex, 16).map_err(|_| wrong())?;
    match hex.len() {
        6 => Ok(rgb(number).into()),
        8 => Ok(rgba(number).into()),
        _ => Err(wrong()),
    }
}

/// A list of `N` colors.
fn colors<const N: usize>(value: &toml::Value, what: &str) -> Result<[Hsla; N], String> {
    let items = value
        .as_array()
        .filter(|items| items.len() == N)
        .ok_or_else(|| format!("{what}: a list of {N} colors"))?;
    let mut colors = [gpui::transparent_black(); N];
    for (index, item) in items.iter().enumerate() {
        colors[index] = color(item, &format!("{what}[{index}]"))?;
    }
    Ok(colors)
}

/// A color, or `{ color = "#…", bold = true, italic = true }`.
fn syntax_style(value: &toml::Value, what: &str) -> Result<SyntaxStyle, String> {
    if value.is_str() {
        return Ok(SyntaxStyle {
            color: color(value, what)?,
            bold: false,
            italic: false,
        });
    }
    let table = value
        .as_table()
        .ok_or_else(|| format!("{what}: a color or {{ color, bold, italic }}"))?;
    let mut style = SyntaxStyle {
        color: color(
            table
                .get("color")
                .ok_or_else(|| format!("{what}: no color"))?,
            what,
        )?,
        bold: false,
        italic: false,
    };
    for (key, value) in table {
        match key.as_str() {
            "color" => {}
            "bold" | "italic" => {
                let on = value
                    .as_bool()
                    .ok_or_else(|| format!("{what}.{key}: true or false"))?;
                if key == "bold" {
                    style.bold = on;
                } else {
                    style.italic = on;
                }
            }
            _ => return Err(format!("{what}: unknown key `{key}` (color, bold, italic)")),
        }
    }
    Ok(style)
}

/// A theme that may be chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeInfo {
    pub name: SharedString,
    pub appearance: Appearance,
    /// The plugin that brings it; none for the fallback in the code.
    pub plugin: Option<SharedString>,
}

/// The themes of the turned-on plugins, and what is shown now.
#[derive(Default)]
struct Themes {
    /// By plugin, in the plugins' order; a later theme with the same name wins.
    themes: Vec<(SharedString, Arc<Theme>)>,
    /// A theme shown for a moment instead of the chosen one (Quick Switch, the theme page).
    preview: Option<SharedString>,
    /// The appearance of macOS, for a theme that follows it.
    system: Appearance,
}

impl Global for Themes {}

/// Reads the theme files of `plugins` (the turned-on ones) and applies the chosen theme. Returns
/// the problems found (plugin id, what), for the plugins' logs.
pub fn load_plugin_themes(plugins: &[Arc<PluginEntry>], cx: &mut App) -> Vec<(String, String)> {
    let mut themes = Vec::new();
    let mut problems = Vec::new();
    for entry in plugins {
        for spec in &entry.manifest.themes {
            let text = entry
                .files
                .read(&spec.file)
                .ok_or_else(|| format!("{} is missing", spec.file))
                .and_then(|bytes| {
                    String::from_utf8(bytes.into_owned())
                        .map_err(|_| format!("{} is not UTF-8", spec.file))
                });
            match text
                .and_then(|text| parse_theme(&text).map_err(|err| format!("{}: {err}", spec.file)))
            {
                Ok(theme) => {
                    themes.push((SharedString::from(entry.id().to_string()), Arc::new(theme)))
                }
                Err(problem) => problems.push((entry.id().to_string(), problem)),
            }
        }
    }
    cx.default_global::<Themes>().themes = themes;
    #[cfg(feature = "scenario")]
    scenario::listen(cx);
    apply(cx);
    problems
}

/// A scenario switches themes without the Settings page: `action:theme::NextTheme` shows the next
/// theme of [`available`] (and chooses it), as a person would on the Theme page.
#[cfg(feature = "scenario")]
pub mod scenario {
    use gpui::{App, actions};

    actions!(theme, [NextTheme]);

    /// Listens to [`NextTheme`] once per process.
    pub fn listen(cx: &mut App) {
        static LISTENING: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if LISTENING.set(()).is_err() {
            return;
        }
        cx.on_action(|_: &NextTheme, cx| {
            let themes = super::available(cx);
            let current = super::Theme::get(cx).name.clone();
            let index = themes.iter().position(|theme| theme.name == current);
            let next = index.map_or(0, |index| (index + 1) % themes.len());
            let name = themes[next].name.clone();
            super::select(&name, cx);
        });
    }
}

/// The themes that may be chosen: the plugins' ones, and the bundled Flux Night and Flux Day when
/// no plugin brings them.
pub fn available(cx: &App) -> Vec<ThemeInfo> {
    let mut list: Vec<ThemeInfo> = Vec::new();
    if let Some(themes) = cx.try_global::<Themes>() {
        for (plugin, theme) in &themes.themes {
            list.retain(|known| known.name != theme.name);
            list.push(ThemeInfo {
                name: theme.name.clone(),
                appearance: theme.appearance,
                plugin: Some(plugin.clone()),
            });
        }
    }
    for (index, (name, appearance)) in [
        (FLUX_NIGHT, Appearance::Dark),
        (FLUX_DAY, Appearance::Light),
    ]
    .into_iter()
    .enumerate()
    {
        if !list.iter().any(|theme| theme.name.as_ref() == name) {
            list.insert(
                index.min(list.len()),
                ThemeInfo {
                    name: name.into(),
                    appearance,
                    plugin: None,
                },
            );
        }
    }
    list
}

/// A theme by name: a plugin's, or a bundled base (previews of themes not shown).
pub fn by_name(name: &str, cx: &App) -> Option<Theme> {
    let from_plugin = cx.try_global::<Themes>().and_then(|themes| {
        themes
            .themes
            .iter()
            .rev()
            .find(|(_, theme)| theme.name.as_ref() == name)
            .map(|(_, theme)| (**theme).clone())
    });
    from_plugin.or_else(|| match name {
        FLUX_NIGHT => Some(Theme::flux_night()),
        FLUX_DAY => Some(Theme::flux_day()),
        _ => None,
    })
}

/// The theme the settings choose now: the one chosen, or — when it follows macOS — the light or
/// dark one for the system's appearance.
pub fn chosen(cx: &App) -> SharedString {
    let appearance = settings::appearance(cx);
    let system = system_appearance(cx);
    let name = if appearance.sync_with_os {
        match system {
            Appearance::Light => appearance.light_theme.as_deref().unwrap_or(FLUX_DAY),
            Appearance::Dark => appearance.dark_theme.as_deref().unwrap_or(FLUX_NIGHT),
        }
    } else {
        appearance.theme.as_deref().unwrap_or(FLUX_NIGHT)
    };
    SharedString::from(name.to_string())
}

/// Puts the theme to show into the global [`Theme`]: the preview, otherwise the chosen one,
/// otherwise Flux Night. Windows redraw; editors take the new highlighting colors.
pub fn apply(cx: &mut App) {
    let preview = cx
        .try_global::<Themes>()
        .and_then(|themes| themes.preview.clone());
    let following = preview.is_none() && settings::appearance(cx).sync_with_os;
    let name = preview.unwrap_or_else(|| chosen(cx));
    let theme = by_name(&name, cx).unwrap_or_else(Theme::flux_night);
    // The windows' own appearance (the frame's hairline, the traffic lights, the system's file
    // panels) is the theme's; a theme that follows macOS lets macOS decide, so a change of the
    // system's appearance still reaches the windows.
    #[cfg(target_os = "macos")]
    appkit::set_app_appearance((!following).then_some(theme.appearance));
    #[cfg(not(target_os = "macos"))]
    let _ = following;
    // The whole theme: a theme file edited in a plugin under development may change any part.
    let same = cx.try_global::<Theme>().is_some_and(|current| *current == theme);
    if !same {
        cx.set_global(theme);
        cx.refresh_windows();
    }
}

/// Chooses a theme (Settings, Quick Switch): saved in the settings and shown.
pub fn select(name: &str, cx: &mut App) {
    settings::update_appearance(cx, |appearance| appearance.theme = Some(name.to_string()));
    cx.default_global::<Themes>().preview = None;
    apply(cx);
}

/// Shows a theme for a moment without choosing it; `None` — back to the chosen one.
pub fn preview(name: Option<&str>, cx: &mut App) {
    cx.default_global::<Themes>().preview = name.map(|name| SharedString::from(name.to_string()));
    apply(cx);
}

/// The appearance of macOS as a window sees it.
pub fn window_appearance(window: &gpui::Window) -> Appearance {
    match window.appearance() {
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => Appearance::Light,
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => Appearance::Dark,
    }
}

/// macOS switched between light and dark: a theme that follows it changes.
pub fn system_appearance_changed(appearance: Appearance, cx: &mut App) {
    cx.default_global::<Themes>().system = appearance;
    apply(cx);
}

/// The appearance of macOS now. On macOS it is read from the system's setting, not from a window:
/// a window takes the appearance of the theme shown in it.
pub fn system_appearance(cx: &App) -> Appearance {
    #[cfg(feature = "scenario")]
    if let Some(appearance) = scenario_appearance() {
        return appearance;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        if appkit::system_is_dark() {
            Appearance::Dark
        } else {
            Appearance::Light
        }
    }
    #[cfg(not(target_os = "macos"))]
    cx.try_global::<Themes>()
        .map_or(Appearance::Dark, |themes| themes.system)
}

/// `FLUX_SCENARIO_APPEARANCE=light|dark`: a scenario's macOS appearance, so a scenario checks a
/// theme that follows the system without touching the system.
#[cfg(feature = "scenario")]
fn scenario_appearance() -> Option<Appearance> {
    match std::env::var("FLUX_SCENARIO_APPEARANCE").ok()?.as_str() {
        "light" => Some(Appearance::Light),
        "dark" => Some(Appearance::Dark),
        _ => None,
    }
}

/// Settings → Appearance → Sync with OS: the theme follows macOS (the light and the dark one are
/// chosen apart, [`select_for`]).
pub fn set_sync_with_os(on: bool, cx: &mut App) {
    settings::update_appearance(cx, |appearance| appearance.sync_with_os = on);
    cx.default_global::<Themes>().preview = None;
    apply(cx);
}

/// Chooses the theme for macOS's light or dark appearance (when the theme follows macOS).
pub fn select_for(appearance: Appearance, name: &str, cx: &mut App) {
    settings::update_appearance(cx, |settings| match appearance {
        Appearance::Light => settings.light_theme = Some(name.to_string()),
        Appearance::Dark => settings.dark_theme = Some(name.to_string()),
    });
    cx.default_global::<Themes>().preview = None;
    apply(cx);
}

/// The windows of Flux in AppKit: their appearance and the system's.
#[cfg(target_os = "macos")]
// `msg_send!` of objc 0.2 checks a `cargo-clippy` feature this crate doesn't declare.
#[allow(unexpected_cfgs)]
mod appkit {
    use std::ffi::{CStr, CString};
    use std::os::raw::c_char;
    use std::sync::Mutex;

    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    use super::Appearance;

    /// The appearance last given to the app: setting the same again would wake every window's
    /// appearance observer for nothing.
    static SET: Mutex<Option<Option<Appearance>>> = Mutex::new(None);

    fn ns_string(text: &str) -> *mut Object {
        let text = CString::new(text).expect("no NUL");
        // SAFETY: a class method of NSString with a NUL-terminated UTF-8 string; the result is
        // autoreleased.
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }

    /// The appearance of the app's windows and panels: Aqua, Dark Aqua, or — `None` — the
    /// system's. On the main thread.
    pub fn set_app_appearance(appearance: Option<Appearance>) {
        let mut set = SET.lock().unwrap();
        if *set == Some(appearance) {
            return;
        }
        *set = Some(appearance);
        // SAFETY: AppKit's shared application and named appearances; `setAppearance:` takes nil
        // for the system's appearance.
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let named: *mut Object = match appearance {
                None => std::ptr::null_mut(),
                Some(appearance) => {
                    let name = match appearance {
                        Appearance::Light => "NSAppearanceNameAqua",
                        Appearance::Dark => "NSAppearanceNameDarkAqua",
                    };
                    msg_send![class!(NSAppearance), appearanceNamed: ns_string(name)]
                }
            };
            let _: () = msg_send![app, setAppearance: named];
        }
    }

    /// The system is in dark mode: `AppleInterfaceStyle` is "Dark" (also while it switches by
    /// itself), absent in light mode.
    pub fn system_is_dark() -> bool {
        // SAFETY: NSUserDefaults is thread-safe; `UTF8String` of an NSString lives as long as it.
        unsafe {
            let defaults: *mut Object = msg_send![class!(NSUserDefaults), standardUserDefaults];
            let style: *mut Object =
                msg_send![defaults, stringForKey: ns_string("AppleInterfaceStyle")];
            if style.is_null() {
                return false;
            }
            let utf8: *const c_char = msg_send![style, UTF8String];
            !utf8.is_null() && CStr::from_ptr(utf8).to_bytes() == b"Dark"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_syntax::{HighlightMap, languages};



    /// Every capture of every language resolves to a scope of both bundled themes: its own or a
    /// more general one, by falling back along the dots. `none` (an internal one in markdown) is
    /// left uncolored.
    #[test]
    fn bundled_themes_cover_every_capture() {
        flux_syntax::standard::register();
        for theme in [Theme::flux_night(), Theme::flux_day()] {
            let scopes = theme.syntax_scopes();
            for language in languages()
                .into_iter()
                .filter(|language| language.owner() == flux_syntax::standard::OWNER)
            {
                let map = HighlightMap::new(&language, &scopes);
                for (i, name) in language.capture_names().iter().enumerate() {
                    let highlight = map.get(i as u32);
                    if *name == "none" {
                        assert_eq!(highlight, None, "{}: {}", theme.name, language.name());
                    } else {
                        let found = highlight.is_some();
                        assert!(found, "{}: {}: @{name}", theme.name, language.name());
                    }
                }
            }
        }
    }

    #[test]
    fn scopes_are_unique_and_styles_line_up() {
        let theme = Theme::flux_night();
        let scopes = theme.syntax_scopes();
        let mut sorted = scopes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), scopes.len());
        let title = scopes.iter().position(|s| *s == "text.title").unwrap();
        assert!(theme.syntax_style(Highlight(title)).unwrap().bold);
        assert_eq!(theme.syntax_style(Highlight(scopes.len())), None);
    }

    /// The bundled themes name every token (they are the bases of the others).
    #[test]
    fn bundled_themes_are_complete() {
        let night = Theme::flux_night();
        assert_eq!(night.name.as_ref(), FLUX_NIGHT);
        assert_eq!(night.appearance, Appearance::Dark);
        // Spot checks of what Flux Night was in the code (the whole of it matched when it moved
        // into the file).
        assert_eq!(night.ui.island, Hsla::from(rgba(0x0d1018e6)));
        assert_eq!(night.ui.accent, Hsla::from(rgb(0x8590ff)));
        assert_eq!(night.ui.graph_lanes[2], Hsla::from(rgb(0xb48cff)));
        assert_eq!(night.terminal.ansi[15], Hsla::from(rgb(0xf0f6fc)));
        assert_eq!(night.style_for("keyword").unwrap().color, Hsla::from(rgb(0xff7b72)));
        assert!(night.style_for("text.title").unwrap().bold);
        let day = Theme::flux_day();
        assert_eq!(day.name.as_ref(), FLUX_DAY);
        assert_eq!(day.appearance, Appearance::Light);
        assert_ne!(day.ui.island, night.ui.island);
        // Highlighting maps captures to scopes by position: every theme lists the scopes in the
        // same order, so code highlighted under one theme (hover, the chat) reads under another.
        assert_eq!(day.syntax_scopes(), night.syntax_scopes());
    }

    #[test]
    fn a_theme_names_what_it_changes() {
        let theme = parse_theme(
            "name = \"Dusk\"\nappearance = \"dark\"\n[ui]\naccent = \"#ff0000\"\n\
             [syntax]\nkeyword = { color = \"#00ff00\", italic = true }\n\"markup.heading\" = \
             \"#0000ff\"\n[terminal]\ncursor = \"#00ff0080\"\n",
        )
        .unwrap();
        let night = Theme::flux_night();
        assert_eq!(theme.name.as_ref(), "Dusk");
        assert_eq!(theme.ui.accent, Hsla::from(rgb(0xff0000)));
        assert_eq!(theme.ui.island, night.ui.island);
        assert_eq!(theme.terminal.cursor, Hsla::from(rgba(0x00ff0080)));
        assert_eq!(theme.terminal.ansi, night.terminal.ansi);
        let keyword = theme.style_for("keyword").unwrap();
        assert!(keyword.italic && !keyword.bold);
        // A scope of its own goes after the base's, which keep their places.
        let scopes = theme.syntax_scopes();
        assert_eq!(scopes[..night.syntax.len()], night.syntax_scopes()[..]);
        assert_eq!(scopes.last(), Some(&"markup.heading"));
        let light = parse_theme("name = \"Dawn\"\nappearance = \"light\"\n").unwrap();
        assert_eq!(light.ui, Theme::flux_day().ui);
    }

    #[test]
    fn mistakes_in_a_theme_file_are_named() {
        let error = |text: &str| parse_theme(text).unwrap_err();
        let head = "name = \"X\"\nappearance = \"dark\"\n";
        assert!(error("appearance = \"dark\"\n").contains("name"));
        assert!(error("name = \"X\"\nappearance = \"sepia\"\n").contains("appearance"));
        assert!(error(&format!("{head}colour = 1\n")).contains("`colour`"));
        assert!(error(&format!("{head}[ui]\nacent = \"#ffffff\"\n")).contains("`acent`"));
        assert!(error(&format!("{head}[ui]\naccent = \"red\"\n")).contains("ui.accent"));
        assert!(error(&format!("{head}[ui]\naccent = \"#fff\"\n")).contains("ui.accent"));
        assert!(error(&format!("{head}[terminal]\nansi = [\"#000000\"]\n")).contains("16"));
        assert!(error(&format!("{head}[terminal]\nbold = \"#000000\"\n")).contains("`bold`"));
        assert!(error(&format!("{head}[syntax]\nKeyword = \"#000000\"\n")).contains("Keyword"));
        let size = format!("{head}[syntax]\nkeyword = {{ color = \"#000000\", size = 2 }}\n");
        assert!(error(&size).contains("`size`"));
        assert!(error(&format!("{head}[ui]\ngraph_lanes = [\"#000000\"]\n")).contains("8"));
        // A base names everything.
        assert!(parse_base(head).unwrap_err().contains("missing"));
    }

    #[test]
    fn styles_fall_back_along_the_dots() {
        let theme = Theme::flux_day();
        assert_eq!(theme.style_for("function.method"), theme.style_for("function"));
        assert!(theme.style_for("no.such.scope").is_none());
        assert!(UI_TOKENS.contains(&"on_accent"));
    }
}
