//! The languages (the guide's third slide): a tile per language of the catalog, the ones found on
//! this Mac chosen beforehand; Continue installs the chosen ones' plugins in the background, with
//! their progress on the tiles. JavaScript and TypeScript come with Flux.
//!
//! "Found" means a toolchain's program is on the login shell's PATH or in its usual home (`cargo`,
//! `go`, a `python3` that isn't macOS's stub…); nothing is run. The language server isn't installed
//! here: Flux brings it when the first file of the language opens (Settings → Language Servers).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flux_plugin::catalog::{Category, Index, IndexEntry};
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, ClickEvent, Context, Div, FontWeight, Hsla,
    Render, SharedString, Stateful, Task, Window, div, prelude::*, px,
};

use super::heading;
use crate::i18n::{lang_code, tr, trn};
use crate::icons::{self, IconName, icon};
use crate::notifications::{Progress, progress_bar};
use crate::theme::{self, Theme, UiColors};
use crate::{plugin_catalog, settings, ui};

/// A tile: three fit in a row of the slide, their lines unclipped in Russian too.
const TILE_WIDTH: f32 = 294.;
const TILE_HEIGHT: f32 = 54.;
const TILE_GAP: f32 = 10.;

/// The languages in the order of the slide, the most common first; the catalog's other languages
/// follow them by name.
const ORDER: &[&str] = &[
    "flux.rust",
    "flux.python",
    "flux.go",
    "flux.markdown",
    "flux.toml",
    "flux.yaml",
    "flux.json",
    "flux.html",
    "flux.css",
    "flux.shell",
    "flux.dockerfile",
    "flux.sql",
    "flux.makefile",
];
/// Where the built-in JavaScript and TypeScript go among them.
const BUILT_IN_AT: usize = 3;
/// In every project (a README): chosen even when nothing on the Mac points at it.
const EVERYWHERE: &[&str] = &["flux.markdown"];

/// The programs that tell a language is used on this Mac, by its plugin.
const TOOLCHAINS: &[(&str, &[&str])] = &[
    ("flux.rust", &["cargo", "rustc", "rustup"]),
    ("flux.go", &["go"]),
    (
        "flux.python",
        &["python3", "python", "uv", "poetry", "pyenv"],
    ),
    ("flux.dockerfile", &["docker", "podman"]),
    ("flux.sql", &["psql", "mysql", "pg_ctl"]),
    ("flux.yaml", &["kubectl", "helm"]),
    ("flux.json", &["node", "npm", "deno", "bun"]),
];
/// What comes along with a found language: `Cargo.toml` is TOML, a Compose file is YAML.
const ALONG: &[(&str, &str)] = &[("flux.rust", "flux.toml"), ("flux.dockerfile", "flux.yaml")];
/// Folders whose programs say nothing about the user: macOS's own (its `python3` only offers to
/// install the developer tools, `make` and `sqlite3` are always there) and the developer tools'.
const SYSTEM_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
const SYSTEM_PREFIXES: &[&str] = &["/System/", "/Library/Developer/", "/Applications/Xcode"];
/// The usual homes of toolchains, looked in besides the PATH (`~` is the home folder).
const HOMES: &[&str] = &[
    "~/.cargo/bin",
    "~/.local/bin",
    "~/.pyenv/shims",
    "~/.pyenv/bin",
    "~/.bun/bin",
    "~/.deno/bin",
    "~/.volta/bin",
    "~/.docker/bin",
    "/usr/local/go/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/Applications/Docker.app/Contents/Resources/bin",
];

/// A language plugin of the catalog, as its tile needs it.
struct Language {
    entry: Arc<IndexEntry>,
    /// The name in the interface language.
    name: SharedString,
    /// It brings a language server (errors, completion), not only highlighting.
    server: bool,
    /// The shade of its icon (`icon-color` of its first language).
    color: Option<String>,
}

/// The languages of the catalog's index, with the index they were read from.
struct Catalog {
    index: Arc<Index>,
    languages: Vec<Language>,
}

pub struct LanguagesStep {
    catalog: Option<Catalog>,
    /// The plugins whose languages this Mac has programs for; `None` while looking.
    found: Option<HashSet<String>>,
    /// The checked plugins.
    chosen: BTreeSet<String>,
    /// The user checked or unchecked a tile: what is found later doesn't change the choice.
    touched: bool,
    /// The plugins Flux has (bundled, installed, under development); `None` until read.
    installed: Option<HashSet<String>>,
    /// Installs from here that failed, by plugin: why.
    failed: HashMap<String, SharedString>,
    /// Plugins installed from here: "ready", not "already installed".
    done: HashSet<String>,
    /// The chosen plugins neither installed nor being installed: the main button's count.
    pending: usize,
    _tasks: Vec<Task<()>>,
}

impl LanguagesStep {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The catalog is read once per run; its disk cache is drawn meanwhile.
        plugin_catalog::index(cx);
        plugin_catalog::ensure_fresh(cx);
        let find = cx.background_spawn(async move {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let path = flux_plugin::login_env()
                .iter()
                .find(|(name, _)| name == "PATH")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            found_on_this_mac(&search_dirs(&path, &home), is_program)
        });
        let found = cx.spawn(async move |this, cx| {
            let found = find.await;
            this.update(cx, |this, cx| {
                this.found = Some(found);
                this.choose_found();
                this.recount(cx);
                cx.notify();
                // The guide's main button tells how many will be installed.
                cx.refresh_windows();
            })
            .ok();
        });
        let mut step = Self {
            catalog: None,
            found: None,
            chosen: BTreeSet::new(),
            touched: false,
            installed: None,
            failed: HashMap::new(),
            done: HashSet::new(),
            pending: 0,
            _tasks: vec![found],
        };
        step.read_installed(cx);
        step
    }

    /// The main button's label while this slide shows ("Install 4 and Continue"); `None` —
    /// Continue.
    pub fn next_label(&self) -> Option<SharedString> {
        (self.pending > 0).then(|| {
            trn(
                self.pending,
                "Install {n} and Continue",
                "Install {n} and Continue",
            )
            .into()
        })
    }

    /// Continue: the chosen languages' plugins start installing (in the background; they go on
    /// while the guide shows other slides).
    pub fn install_chosen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_catalog(cx);
        let installed = self.installed.clone().unwrap_or_default();
        let entries: Vec<IndexEntry> = self
            .chosen
            .iter()
            .filter(|id| !installed.contains(*id) && plugin_catalog::progress(id, cx).is_none())
            .filter_map(|id| self.language(id).map(|language| (*language.entry).clone()))
            .collect();
        for entry in entries {
            self.install(entry, window.window_handle(), cx);
        }
        self.recount(cx);
    }

    fn install(&mut self, entry: IndexEntry, window: AnyWindowHandle, cx: &mut Context<Self>) {
        let id = entry.id.clone();
        self.failed.remove(&id);
        let install = plugin_catalog::install_quietly(entry, Some(window), cx);
        // Detached: the install goes on even if the guide closes meanwhile.
        cx.spawn(async move |this, cx| {
            let result = install.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(true) => {
                        this.done.insert(id);
                    }
                    Ok(false) => {}
                    Err(reason) => {
                        this.failed.insert(id, reason);
                    }
                }
                this.read_installed(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads which plugins Flux has (in the background), then counts again.
    fn read_installed(&mut self, cx: &mut Context<Self>) {
        let dev = settings::dev_plugins(cx);
        let scan = cx.background_spawn(async move {
            flux_plugin::registry::scan(crate::bundled::plugins(), &dev)
                .plugins
                .into_iter()
                .map(|entry| entry.id().to_string())
                .collect::<HashSet<String>>()
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let installed = scan.await;
            this.update(cx, |this, cx| {
                this.installed = Some(installed);
                this.recount(cx);
                cx.notify();
                cx.refresh_windows();
            })
            .ok();
        }));
    }

    /// Until the user touches a tile, the found languages (and what comes along with them) are the
    /// choice.
    fn choose_found(&mut self) {
        if self.touched {
            return;
        }
        let Some(found) = &self.found else {
            return;
        };
        self.chosen = preselected(found);
    }

    fn recount(&mut self, cx: &App) {
        let installed = self.installed.clone().unwrap_or_default();
        self.pending = self
            .chosen
            .iter()
            .filter(|id| {
                !installed.contains(*id)
                    && plugin_catalog::progress(id, cx).is_none()
                    && self.language(id).is_some()
            })
            .count();
    }

    fn language(&self, id: &str) -> Option<&Language> {
        self.catalog
            .as_ref()?
            .languages
            .iter()
            .find(|language| language.entry.id == id)
    }

    /// The catalog's language plugins, read again when the index changed.
    fn refresh_catalog(&mut self, cx: &App) {
        let Some(index) = plugin_catalog::peek(cx) else {
            return;
        };
        if self
            .catalog
            .as_ref()
            .is_some_and(|catalog| Arc::ptr_eq(&catalog.index, &index))
        {
            return;
        }
        let mut languages: Vec<Language> = index
            .plugins
            .iter()
            .filter(|entry| {
                entry.compatible()
                    && entry.categories.contains(&Category::Languages)
                    && !entry.languages.is_empty()
            })
            .map(|entry| {
                let manifest = entry.parse_manifest().ok();
                Language {
                    name: entry.translate(lang_code(), &entry.name).to_string().into(),
                    server: manifest
                        .as_ref()
                        .is_some_and(|manifest| !manifest.language_servers.is_empty()),
                    color: manifest.and_then(|manifest| {
                        manifest
                            .languages
                            .into_iter()
                            .find_map(|language| language.icon_color)
                    }),
                    entry: Arc::new(entry.clone()),
                }
            })
            .collect();
        languages.sort_by(|a, b| {
            let place = |id: &str| ORDER.iter().position(|known| *known == id);
            match (place(&a.entry.id), place(&b.entry.id)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            }
        });
        self.catalog = Some(Catalog { index, languages });
    }

    fn toggle(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.touched = true;
        if !self.chosen.remove(id) {
            self.chosen.insert(id.to_string());
        }
        self.recount(cx);
        cx.notify();
        // The guide's main button counts the chosen ones.
        window.refresh();
    }

    fn render_tile(
        &self,
        index: usize,
        language: &Language,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let id = language.entry.id.clone();
        let installed = self
            .installed
            .as_ref()
            .is_some_and(|installed| installed.contains(&id));
        let progress = plugin_catalog::progress(&id, cx);
        let failed = self.failed.get(&id).cloned();
        let chosen = self.chosen.contains(&id);
        let shade = shade(language.color.as_deref(), ui);
        let (subtitle, subtitle_color): (SharedString, Hsla) = match (&progress, &failed) {
            _ if installed && self.done.contains(&id) => (tr("Ready to use").into(), ui.success),
            _ if installed => (tr("Already installed").into(), ui.dim),
            (Some(progress), _) if progress.installing() => (tr("Installing…").into(), ui.dim),
            (Some(progress), _) => (progress.label().into(), ui.dim),
            (None, Some(_)) => (tr("Couldn't install").into(), ui.error),
            _ if self.found.as_ref().is_some_and(|found| found.contains(&id)) => {
                (tr("Found on this Mac").into(), ui.success)
            }
            _ if EVERYWHERE.contains(&id.as_str()) => (tr("In every project").into(), ui.dim),
            _ if language.server => (tr("Highlighting, errors, completion").into(), ui.dim),
            _ => (tr("Highlighting").into(), ui.dim),
        };
        let trailing: AnyElement = if installed {
            icon(IconName::CheckCircle, ui.success)
                .size(px(16.))
                .into_any_element()
        } else if progress.is_some() {
            div().into_any_element()
        } else if failed.is_some() {
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ui.accent_text)
                .child(tr("Retry"))
                .into_any_element()
        } else {
            ui::checkbox(
                SharedString::from(format!("welcome-language-check-{id}")),
                ui::CheckState::from_bool(chosen),
                ui,
            )
            .into_any_element()
        };
        let selectable = !installed && progress.is_none();
        let highlighted = selectable && chosen && failed.is_none();
        let entry = (*language.entry).clone();
        let tile_id = id.clone();
        div()
            .id(("welcome-language", index))
            .relative()
            .w(px(TILE_WIDTH))
            .h(px(TILE_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap(px(11.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(if highlighted {
                UiColors::tint(ui.accent, 0.7)
            } else {
                ui.island_border
            })
            .bg(if highlighted {
                ui.accent_soft
            } else {
                ui.hover
            })
            .when(selectable, |tile| {
                tile.cursor_pointer()
                    .hover(move |style| style.border_color(UiColors::tint(ui.accent, 0.5)))
            })
            .when(selectable, |tile| {
                tile.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if this.failed.contains_key(&tile_id) {
                        this.install(entry.clone(), window.window_handle(), cx);
                        this.recount(cx);
                        cx.notify();
                    } else {
                        this.toggle(&tile_id, window, cx);
                    }
                }))
            })
            .child(icon_tile(
                plugin_catalog::entry_icon(&language.entry, 17., shade),
                shade,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui.foreground)
                            .child(language.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_XS + 0.5))
                            .text_color(subtitle_color)
                            .child(subtitle),
                    )
                    .children(progress.map(|progress| {
                        progress_bar(
                            match progress.fraction() {
                                Some(done) if !progress.installing() => Progress::Fraction(done),
                                _ => Progress::Indeterminate,
                            },
                            ui,
                        )
                    })),
            )
            .child(trailing)
    }

    /// JavaScript and TypeScript: in Flux already, checked and locked.
    fn render_built_in(&self, cx: &App) -> Div {
        let ui = Theme::ui(cx);
        let script = icons::file_icon("index.ts", &ui);
        let shade = if script.multicolor {
            ui.amber
        } else {
            script.color
        };
        div()
            .w(px(TILE_WIDTH))
            .h(px(TILE_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap(px(11.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(ui.island_border)
            .bg(ui.hover)
            .child(icon_tile(script.render().into_any_element(), shade))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui.foreground)
                            .child(tr("JavaScript · TypeScript")),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_XS + 0.5))
                            .text_color(ui.dim)
                            .child(tr("Come with Flux")),
                    ),
            )
            .child(
                ui::checkbox("welcome-language-built-in", ui::CheckState::Checked, ui).opacity(0.5),
            )
    }

    /// The catalog isn't there: being read, or it can't be reached.
    fn render_no_catalog(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let failed = plugin_catalog::error(cx).is_some() && !plugin_catalog::is_loading(cx);
        div()
            .flex()
            .items_center()
            .gap_3()
            .text_size(px(theme::TEXT_SM + 0.5))
            .text_color(ui.text_muted)
            .child(
                icon(if failed { IconName::Warning } else { IconName::Download }, ui.dim)
                    .size(px(14.)),
            )
            .child(div().child(if failed {
                tr("The catalog can't be reached. Languages can be installed later: Settings → Plugins.")
            } else {
                tr("Reading the catalog…")
            }))
            .when(failed, |row| {
                row.child(ui::text_button("welcome-catalog-retry", tr("Retry"), false, ui).on_click(
                    cx.listener(|_, _: &ClickEvent, _, cx| {
                        plugin_catalog::fetch(cx).detach();
                        cx.notify();
                    }),
                ))
            })
    }
}

impl Render for LanguagesStep {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        // The catalog may have just come, an install started or ended: the guide's main button,
        // drawn before this slide, says the new count on the next frame (a refresh asked for
        // while the window draws is dropped, so it is asked for after).
        let pending = self.pending;
        self.refresh_catalog(cx);
        self.recount(cx);
        if self.pending != pending {
            cx.defer_in(window, |_, window, _| window.refresh());
        }
        let mut tiles: Vec<AnyElement> = Vec::new();
        let languages = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.languages.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for (index, language) in languages.iter().enumerate() {
            if index == BUILT_IN_AT {
                tiles.push(self.render_built_in(cx).into_any_element());
            }
            tiles.push(self.render_tile(index, language, cx).into_any_element());
        }
        if languages.len() <= BUILT_IN_AT {
            tiles.push(self.render_built_in(cx).into_any_element());
        }
        div()
            .id("welcome-languages")
            .size_full()
            .overflow_y_scroll()
            .px(px(36.))
            .pt(px(28.))
            .pb(px(24.))
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(heading(
                tr("Setup · Languages"),
                tr("What Do You Write In?"),
                tr(
                    "The languages found on this Mac are checked. Flux installs their plugins from the catalog; a language server comes by itself when you open the first file.",
                ),
                ui,
            ))
            .child(div().flex().flex_wrap().gap(px(TILE_GAP)).children(tiles))
            .when(self.catalog.is_none(), |slide| {
                slide.child(self.render_no_catalog(cx))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(theme::TEXT_SM + 0.5))
                    .text_color(ui.dim)
                    .child(icon(IconName::Puzzle, ui.dim).size(px(13.)))
                    .child(tr(
                        "Themes, file icons and tools are in the Marketplace: Settings → Plugins.",
                    )),
            )
    }
}

/// A language's icon on a square of its shade.
fn icon_tile(icon: AnyElement, shade: Hsla) -> Div {
    div()
        .flex_none()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .bg(UiColors::tint(shade, 0.14))
        .child(icon)
}

/// The shade a language plugin names for its icon (`icon-color`), in the theme's colors.
fn shade(name: Option<&str>, ui: UiColors) -> Hsla {
    match name.map(|name| name.replace('_', "-")).as_deref() {
        Some("blue") => ui.blue,
        Some("indigo") => ui.indigo,
        Some("violet") => ui.violet,
        Some("pink") => ui.pink,
        Some("red") => ui.red,
        Some("orange") => ui.orange,
        Some("amber") => ui.amber,
        Some("lime") => ui.lime,
        Some("green") => ui.green,
        Some("teal") => ui.teal,
        Some("cyan") => ui.cyan,
        Some(hex) if hex.starts_with('#') => u32::from_str_radix(&hex[1..], 16)
            .ok()
            .filter(|_| hex.len() == 7)
            .map_or(ui.text_muted, |value| gpui::rgb(value).into()),
        _ => ui.text_muted,
    }
}

/// The folders to look for toolchains in: the login shell's PATH and the usual homes, without
/// macOS's own folders and the developer tools'.
fn search_dirs(path: &str, home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let homes = HOMES.iter().map(|dir| match dir.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(dir),
    });
    for dir in path
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .chain(homes)
    {
        let text = dir.to_string_lossy();
        let system = SYSTEM_DIRS
            .iter()
            .any(|system| text.trim_end_matches('/') == *system)
            || SYSTEM_PREFIXES
                .iter()
                .any(|prefix| text.starts_with(prefix));
        if !system && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The plugins whose toolchains are in `dirs` (`is_program` — an executable file is there).
fn found_on_this_mac(dirs: &[PathBuf], is_program: impl Fn(&Path) -> bool) -> HashSet<String> {
    TOOLCHAINS
        .iter()
        .filter(|(_, programs)| {
            programs
                .iter()
                .any(|program| dirs.iter().any(|dir| is_program(&dir.join(program))))
        })
        .map(|(plugin, _)| plugin.to_string())
        .collect()
}

/// The choice before the user touches it: the found languages, what comes along with them, and
/// what every project has.
fn preselected(found: &HashSet<String>) -> BTreeSet<String> {
    let mut chosen: BTreeSet<String> = found.iter().cloned().collect();
    for (with, along) in ALONG {
        if found.contains(*with) {
            chosen.insert(along.to_string());
        }
    }
    chosen.extend(EVERYWHERE.iter().map(|id| id.to_string()));
    chosen
}

/// An executable file (a link to one counts: Homebrew's programs are links).
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_own_folders_say_nothing() {
        let home = Path::new("/Users/someone");
        let dirs = search_dirs(
            "/usr/bin:/opt/homebrew/bin:/bin:/Library/Developer/CommandLineTools/usr/bin:/usr/bin/",
            home,
        );
        assert!(
            !dirs
                .iter()
                .any(|dir| dir.starts_with("/usr/bin") || dir == Path::new("/bin"))
        );
        assert!(!dirs.iter().any(|dir| dir.starts_with("/Library/Developer")));
        // The usual homes are looked in too, once each.
        assert!(dirs.contains(&home.join(".cargo/bin")));
        assert_eq!(
            dirs.iter()
                .filter(|dir| *dir == Path::new("/opt/homebrew/bin"))
                .count(),
            1
        );
    }

    #[test]
    fn toolchains_found_on_disk_choose_their_languages() {
        let root = std::env::temp_dir().join(format!("flux-welcome-langs-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = |name: &str, executable: bool| {
            use std::os::unix::fs::PermissionsExt;
            let path = bin.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        program("cargo", true);
        program("docker", true);
        // A file that can't run isn't a program.
        program("go", false);
        let found = found_on_this_mac(std::slice::from_ref(&bin), is_program);
        assert_eq!(
            found,
            HashSet::from(["flux.rust".to_string(), "flux.dockerfile".to_string()])
        );
        let chosen = preselected(&found);
        // Cargo.toml and Compose files come along; Markdown is in every project.
        for id in [
            "flux.rust",
            "flux.toml",
            "flux.dockerfile",
            "flux.yaml",
            "flux.markdown",
        ] {
            assert!(chosen.contains(id), "{id} isn't chosen");
        }
        assert!(!chosen.contains("flux.go"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn every_toolchain_names_a_language_of_the_order() {
        for (plugin, _) in TOOLCHAINS {
            assert!(ORDER.contains(plugin), "{plugin} isn't in ORDER");
        }
        for (with, along) in ALONG {
            assert!(ORDER.contains(with) && ORDER.contains(along));
        }
    }
}
