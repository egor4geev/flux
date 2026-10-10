//! Sets of file icons (stage 8.3, ADR-032), as icon themes in Zed and icon packs in JetBrains IDEs:
//! a plugin's `[[icon-themes]]` file maps file names, name prefixes and extensions to its SVGs and
//! colors (`docs/icon-themes.md`); Settings → Appearance → File Icons chooses one. Flux's own set is
//! the bundled plugin `flux.icons`. A language plugin may give its files an icon of its own (`icon`
//! of `[[languages]]`): used when the chosen set has none for the file.
//!
//! A file's icon, in order: the set's exact file name, its name prefix, its extension (compound ones
//! like `d.ts` first), the language plugins' icon, the set's default; none of them — Flux's plain
//! icon ([`crate::icons::file_icon`]). A color is a shade of the theme's palette, so a monochrome
//! set follows the light and dark themes; an entry without a color is a full-color SVG.
//!
//! The chosen set is kept for the whole process, not per window: [`crate::icons::file_icon`] asks
//! [`resolve_file`] without a context, on every row of every list, so everything is looked up in
//! maps made when the set is read.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use flux_plugin::registry::PluginEntry;
use gpui::{App, Hsla, SharedString, rgba};

use crate::icons::{self, FileIcon, IconSource};
use crate::settings;
use crate::theme::UiColors;

/// The name of Flux's own set (the bundled plugin `flux.icons`): the default.
pub const FLUX_ICONS: &str = "Flux Icons";

/// Longer names are lowercased into a new string; shorter ones on the stack.
const NAME_BUFFER: usize = 256;

/// A set of file icons, read from a plugin's file.
#[derive(Debug, Clone)]
pub struct IconTheme {
    pub name: SharedString,
    /// The plugin that brings it: its SVGs are its files.
    pub plugin: SharedString,
    defaults: Defaults,
    rules: Rules,
}

/// A set that may be chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct IconThemeInfo {
    pub name: SharedString,
    pub plugin: SharedString,
}

/// The icon a language plugin gives to its files.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguageIcon {
    pub plugin: SharedString,
    pub extensions: Vec<String>,
    pub file_names: Vec<String>,
    /// The SVG in the plugin's folder.
    pub icon: String,
    /// A shade of the theme's palette ("orange", "text-muted") or "#rrggbb"; none — the default
    /// color of files.
    pub color: Option<String>,
}

/// What a set gives files and folders nothing else matches.
#[derive(Debug, Clone, Default)]
struct Defaults {
    file: Option<Entry>,
    folder: Option<Entry>,
    folder_open: Option<Entry>,
}

/// File names, name prefixes and extensions → icons; all keys in lowercase.
#[derive(Debug, Clone, Default)]
struct Rules {
    file_names: HashMap<String, Entry>,
    /// Longest first: the first that matches wins.
    prefixes: Vec<(String, Entry)>,
    extensions: HashMap<String, Entry>,
    /// The most dots in an extension (`tar.gz` has one): how far back a name is looked at.
    extension_dots: usize,
}

/// One icon of a set.
#[derive(Debug, Clone)]
struct Entry {
    source: IconSource,
    /// The color of a monochrome icon; none — a full-color SVG.
    color: Option<ColorRef>,
    /// The color tints take for a full-color icon (a tab's badge); `text-muted` by default.
    tint: Option<ColorRef>,
}

/// A color: a shade of the theme's palette, or a fixed one.
#[derive(Debug, Clone, Copy)]
enum ColorRef {
    Token(fn(&UiColors) -> Hsla),
    Fixed(Hsla),
}

impl ColorRef {
    fn resolve(self, ui: &UiColors) -> Hsla {
        match self {
            ColorRef::Token(token) => token(ui),
            ColorRef::Fixed(color) => color,
        }
    }
}

impl Entry {
    fn icon(&self, ui: &UiColors) -> FileIcon {
        match self.color {
            Some(color) => FileIcon {
                source: self.source.clone(),
                color: color.resolve(ui),
                multicolor: false,
            },
            None => FileIcon {
                source: self.source.clone(),
                color: self.tint.map_or(ui.text_muted, |tint| tint.resolve(ui)),
                multicolor: true,
            },
        }
    }
}

impl Rules {
    /// The entry for a lowercased file name: its exact name, a prefix, then its extensions,
    /// longest first.
    fn lookup(&self, lower: &str) -> Option<&Entry> {
        if let Some(entry) = self.file_names.get(lower) {
            return Some(entry);
        }
        if let Some((_, entry)) = self
            .prefixes
            .iter()
            .find(|(prefix, _)| lower.starts_with(prefix.as_str()))
        {
            return Some(entry);
        }
        self.extension(lower)
    }

    /// The longest extension of the name the rules have: `types.d.ts` → `d.ts`, then `ts`.
    fn extension(&self, lower: &str) -> Option<&Entry> {
        if self.extensions.is_empty() {
            return None;
        }
        // The dots of the name from the end: the extension after the last one, then after the one
        // before it, up to as many dots as the rules' extensions have.
        let mut dots = lower.rmatch_indices('.').map(|(index, _)| index);
        let mut candidates: [Option<usize>; 4] = [None; 4];
        for slot in candidates
            .iter_mut()
            .take((self.extension_dots + 1).min(4))
        {
            *slot = dots.next();
        }
        for index in candidates.iter().rev().flatten() {
            let extension = &lower[index + 1..];
            if extension.is_empty() {
                continue;
            }
            if let Some(entry) = self.extensions.get(extension) {
                return Some(entry);
            }
        }
        None
    }

    fn is_empty(&self) -> bool {
        self.file_names.is_empty() && self.prefixes.is_empty() && self.extensions.is_empty()
    }
}

#[derive(Default)]
struct State {
    themes: Vec<Arc<IconTheme>>,
    /// The language plugins' icons, as rules.
    languages: Rules,
    /// The set in use; none — Flux's own icons.
    active: Option<Arc<IconTheme>>,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(RwLock::default);

/// Reads the icon sets of `plugins` (the turned-on ones) and the icons of their languages, and
/// puts the chosen set in use. Returns the problems found (plugin id, what).
pub fn load_plugin_icons(plugins: &[Arc<PluginEntry>], cx: &mut App) -> Vec<(String, String)> {
    let mut themes = Vec::new();
    let mut languages = Rules::default();
    let mut problems = Vec::new();
    for entry in plugins {
        let manifest = &entry.manifest;
        let has_icons = !manifest.icon_themes.is_empty()
            || manifest.languages.iter().any(|language| language.icon.is_some());
        if !has_icons {
            continue;
        }
        let plugin = SharedString::from(entry.id().to_string());
        // The SVGs are drawn as the plugin's assets; the store registers them too, but only once
        // the window has found the plugins — the first frame must already have them.
        icons::register_plugin_files(entry.id(), entry.files.clone());
        for spec in &manifest.icon_themes {
            let text = entry
                .files
                .read(&spec.file)
                .and_then(|bytes| String::from_utf8(bytes.into_owned()).ok());
            let Some(text) = text else {
                problems.push((plugin.to_string(), format!("{} is missing", spec.file)));
                continue;
            };
            match parse_icon_theme(&plugin, &text) {
                Ok(mut theme) => {
                    let missing = theme.drop_missing(|path| entry.files.read(path).is_some());
                    if !missing.is_empty() {
                        problems.push((
                            plugin.to_string(),
                            format!("{}: missing icons: {}", spec.file, missing.join(", ")),
                        ));
                    }
                    themes.push(Arc::new(theme));
                }
                Err(err) => problems.push((plugin.to_string(), format!("{}: {err}", spec.file))),
            }
        }
        for language in &manifest.languages {
            let Some(icon) = &language.icon else {
                continue;
            };
            if entry.files.read(icon).is_none() {
                problems.push((
                    plugin.to_string(),
                    format!("language {}: {icon} is missing", language.id),
                ));
                continue;
            }
            let color = match language.icon_color.as_deref() {
                None => ColorRef::Token(|ui| ui.text_muted),
                Some(color) => match parse_color(color) {
                    Ok(color) => color,
                    Err(err) => {
                        problems.push((
                            plugin.to_string(),
                            format!("language {}: icon-color: {err}", language.id),
                        ));
                        ColorRef::Token(|ui| ui.text_muted)
                    }
                },
            };
            let entry = Entry {
                source: IconSource::Asset(icons::plugin_asset(&plugin, icon)),
                color: Some(color),
                tint: None,
            };
            add_language_icon(
                &mut languages,
                &LanguageIcon {
                    plugin: plugin.clone(),
                    extensions: language.extensions.clone(),
                    file_names: language.file_names.clone(),
                    icon: icon.clone(),
                    color: language.icon_color.clone(),
                },
                entry,
            );
        }
    }
    {
        let mut state = STATE.write().unwrap();
        state.themes = themes;
        state.languages = languages;
    }
    apply(cx);
    problems
}

/// A language plugin's icon for its files: later plugins win, as their languages do.
fn add_language_icon(rules: &mut Rules, language: &LanguageIcon, entry: Entry) {
    for name in &language.file_names {
        rules
            .file_names
            .insert(name.to_ascii_lowercase(), entry.clone());
    }
    for extension in &language.extensions {
        let extension = extension.to_ascii_lowercase();
        rules.extension_dots = rules.extension_dots.max(extension.matches('.').count());
        rules.extensions.insert(extension, entry.clone());
    }
}

impl IconTheme {
    /// Drops the entries whose SVG isn't among the plugin's files (they fall back to what comes
    /// next); returns their paths.
    fn drop_missing(&mut self, exists: impl Fn(&str) -> bool) -> Vec<String> {
        let prefix = format!("plugins/{}/", self.plugin);
        let relative = |entry: &Entry| match &entry.source {
            IconSource::Asset(path) => path.strip_prefix(&prefix).map(str::to_string),
            IconSource::Builtin(_) => None,
        };
        let mut missing = Vec::new();
        let mut keep = |entry: &Entry| match relative(entry) {
            Some(path) if !exists(&path) => {
                if !missing.contains(&path) {
                    missing.push(path);
                }
                false
            }
            _ => true,
        };
        let defaults = &mut self.defaults;
        for slot in [
            &mut defaults.file,
            &mut defaults.folder,
            &mut defaults.folder_open,
        ] {
            if slot.as_ref().is_some_and(|entry| !keep(entry)) {
                *slot = None;
            }
        }
        let rules = &mut self.rules;
        rules.file_names.retain(|_, entry| keep(entry));
        rules.prefixes.retain(|(_, entry)| keep(entry));
        rules.extensions.retain(|_, entry| keep(entry));
        missing.sort();
        missing
    }
}

/// Reads an icon set's file (`docs/icon-themes.md`) of the plugin `plugin`: its SVGs become that
/// plugin's assets.
pub fn parse_icon_theme(plugin: &str, text: &str) -> Result<IconTheme, String> {
    let table: toml::Table = text.parse().map_err(|err: toml::de::Error| {
        err.message().to_string()
    })?;
    let mut name = None;
    let mut defaults = Defaults::default();
    let mut rules = Rules::default();
    for (key, value) in &table {
        match key.as_str() {
            "name" => match value.as_str() {
                Some(text) if !text.trim().is_empty() => name = Some(text.trim().to_string()),
                _ => return Err("name: a non-empty string".into()),
            },
            "defaults" => {
                let section = value
                    .as_table()
                    .ok_or("defaults: a table of file, folder, folder-open")?;
                for (kind, value) in section {
                    let entry = parse_entry(plugin, &format!("defaults.{kind}"), value)?;
                    match kind.as_str() {
                        "file" => defaults.file = Some(entry),
                        "folder" => defaults.folder = Some(entry),
                        "folder-open" => defaults.folder_open = Some(entry),
                        other => {
                            return Err(format!(
                                "defaults.{other}: unknown (file, folder, folder-open)"
                            ));
                        }
                    }
                }
            }
            "file-names" | "name-prefixes" | "extensions" => {
                let section = value
                    .as_table()
                    .ok_or_else(|| format!("{key}: a table of names and icons"))?;
                for (name, value) in section {
                    let path = format!("{key}.\"{name}\"");
                    let lower = name.to_ascii_lowercase();
                    if lower.is_empty() {
                        return Err(format!("{path}: an empty name"));
                    }
                    let entry = parse_entry(plugin, &path, value)?;
                    match key.as_str() {
                        "file-names" => {
                            rules.file_names.insert(lower, entry);
                        }
                        "name-prefixes" => rules.prefixes.push((lower, entry)),
                        _ => {
                            if lower.starts_with('.') {
                                return Err(format!(
                                    "{path}: extensions go without the leading dot"
                                ));
                            }
                            rules.extension_dots = rules.extension_dots.max(lower.matches('.').count());
                            rules.extensions.insert(lower, entry);
                        }
                    }
                }
            }
            other => {
                return Err(format!(
                    "{other}: unknown key (name, defaults, file-names, name-prefixes, extensions)"
                ));
            }
        }
    }
    rules
        .prefixes
        .sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let name = name.ok_or("name is missing")?;
    if rules.is_empty()
        && defaults.file.is_none()
        && defaults.folder.is_none()
        && defaults.folder_open.is_none()
    {
        return Err("no icons".into());
    }
    Ok(IconTheme {
        name: name.into(),
        plugin: plugin.to_string().into(),
        defaults,
        rules,
    })
}

/// `{ icon = "icons/rust.svg", color = "orange" }`; without a color — a full-color SVG, whose
/// optional `tint` is the color of what goes with the file.
fn parse_entry(plugin: &str, path: &str, value: &toml::Value) -> Result<Entry, String> {
    let table = value
        .as_table()
        .ok_or_else(|| format!("{path}: {{ icon = \"icons/….svg\", color = \"…\" }}"))?;
    let mut icon = None;
    let mut color = None;
    let mut tint = None;
    for (key, value) in table {
        let text = || {
            value
                .as_str()
                .ok_or_else(|| format!("{path}.{key}: a string"))
        };
        match key.as_str() {
            "icon" => icon = Some(text()?.to_string()),
            "color" => {
                color = Some(parse_color(text()?).map_err(|err| format!("{path}.color: {err}"))?)
            }
            "tint" => {
                tint = Some(parse_color(text()?).map_err(|err| format!("{path}.tint: {err}"))?)
            }
            other => return Err(format!("{path}.{other}: unknown (icon, color, tint)")),
        }
    }
    let icon = icon.ok_or_else(|| format!("{path}: no icon"))?;
    let fine = icon.ends_with(".svg")
        && !icon.starts_with('/')
        && !icon.contains('\\')
        && icon.split('/').all(|part| !part.is_empty() && part != "..");
    if !fine {
        return Err(format!(
            "{path}.icon: \"{icon}\" — an SVG in the plugin's folder, like \"icons/rust.svg\""
        ));
    }
    if color.is_some() && tint.is_some() {
        return Err(format!(
            "{path}: `tint` is for full-color icons (without `color`)"
        ));
    }
    Ok(Entry {
        source: IconSource::Asset(icons::plugin_asset(plugin, &icon)),
        color,
        tint,
    })
}

/// A shade of the theme's palette by name (`orange`, `text-muted`), or `#rrggbb` / `#rrggbbaa`.
fn parse_color(text: &str) -> Result<ColorRef, String> {
    if let Some(hex) = text.strip_prefix('#') {
        let value = u32::from_str_radix(hex, 16).ok();
        return match (hex.len(), value) {
            (6, Some(value)) => Ok(ColorRef::Fixed(rgba((value << 8) | 0xff).into())),
            (8, Some(value)) => Ok(ColorRef::Fixed(rgba(value).into())),
            _ => Err(format!("\"{text}\" — #rrggbb or #rrggbbaa")),
        };
    }
    let token: fn(&UiColors) -> Hsla = match text.replace('_', "-").as_str() {
        "foreground" => |ui| ui.foreground,
        "text-muted" => |ui| ui.text_muted,
        "dim" => |ui| ui.dim,
        "text-disabled" => |ui| ui.text_disabled,
        "accent" => |ui| ui.accent,
        "accent-text" => |ui| ui.accent_text,
        "success" => |ui| ui.success,
        "warning" => |ui| ui.warning,
        "error" => |ui| ui.error,
        "info" => |ui| ui.info,
        "modified" => |ui| ui.modified,
        "blue" => |ui| ui.blue,
        "indigo" => |ui| ui.indigo,
        "violet" => |ui| ui.violet,
        "pink" => |ui| ui.pink,
        "red" => |ui| ui.red,
        "orange" => |ui| ui.orange,
        "amber" => |ui| ui.amber,
        "lime" => |ui| ui.lime,
        "green" => |ui| ui.green,
        "teal" => |ui| ui.teal,
        "cyan" => |ui| ui.cyan,
        "folder" => |ui| ui.folder,
        _ => {
            return Err(format!(
                "\"{text}\" — a palette shade (blue, indigo, violet, pink, red, orange, amber, \
                 lime, green, teal, cyan, folder, text-muted, dim, foreground, accent…) or #rrggbb"
            ));
        }
    };
    Ok(ColorRef::Token(token))
}

/// The sets that may be chosen.
pub fn available() -> Vec<IconThemeInfo> {
    let state = STATE.read().unwrap();
    let mut list: Vec<IconThemeInfo> = Vec::new();
    for theme in &state.themes {
        // A later plugin's set with the same name takes its place (a plugin under development).
        list.retain(|known| known.name != theme.name);
        list.push(IconThemeInfo {
            name: theme.name.clone(),
            plugin: theme.plugin.clone(),
        });
    }
    list
}

/// The set in use; none — Flux's own icons.
pub fn active() -> Option<SharedString> {
    STATE
        .read()
        .unwrap()
        .active
        .as_ref()
        .map(|theme| theme.name.clone())
}

/// Puts the chosen set (Settings → Appearance → File Icons) in use: the one the settings name,
/// Flux Icons when that one is gone; windows redraw.
pub fn apply(cx: &mut App) {
    let chosen = settings::appearance(cx)
        .icon_theme
        .unwrap_or_else(|| FLUX_ICONS.to_string());
    let changed = {
        let mut state = STATE.write().unwrap();
        let find = |name: &str| {
            state
                .themes
                .iter()
                .rev()
                .find(|theme| theme.name.as_ref() == name)
                .cloned()
        };
        let active = find(&chosen).or_else(|| find(FLUX_ICONS));
        let changed = match (&state.active, &active) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        state.active = active;
        changed
    };
    if changed {
        cx.refresh_windows();
    }
}

/// Chooses a set (Settings, Quick Switch): saved in the settings and put in use.
pub fn select(name: &str, cx: &mut App) {
    settings::update_appearance(cx, |appearance| appearance.icon_theme = Some(name.to_string()));
    apply(cx);
}

/// A file's icon from the set in use, then from its language's plugin, then the set's default;
/// none — Flux's own.
pub fn resolve_file(file_name: &str, ui: &UiColors) -> Option<FileIcon> {
    let state = STATE.read().unwrap();
    let mut buffer = [0u8; NAME_BUFFER];
    let owned;
    let lower = match lowercase_into(file_name, &mut buffer) {
        Some(lower) => lower,
        None => {
            owned = file_name.to_ascii_lowercase();
            owned.as_str()
        }
    };
    resolve_with(state.active.as_deref(), &state.languages, lower, ui)
}

fn resolve_with(
    theme: Option<&IconTheme>,
    languages: &Rules,
    lower: &str,
    ui: &UiColors,
) -> Option<FileIcon> {
    if let Some(entry) = theme.and_then(|theme| theme.rules.lookup(lower)) {
        return Some(entry.icon(ui));
    }
    if let Some(entry) = languages.lookup(lower) {
        return Some(entry.icon(ui));
    }
    theme
        .and_then(|theme| theme.defaults.file.as_ref())
        .map(|entry| entry.icon(ui))
}

/// A folder's icon from the set in use; none — Flux's own.
pub fn resolve_folder(expanded: bool, ui: &UiColors) -> Option<FileIcon> {
    let state = STATE.read().unwrap();
    folder_with(state.active.as_deref(), expanded, ui)
}

fn folder_with(theme: Option<&IconTheme>, expanded: bool, ui: &UiColors) -> Option<FileIcon> {
    let defaults = &theme?.defaults;
    let entry = if expanded {
        defaults.folder_open.as_ref().or(defaults.folder.as_ref())
    } else {
        defaults.folder.as_ref()
    };
    entry.map(|entry| entry.icon(ui))
}

/// The files a set's preview shows (Settings → Appearance → File Icons, Quick Switch).
pub const SAMPLE_FILES: [&str; 6] = [
    "main.rs",
    "App.tsx",
    "package.json",
    "README.md",
    "Dockerfile",
    ".gitignore",
];

/// How the set `name` draws a few files and a folder ([`SAMPLE_FILES`], then a closed folder),
/// without putting it in use; `None` (or a set that isn't there) — Flux's plain icons.
pub fn sample_icons(name: Option<&str>, ui: &UiColors) -> Vec<FileIcon> {
    let state = STATE.read().unwrap();
    let theme = name.and_then(|name| {
        state
            .themes
            .iter()
            .rev()
            .find(|theme| theme.name.as_ref() == name)
            .cloned()
    });
    let theme = theme.as_deref();
    let mut samples: Vec<FileIcon> = SAMPLE_FILES
        .iter()
        .map(|file| {
            resolve_with(theme, &state.languages, &file.to_ascii_lowercase(), ui)
                .unwrap_or_else(|| icons::builtin_file_icon(file, ui))
        })
        .collect();
    samples.push(
        folder_with(theme, false, ui).unwrap_or_else(|| icons::builtin_folder_icon(false, ui)),
    );
    samples
}

/// `name` in ASCII lowercase in `buffer`, without allocating; `None` if it doesn't fit.
fn lowercase_into<'a>(name: &str, buffer: &'a mut [u8; NAME_BUFFER]) -> Option<&'a str> {
    let bytes = name.as_bytes();
    let target = buffer.get_mut(..bytes.len())?;
    target.copy_from_slice(bytes);
    target.make_ascii_lowercase();
    // ASCII lowercasing keeps UTF-8 valid: multibyte sequences have no ASCII bytes.
    std::str::from_utf8(target).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Instant;

    use flux_plugin::registry::load_bundled;

    use super::*;
    use crate::icons::IconName;
    use crate::theme::Theme;

    /// Flux's own set, as the bundled plugin ships it.
    pub(crate) fn flux_set() -> IconTheme {
        parse_icon_theme(
            "flux.icons",
            include_str!("../../../plugins/icons/icon-themes/flux.toml"),
        )
        .unwrap()
    }

    fn icon_of(theme: &IconTheme, name: &str, ui: &UiColors) -> Option<FileIcon> {
        resolve_with(Some(theme), &Rules::default(), &name.to_ascii_lowercase(), ui)
    }

    /// The icon file a set gives a name, as `(file stem, color)`.
    fn stem_and_color(icon: &FileIcon) -> (String, Hsla) {
        let path = icon.source.path();
        let stem = path
            .rsplit('/')
            .next()
            .unwrap()
            .trim_end_matches(".svg")
            .to_string();
        (stem, icon.color)
    }

    /// Flux's icons before stage 8.3, for the parity test: the glyph's name and its shade.
    fn before_8_3(file_name: &str, ui: &UiColors) -> (&'static str, Hsla) {
        let lower = file_name.to_ascii_lowercase();
        match lower.as_str() {
            "cargo.toml" => ("package", ui.orange),
            "package.json" => ("package", ui.red),
            "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb"
            | "poetry.lock" | "gemfile.lock" | "composer.lock" | "flake.lock" => ("lock", ui.dim),
            ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => ("git", ui.red),
            "dockerfile"
            | ".dockerignore"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | "compose.yml"
            | "compose.yaml" => ("docker", ui.blue),
            "readme" | "readme.md" | "readme.markdown" | "readme.txt" => ("readme", ui.teal),
            "makefile" | "gnumakefile" | "justfile" => ("shell", ui.green),
            "go.mod" | "go.sum" => ("go", ui.cyan),
            "rust-toolchain" | "rust-toolchain.toml" => ("rust", ui.orange),
            "tsconfig.json" => ("typescript", ui.blue),
            "jsconfig.json" => ("javascript", ui.amber),
            _ if lower.starts_with("license")
                || lower.starts_with("licence")
                || lower == "copying" =>
            {
                ("license", ui.amber)
            }
            _ if lower == ".env" || lower.starts_with(".env.") => ("config", ui.lime),
            _ => match lower.rsplit_once('.').map_or("", |(_, ext)| ext) {
                "rs" => ("rust", ui.orange),
                "ts" | "mts" | "cts" => ("typescript", ui.blue),
                "js" | "mjs" | "cjs" => ("javascript", ui.amber),
                "tsx" | "jsx" => ("react", ui.cyan),
                "json" | "jsonc" | "json5" => ("json", ui.amber),
                "md" | "markdown" | "mdx" => ("markdown", ui.indigo),
                "toml" => ("toml", ui.text_muted),
                "yaml" | "yml" => ("yaml", ui.pink),
                "py" | "pyi" | "pyw" => ("python", ui.blue),
                "go" => ("go", ui.cyan),
                "sh" | "bash" | "zsh" | "fish" | "ksh" => ("shell", ui.green),
                "swift" => ("swift", ui.orange),
                "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" => ("c", ui.indigo),
                "html" | "htm" | "xhtml" => ("html", ui.orange),
                "css" | "scss" | "sass" | "less" => ("css", ui.violet),
                "lock" => ("lock", ui.dim),
                "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "tiff"
                | "avif" | "icns" => ("image", ui.violet),
                "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "dmg" => {
                    ("archive", ui.amber)
                }
                "txt" | "log" | "text" | "rst" => ("text", ui.text_muted),
                "env" | "ini" | "cfg" | "conf" | "editorconfig" | "properties" | "plist" => {
                    ("config", ui.lime)
                }
                "rb" => ("code", ui.red),
                "java" | "xml" | "svelte" => ("code", ui.orange),
                "kt" | "kts" | "wasm" | "wat" => ("code", ui.violet),
                "php" => ("code", ui.indigo),
                "lua" | "dart" => ("code", ui.blue),
                "sql" => ("code", ui.amber),
                "vue" | "zig" => ("code", ui.green),
                _ => ("file", ui.text_muted),
            },
        }
    }

    /// Names covering every rule of the table before 8.3, and some it doesn't know.
    const NAMES: &[&str] = &[
        "Cargo.toml", "package.json", "Cargo.lock", "package-lock.json", "yarn.lock",
        "pnpm-lock.yaml", "bun.lockb", "poetry.lock", "Gemfile.lock", "composer.lock",
        "flake.lock", ".gitignore", ".gitattributes", ".gitmodules", ".gitkeep", "Dockerfile",
        ".dockerignore", "docker-compose.yml", "docker-compose.yaml", "compose.yml",
        "compose.yaml", "README", "README.md", "readme.markdown", "README.txt", "Makefile",
        "GNUmakefile", "justfile", "go.mod", "go.sum", "rust-toolchain", "rust-toolchain.toml",
        "tsconfig.json", "jsconfig.json", "LICENSE", "LICENSE-MIT", "licence.txt", "COPYING",
        ".env", ".env.local", "main.rs", "a.ts", "a.mts", "a.cts", "a.js", "a.mjs", "a.cjs",
        "App.tsx", "App.jsx", "a.json", "a.jsonc", "a.json5", "a.md", "a.markdown", "a.mdx",
        "config.toml", "a.yaml", "a.yml", "a.py", "a.pyi", "a.pyw", "a.go", "a.sh", "a.bash",
        "a.zsh", "a.fish", "a.ksh", "a.swift", "a.c", "a.h", "a.cc", "a.cpp", "a.cxx", "a.hpp",
        "a.hh", "a.m", "a.mm", "a.html", "a.htm", "a.xhtml", "a.css", "a.scss", "a.sass",
        "a.less", "x.lock", "a.png", "a.jpg", "a.jpeg", "a.gif", "a.svg", "a.webp", "a.ico",
        "a.bmp", "a.tiff", "a.avif", "a.icns", "a.zip", "a.tar", "archive.tar.gz", "a.tgz",
        "a.bz2", "a.xz", "a.7z", "a.rar", "a.zst", "a.dmg", "a.txt", "a.log", "a.text",
        "a.rst", "x.env", "a.ini", "a.cfg", "a.conf", "a.editorconfig", "a.properties",
        "a.plist", "a.rb", "a.java", "a.xml", "a.svelte", "a.kt", "a.kts", "a.wasm", "a.wat",
        "a.php", "a.lua", "a.dart", "a.sql", "a.vue", "a.zig", "types.d.ts", "LIB.RS",
        "Без расширения", "trailing.", "no_extension", ".bashrc", "Dockerfile.dev",
        "notes.md.txt", "a.unknown",
    ];

    #[test]
    fn the_flux_set_draws_what_flux_drew_before() {
        let ui = Theme::flux_night().ui;
        let set = flux_set();
        for name in NAMES {
            let (stem, color) = before_8_3(name, &ui);
            let found = icon_of(&set, name, &ui)
                .map(|icon| stem_and_color(&icon))
                .unwrap_or_else(|| ("file".to_string(), ui.text_muted));
            assert_eq!(found, (stem.to_string(), color), "{name}");
        }
    }

    #[test]
    fn the_bundled_set_has_every_icon_it_names() {
        let bundled = crate::bundled::plugins()
            .iter()
            .map(|plugin| load_bundled(plugin).unwrap())
            .find(|entry| entry.id() == "flux.icons")
            .expect("Flux Icons is bundled");
        let file = &bundled.manifest.icon_themes[0].file;
        let text = bundled.files.read(file).unwrap();
        let mut set = parse_icon_theme("flux.icons", std::str::from_utf8(&text).unwrap()).unwrap();
        assert_eq!(set.name.as_ref(), FLUX_ICONS);
        let missing = set.drop_missing(|path| bundled.files.read(path).is_some());
        assert!(missing.is_empty(), "{missing:?}");
        for (path, svg) in bundled
            .files
            .list("icons/")
            .into_iter()
            .map(|path| {
                let svg = bundled.files.read(&path).unwrap();
                (path, String::from_utf8(svg.into_owned()).unwrap())
            })
        {
            assert!(svg.contains("viewBox=\"0 0 16 16\""), "{path}");
        }
    }

    #[test]
    fn exact_names_beat_prefixes_beat_extensions() {
        let set = parse_icon_theme(
            "test.set",
            r##"
name = "Test"
[file-names]
"Special.TXT" = { icon = "icons/special.svg", color = "red" }
[name-prefixes]
"lic" = { icon = "icons/short.svg", color = "blue" }
"license" = { icon = "icons/long.svg", color = "green" }
[extensions]
"txt" = { icon = "icons/text.svg", color = "dim" }
"d.ts" = { icon = "icons/dts.svg", color = "cyan" }
"ts" = { icon = "icons/ts.svg", color = "blue" }
"tar.gz" = { icon = "icons/tgz.svg", color = "amber" }
"##,
        )
        .unwrap();
        let ui = Theme::flux_night().ui;
        let stem = |name: &str| icon_of(&set, name, &ui).map(|icon| stem_and_color(&icon).0);
        assert_eq!(stem("special.txt").as_deref(), Some("special"));
        assert_eq!(stem("LICENSE.txt").as_deref(), Some("long"), "the longest prefix");
        assert_eq!(stem("lichen.txt").as_deref(), Some("short"));
        assert_eq!(stem("notes.txt").as_deref(), Some("text"));
        assert_eq!(stem("index.d.ts").as_deref(), Some("dts"), "the longest extension");
        assert_eq!(stem("index.ts").as_deref(), Some("ts"));
        assert_eq!(stem("a.b.tar.gz").as_deref(), Some("tgz"));
        assert_eq!(stem("plain.gz"), None);
        assert_eq!(stem("Makefile"), None);
        assert_eq!(stem(""), None);
    }

    #[test]
    fn language_icons_come_after_the_set_and_before_its_default() {
        let set = parse_icon_theme(
            "test.set",
            r##"
name = "Test"
[defaults]
file = { icon = "icons/file.svg", color = "dim" }
[extensions]
"rs" = { icon = "icons/rust.svg", color = "orange" }
"##,
        )
        .unwrap();
        let mut languages = Rules::default();
        for (extension, icon) in [("rs", "icons/ferris.svg"), ("kt", "icons/kotlin.svg")] {
            add_language_icon(
                &mut languages,
                &LanguageIcon {
                    plugin: "test.lang".into(),
                    extensions: vec![extension.into()],
                    file_names: vec!["Kotlinfile".into()],
                    icon: icon.into(),
                    color: Some("violet".into()),
                },
                Entry {
                    source: IconSource::Asset(icons::plugin_asset("test.lang", icon)),
                    color: Some(ColorRef::Token(|ui| ui.violet)),
                    tint: None,
                },
            );
        }
        let ui = Theme::flux_night().ui;
        let stem = |name: &str| {
            resolve_with(Some(&set), &languages, &name.to_ascii_lowercase(), &ui)
                .map(|icon| stem_and_color(&icon).0)
        };
        assert_eq!(stem("main.rs").as_deref(), Some("rust"), "the set wins");
        assert_eq!(stem("Main.kt").as_deref(), Some("kotlin"), "then the language's");
        assert_eq!(stem("kotlinfile").as_deref(), Some("kotlin"));
        assert_eq!(stem("notes.xyz").as_deref(), Some("file"), "then the set's default");
        let without_set = resolve_with(None, &languages, "main.rs", &ui).unwrap();
        assert_eq!(stem_and_color(&without_set).0, "ferris");
        assert_eq!(resolve_with(None, &languages, "notes.xyz", &ui), None);
    }

    #[test]
    fn colors_follow_the_theme_and_full_color_icons_keep_theirs() {
        let set = parse_icon_theme(
            "test.set",
            r##"
name = "Test"
[defaults]
folder = { icon = "icons/folder.svg", color = "folder" }
[extensions]
"rs" = { icon = "icons/rust.svg", color = "orange" }
"go" = { icon = "icons/go.svg", color = "#00add8" }
"py" = { icon = "icons/python.svg", tint = "blue" }
"ts" = { icon = "icons/ts.svg" }
"##,
        )
        .unwrap();
        let night = Theme::flux_night().ui;
        let mut other = night;
        other.orange = gpui::rgb(0x123456).into();
        other.folder = gpui::rgb(0x654321).into();
        let rust = |ui: &UiColors| icon_of(&set, "a.rs", ui).unwrap();
        assert_eq!(rust(&night).color, night.orange);
        assert_eq!(rust(&other).color, other.orange, "a shade of the theme in use");
        assert!(!rust(&night).multicolor);
        let go = icon_of(&set, "a.go", &other).unwrap();
        assert_eq!(go.color, Hsla::from(gpui::rgb(0x00add8)));
        let python = icon_of(&set, "a.py", &night).unwrap();
        assert!(python.multicolor);
        assert_eq!(python.color, night.blue, "a full-color icon's tint");
        let ts = icon_of(&set, "a.ts", &night).unwrap();
        assert!(ts.multicolor);
        assert_eq!(ts.color, night.text_muted);
        assert_eq!(
            folder_with(Some(&set), true, &other).unwrap().color,
            other.folder,
            "an open folder takes the closed one's icon when the set has none"
        );
    }

    #[test]
    fn mistakes_name_the_key() {
        let error = |text: &str| parse_icon_theme("test.set", text).unwrap_err();
        assert!(error("[extensions]\nrs = { icon = \"icons/rust.svg\" }").contains("name"));
        assert!(
            error("name = \"T\"\n[extensions]\nrs = { icon = \"icons/rust.svg\", color = \"oranje\" }")
                .contains("extensions.\"rs\".color")
        );
        assert!(
            error("name = \"T\"\n[extensions]\nrs = { icon = \"../rust.svg\", color = \"red\" }")
                .contains("extensions.\"rs\".icon")
        );
        assert!(
            error("name = \"T\"\n[extensions]\nrs = { icon = \"icons/rust.png\", color = \"red\" }")
                .contains("SVG")
        );
        assert!(
            error("name = \"T\"\n[extensions]\nrs = { icon = \"icons/rust.svg\", size = 3 }")
                .contains("extensions.\"rs\".size")
        );
        assert!(error("name = \"T\"\n[extensions]\n\".rs\" = { icon = \"icons/rust.svg\" }").contains("dot"));
        assert!(error("name = \"T\"\n[folders]\nsrc = { icon = \"icons/src.svg\" }").contains("folders"));
        assert!(error("name = \"T\"\n[defaults]\ndir = { icon = \"icons/d.svg\" }").contains("defaults.dir"));
        assert!(
            error("name = \"T\"\n[extensions]\nrs = { icon = \"icons/r.svg\", color = \"red\", tint = \"red\" }")
                .contains("tint")
        );
        assert!(error("name = \"T\"").contains("no icons"));
        assert!(error("name = ").contains("string") || !error("name = ").is_empty());
    }

    #[test]
    fn missing_svgs_fall_back() {
        let mut set = parse_icon_theme(
            "test.set",
            r##"
name = "Test"
[defaults]
file = { icon = "icons/gone.svg", color = "dim" }
[extensions]
"rs" = { icon = "icons/rust.svg", color = "orange" }
"go" = { icon = "icons/gone.svg", color = "cyan" }
"##,
        )
        .unwrap();
        let missing = set.drop_missing(|path| path == "icons/rust.svg");
        assert_eq!(missing, ["icons/gone.svg"]);
        let ui = Theme::flux_night().ui;
        assert!(icon_of(&set, "a.rs", &ui).is_some());
        assert_eq!(icon_of(&set, "a.go", &ui), None);
    }

    #[test]
    fn lowercasing_on_the_stack() {
        let mut buffer = [0u8; NAME_BUFFER];
        assert_eq!(lowercase_into("Main.RS", &mut buffer), Some("main.rs"));
        let mut buffer = [0u8; NAME_BUFFER];
        assert_eq!(lowercase_into("Файл.TXT", &mut buffer), Some("Файл.txt"));
        let long = "A".repeat(NAME_BUFFER + 1);
        let mut buffer = [0u8; NAME_BUFFER];
        assert_eq!(lowercase_into(&long, &mut buffer), None);
    }

    #[test]
    fn plain_icons_without_a_set() {
        let ui = Theme::flux_night().ui;
        let samples = sample_icons(Some("No Such Set"), &ui);
        assert_eq!(samples.len(), SAMPLE_FILES.len() + 1);
        assert!(
            samples[..SAMPLE_FILES.len()]
                .iter()
                .all(|icon| icon.source == IconSource::Builtin(IconName::File))
        );
        assert_eq!(
            samples.last().unwrap().source,
            IconSource::Builtin(IconName::Folder)
        );
    }

    /// A smoke test of the cost per row: the lookups are map hits, no parsing.
    #[test]
    fn resolving_is_cheap() {
        let set = flux_set();
        let ui = Theme::flux_night().ui;
        let languages = Rules::default();
        let start = Instant::now();
        let rounds = 200;
        for _ in 0..rounds {
            for name in NAMES {
                let mut buffer = [0u8; NAME_BUFFER];
                let lower = lowercase_into(name, &mut buffer).unwrap();
                std::hint::black_box(resolve_with(Some(&set), &languages, lower, &ui));
            }
        }
        let per_call = start.elapsed() / (rounds * NAMES.len()) as u32;
        eprintln!("resolve_with: {per_call:?} per name");
        // Debug builds are slow; this only catches something going quadratic.
        assert!(per_call.as_micros() < 50, "{per_call:?}");
    }
}
