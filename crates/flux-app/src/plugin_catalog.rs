//! The Marketplace (stage 8.3): the catalog's plugins in Settings → Plugins, as the Marketplace tab
//! of JetBrains IDEs — search, categories, details, Install and Update.
//!
//! The catalog's index lives in [`Catalog`] for the whole process: read from the disk cache on first
//! use, then from the catalog ([`fetch`]) — the Marketplace, the updates at start
//! ([`crate::plugin_updates`]) and the suggestions above the editor ([`crate::plugin_suggestions`])
//! all look at it. Installing from the catalog ([`install`]) downloads the package, checks its
//! SHA-256, asks the question about the plugin's permissions (an update only when it asks for
//! more than the installed version) and installs it as a plugin from disk is installed; the
//! progress is drawn where the install was started from.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use flux_plugin::catalog::{self, Category, Index, IndexEntry};
use flux_plugin::registry::{PluginFiles, PluginSource};
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, ClickEvent, Context, Div, Entity,
    FocusHandle, Focusable, FontWeight, Global, Hsla, Render, SharedString, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px, svg,
};

use crate::dialog::Dialog;
use crate::i18n::{lang_code, tr, trf};
use crate::icons::{self, IconName, icon};
use crate::input::{InputEvent, TextInput};
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::plugin_manager::{self, INSTALL};
use crate::plugins::PluginStore;
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::workspace::Workspace;

/// The list of plugins; the details take the rest of the page (as on the Installed tab).
const LIST_WIDTH: f32 = 284.;
const LIST_ICON: f32 = 18.;
const DETAILS_ICON: f32 = 30.;
const TABS_HEIGHT: f32 = 34.;

// --- The catalog of the process ---

/// The catalog's index and the installs from it, for the whole process.
#[derive(Default)]
pub struct Catalog {
    index: Option<Arc<Index>>,
    /// The disk cache was looked at.
    disk_checked: bool,
    /// Read from the catalog during this run (the disk cache is only a start).
    fresh: bool,
    loading: bool,
    /// Why the last read failed.
    error: Option<SharedString>,
    /// Installs and updates in progress, by plugin id.
    installs: HashMap<SharedString, InstallProgress>,
}

impl Global for Catalog {}

/// An install from the catalog: the download's progress, its cancel flag.
#[derive(Clone, Default)]
pub struct InstallProgress {
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    /// Downloaded: unpacking, asking, installing.
    installing: Arc<AtomicBool>,
}

impl InstallProgress {
    /// The part downloaded (0–1); none while the size isn't known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.done.load(Ordering::Relaxed) as f32 / total as f32).min(1.))
    }

    pub fn installing(&self) -> bool {
        self.installing.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// "Downloading 45%", "Installing…".
    pub fn label(&self) -> String {
        if self.installing() {
            return tr("Installing…").to_string();
        }
        match self.fraction() {
            Some(fraction) => trf("Downloading {0}%", &[&((fraction * 100.).round() as u32)]),
            None => tr("Downloading…").to_string(),
        }
    }
}

/// The catalog's index: in memory, otherwise the disk cache (looked at once).
pub fn index(cx: &mut App) -> Option<Arc<Index>> {
    let catalog = cx.default_global::<Catalog>();
    if catalog.index.is_none() && !catalog.disk_checked {
        catalog.disk_checked = true;
        catalog.index = catalog::cached().map(Arc::new);
    }
    catalog.index.clone()
}

/// The index in memory, without looking at the disk (for drawing).
pub fn peek(cx: &App) -> Option<Arc<Index>> {
    cx.try_global::<Catalog>()
        .and_then(|catalog| catalog.index.clone())
}

pub fn is_loading(cx: &App) -> bool {
    cx.try_global::<Catalog>()
        .is_some_and(|catalog| catalog.loading)
}

/// Why the catalog couldn't be read the last time.
pub fn error(cx: &App) -> Option<SharedString> {
    cx.try_global::<Catalog>()
        .and_then(|catalog| catalog.error.clone())
}

/// The install of a plugin in progress.
pub fn progress(id: &str, cx: &App) -> Option<InstallProgress> {
    cx.try_global::<Catalog>()
        .and_then(|catalog| catalog.installs.get(id).cloned())
}

/// Reads the catalog in the background; the Marketplace, the updates and the suggestions follow.
/// The task gives the index, or why it couldn't be read.
pub fn fetch(cx: &mut App) -> Task<Result<Arc<Index>, SharedString>> {
    let url = catalog::index_url();
    {
        let catalog = cx.default_global::<Catalog>();
        catalog.loading = true;
    }
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move { catalog::fetch(&url) })
            .await
            .map(Arc::new)
            .map_err(SharedString::from);
        cx.update(|cx| {
            let catalog = cx.default_global::<Catalog>();
            catalog.loading = false;
            catalog.disk_checked = true;
            match &result {
                Ok(index) => {
                    catalog.index = Some(index.clone());
                    catalog.fresh = true;
                    catalog.error = None;
                }
                Err(error) => catalog.error = Some(error.clone()),
            }
            cx.refresh_windows();
        })
        .ok();
        result
    })
}

/// Reads the catalog once per run (the Marketplace opened, a file without a language).
pub fn ensure_fresh(cx: &mut App) {
    let catalog = cx.default_global::<Catalog>();
    if !catalog.fresh && !catalog.loading && catalog.error.is_none() {
        fetch(cx).detach();
    }
}

/// The installed plugin with this id: its version and where it comes from.
pub fn installed(store: &PluginStore, id: &str) -> Option<(String, PluginSource)> {
    store
        .plugin(id)
        .map(|plugin| (plugin.entry.manifest.version.clone(), plugin.source()))
}

/// What the catalog offers for a plugin, compared with what is installed.
#[derive(Debug, Clone, PartialEq)]
pub enum Offer {
    Install,
    /// A newer version than the installed one.
    Update { from: String },
    /// The installed version (as new as the catalog's, or newer).
    Installed { version: String },
    /// Under development: the author's folder wins, the catalog doesn't touch it.
    Development,
    /// Built for a plugin API this Flux doesn't run.
    Incompatible,
}

pub fn offer(entry: &IndexEntry, store: &PluginStore) -> Offer {
    match installed(store, &entry.id) {
        Some((_, PluginSource::Dev)) => Offer::Development,
        Some((version, _)) if !entry.compatible() => Offer::Installed { version },
        Some((version, _)) if catalog::is_newer(&entry.version, &version) => {
            Offer::Update { from: version }
        }
        Some((version, _)) => Offer::Installed { version },
        None if !entry.compatible() => Offer::Incompatible,
        None => Offer::Install,
    }
}

/// The installed plugins (downloaded and bundled; not under development) the catalog has a newer
/// version of.
pub fn updates(store: &PluginStore, index: &Index) -> Vec<IndexEntry> {
    let installed: Vec<(String, String)> = store
        .plugins()
        .iter()
        .filter(|plugin| plugin.source() != PluginSource::Dev)
        .map(|plugin| {
            (
                plugin.id().to_string(),
                plugin.entry.manifest.version.clone(),
            )
        })
        .collect();
    catalog::updates(index, &installed)
        .into_iter()
        .cloned()
        .collect()
}

// --- Installing ---

/// Installs (or updates) a plugin of the catalog: download, checksum, the question about its
/// permissions, install; then a notification. Does nothing while the same plugin is being
/// installed.
pub fn install(
    entry: IndexEntry,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let id = SharedString::from(entry.id.clone());
    let progress = {
        let catalog = cx.default_global::<Catalog>();
        if catalog.installs.contains_key(&id) {
            return;
        }
        let progress = InstallProgress::default();
        catalog.installs.insert(id.clone(), progress.clone());
        progress
    };
    cx.refresh_windows();
    window
        .spawn(cx, async move |cx| {
            let updating = workspace
                .read_with(cx, |workspace, cx| {
                    workspace.plugins.read(cx).plugin(&entry.id).is_some()
                })
                .unwrap_or(false);
            let result = run_install(&entry, &progress, &workspace, cx).await;
            cx.update(|_, cx| {
                cx.default_global::<Catalog>().installs.remove(&id);
                cx.refresh_windows();
            })
            .ok();
            match result {
                Ok(true) => {
                    let title = if updating {
                        trf("“{0}” is updated to {1}", &[&entry.name, &entry.version])
                    } else {
                        trf("“{0}” {1} is installed", &[&entry.name, &entry.version])
                    };
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.notify(
                                Notification::success(title)
                                    .group(NotificationGroup::Plugins)
                                    .transient(),
                                cx,
                            );
                            // Nothing left to update: the card offering updates goes.
                            let left = peek(cx).is_some_and(|index| {
                                !updates(workspace.plugins.read(cx), &index).is_empty()
                            });
                            if !left {
                                workspace.dismiss_notifications(
                                    &["plugin_updates::UpdatePlugins"],
                                    cx,
                                );
                            }
                        })
                        .ok();
                }
                Ok(false) => {}
                Err(reason) => {
                    Dialog::warning(trf("Couldn't install “{0}”", &[&entry.name]))
                        .message(reason)
                        .primary(tr("OK"))
                        .show_async(cx)
                        .await;
                }
            }
        })
        .detach();
}

/// Cancels the download of a plugin being installed.
pub fn cancel(id: &str, cx: &mut App) {
    if let Some(progress) = progress(id, cx) {
        progress.cancel();
    }
}

/// A step of a download, sent to the window's task.
enum Step {
    Progress,
    Done(Result<PathBuf, String>),
}

/// The install itself; `Ok(false)` — cancelled or declined.
async fn run_install(
    entry: &IndexEntry,
    progress: &InstallProgress,
    workspace: &WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
) -> Result<bool, String> {
    // The download, drawn as it goes.
    let (sender, mut steps) = unbounded::<Step>();
    {
        let entry = entry.clone();
        let progress = progress.clone();
        cx.background_spawn(async move {
            let mut last = Instant::now();
            let result = catalog::download(&entry, &progress.cancel, &mut |done, total| {
                progress.done.store(done, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
                if last.elapsed() >= Duration::from_millis(100) {
                    last = Instant::now();
                    let _ = sender.unbounded_send(Step::Progress);
                }
            });
            let _ = sender.unbounded_send(Step::Done(result));
        })
        .detach();
    }
    let mut archive = None;
    while let Some(step) = steps.next().await {
        match step {
            Step::Progress => {
                cx.update(|_, cx| cx.refresh_windows()).ok();
            }
            Step::Done(result) => {
                archive = Some(result);
                break;
            }
        }
    }
    let archive = match archive.unwrap_or_else(|| Err("The download stopped".into())) {
        Err(reason) if reason == catalog::CANCELLED => return Ok(false),
        other => other?,
    };
    progress.installing.store(true, Ordering::Relaxed);
    cx.update(|_, cx| cx.refresh_windows()).ok();
    let candidate = cx
        .background_spawn(async move { flux_plugin::install::inspect(&archive) })
        .await?;
    let manifest = &candidate.entry.manifest;
    if manifest.id != entry.id || manifest.version != entry.version {
        return Err(trf(
            "The package holds “{0}” {1}, not the plugin the catalog describes",
            &[&manifest.id, &manifest.version],
        ));
    }
    if let Some(problem) = &candidate.entry.problem {
        return Err(problem.clone());
    }
    // A new plugin is asked about; an update only when it asks for more than the installed one.
    let permissions = workspace
        .read_with(cx, |workspace, cx| {
            workspace
                .plugins
                .read(cx)
                .plugin(&entry.id)
                .map(|plugin| plugin.entry.manifest.permissions.clone())
        })
        .map_err(|_| tr("The window is closed").to_string())?;
    if permissions.as_ref() != Some(&candidate.entry.manifest.permissions)
        && plugin_manager::install_question(&candidate)
            .show_async(cx)
            .await
            != Some(INSTALL)
    {
        return Ok(false);
    }
    workspace
        .update(cx, |workspace, cx| {
            workspace
                .plugins
                .update(cx, |store, cx| store.install(candidate, cx))
        })
        .map_err(|_| tr("The window is closed").to_string())??;
    Ok(true)
}

// --- Icons of the index ---

/// The asset paths of the icons of the index, by plugin, with the hash of their SVG.
static ICONS: LazyLock<Mutex<HashMap<String, (u64, SharedString)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The plugin's icon from the index (its SVG, before it is installed), as an asset path. The SVG
/// is served from memory; a changed icon gets a new path (gpui keeps drawn SVGs by path).
pub fn index_icon(entry: &IndexEntry) -> Option<SharedString> {
    let svg = entry.icon_svg.as_ref()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    svg.hash(&mut hasher);
    let hash = hasher.finish();
    let mut known = ICONS.lock().unwrap();
    if let Some((known_hash, path)) = known.get(&entry.id)
        && *known_hash == hash
    {
        return Some(path.clone());
    }
    let key = format!("catalog:{}", entry.id);
    let file: &'static str = Box::leak(format!("icon-{hash:x}.svg").into_boxed_str());
    let bytes: &'static [u8] = Box::leak(svg.clone().into_bytes().into_boxed_slice());
    let files: &'static [(&'static str, &'static [u8])] =
        Box::leak(vec![(file, bytes)].into_boxed_slice());
    icons::register_plugin_files(&key, PluginFiles::Embedded(files));
    let path = icons::plugin_asset(&key, file);
    known.insert(entry.id.clone(), (hash, path.clone()));
    Some(path)
}

/// The plugin's icon of the index in `color`, or a puzzle piece.
pub fn entry_icon(entry: &IndexEntry, size: f32, color: Hsla) -> AnyElement {
    match index_icon(entry) {
        Some(path) => svg()
            .path(path)
            .flex_none()
            .size(px(size))
            .text_color(color)
            .into_any_element(),
        None => icon(IconName::Puzzle, color)
            .flex_none()
            .size(px(size))
            .into_any_element(),
    }
}

// --- The Marketplace ---

fn category_label(category: Option<Category>) -> &'static str {
    match category {
        None => tr("All"),
        Some(Category::Languages) => tr("Languages"),
        Some(Category::Themes) => tr("Themes"),
        Some(Category::Icons) => tr("Icons"),
        Some(Category::Tools) => tr("Tools"),
    }
}

/// The tabs of a plugin's details in the Marketplace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Permissions,
    Contributions,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Overview, Tab::Permissions, Tab::Contributions];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => tr("Overview"),
            Tab::Permissions => tr("Permissions"),
            Tab::Contributions => tr("Contributions"),
        }
    }
}

/// The Marketplace tab of Settings → Plugins.
pub struct PluginCatalog {
    plugins: Entity<PluginStore>,
    workspace: WeakEntity<Workspace>,
    search: Entity<TextInput>,
    category: Option<Category>,
    selected: Option<SharedString>,
    tab: Tab,
    /// A scenario's plugin to install once the catalog has it (`FLUX_SCENARIO_CATALOG_INSTALL`).
    #[cfg(feature = "scenario")]
    scenario_install: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for PluginCatalog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PluginCatalog {
    pub fn new(
        plugins: Entity<PluginStore>,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| TextInput::new(tr("Search the Marketplace"), cx).icon(IconName::Search));
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, _: &InputEvent, cx| {
                this.keep_selection_listed(cx);
                cx.notify()
            }),
            cx.observe(&plugins, |_, _, cx| cx.notify()),
            cx.observe_global::<Catalog>(|this, cx| {
                this.keep_selection_listed(cx);
                cx.notify()
            }),
        ];
        index(cx);
        ensure_fresh(cx);
        let mut catalog = Self {
            plugins,
            workspace,
            search,
            category: None,
            selected: None,
            tab: Tab::Overview,
            #[cfg(feature = "scenario")]
            scenario_install: std::env::var("FLUX_SCENARIO_CATALOG_INSTALL").ok(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        #[cfg(feature = "scenario")]
        if let Ok(id) = std::env::var("FLUX_SCENARIO_CATALOG_PLUGIN") {
            catalog.selected = Some(id.into());
        }
        #[cfg(feature = "scenario")]
        if let Ok(tab) = std::env::var("FLUX_SCENARIO_CATALOG_TAB") {
            catalog.tab = match tab.as_str() {
                "permissions" => Tab::Permissions,
                "contributions" => Tab::Contributions,
                _ => Tab::Overview,
            };
        }
        catalog.keep_selection_listed(cx);
        catalog
    }

    pub fn focus_search(&self, window: &mut Window, cx: &App) {
        window.focus(&self.search.focus_handle(cx));
    }

    /// The published plugins the list shows: the category's, those the search finds.
    fn listed(&self, cx: &App) -> Vec<IndexEntry> {
        let Some(index) = peek(cx) else {
            return Vec::new();
        };
        let query = self.search.read(cx).text().trim().to_lowercase();
        index
            .plugins
            .iter()
            .filter(|entry| {
                self.category
                    .is_none_or(|category| entry.categories.contains(&category))
            })
            .filter(|entry| query.is_empty() || matches(entry, &query))
            .cloned()
            .collect()
    }

    fn keep_selection_listed(&mut self, cx: &mut Context<Self>) {
        let listed = self.listed(cx);
        let kept = self
            .selected
            .as_ref()
            .is_some_and(|id| listed.iter().any(|entry| entry.id == id.as_ref()));
        if !kept {
            self.selected = listed.first().map(|entry| entry.id.clone().into());
        }
    }

    fn select(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            self.tab = Tab::Overview;
            cx.notify();
        }
    }

    pub fn select_next(&mut self, step: isize, cx: &mut Context<Self>) {
        let listed = self.listed(cx);
        if listed.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|id| listed.iter().position(|entry| entry.id == id.as_ref()));
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, listed.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(listed[next].id.clone().into(), cx);
    }

    fn set_category(&mut self, category: Option<Category>, cx: &mut Context<Self>) {
        self.category = category;
        self.keep_selection_listed(cx);
        cx.notify();
    }

    fn install(&mut self, entry: IndexEntry, window: &mut Window, cx: &mut Context<Self>) {
        install(entry, self.workspace.clone(), window, cx);
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        cx.default_global::<Catalog>().error = None;
        fetch(cx).detach();
    }
}

/// Whether the search finds a published plugin: its name, id, description (as shown or in
/// English), authors, languages.
fn matches(entry: &IndexEntry, query: &str) -> bool {
    let language = lang_code();
    let shown = format!(
        "{} {}",
        entry.translate(language, &entry.name),
        entry.translate(language, &entry.description)
    );
    if shown.to_lowercase().contains(query) {
        return true;
    }
    let languages = entry
        .languages
        .iter()
        .map(|language| language.name.clone())
        .collect::<Vec<_>>()
        .join(" ");
    [
        entry.name.as_str(),
        entry.id.as_str(),
        entry.description.as_str(),
        &entry.authors.join(" "),
        &languages,
    ]
    .iter()
    .any(|text| text.to_lowercase().contains(query))
}

/// A README as the plugin's page shows it, under its name and description: without a first
/// heading that is the name, and without a first paragraph that is the description.
fn readme_body(readme: &str, name: &str, description: &str) -> String {
    let mut rest = readme.trim_start();
    if let Some(heading) = rest.strip_prefix("# ") {
        let (title, after) = heading.split_once('\n').unwrap_or((heading, ""));
        if title.trim().eq_ignore_ascii_case(name.trim()) {
            rest = after.trim_start();
        }
    }
    let (first, after) = rest.split_once("\n\n").unwrap_or((rest, ""));
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !description.trim().is_empty() && flat(first) == flat(description) {
        rest = after.trim_start();
    }
    rest.to_string()
}

/// "1.4 MB", "820 KB".
fn size_label(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

impl PluginCatalog {
    fn render_categories(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let chips = std::iter::once(None)
            .chain(Category::ALL.into_iter().map(Some))
            .map(|category| {
                let active = self.category == category;
                div()
                    .id(SharedString::from(format!(
                        "catalog-category-{}",
                        category_label(category)
                    )))
                    .px_1p5()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .rounded(px(ui::RADIUS_SM))
                    .text_size(px(theme::TEXT_SM))
                    .cursor_pointer()
                    .when(active, |chip| {
                        chip.bg(ui.accent_soft).text_color(ui.accent_text)
                    })
                    .when(!active, |chip| {
                        chip.text_color(ui.text_muted)
                            .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_category(category, cx)
                    }))
                    .child(category_label(category))
            });
        div().flex().flex_wrap().gap_0p5().px_2().pb_2().children(chips)
    }

    fn render_list(&self, listed: &[IndexEntry], cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let any = peek(cx).is_some_and(|index| !index.plugins.is_empty());
        let body = if listed.is_empty() {
            let (title, hint) = if peek(cx).is_none() {
                if is_loading(cx) {
                    (tr("Loading the catalog…"), "")
                } else {
                    (
                        tr("The catalog isn't loaded"),
                        tr("Flux reads it from the catalog's repository on GitHub."),
                    )
                }
            } else if any {
                (
                    tr("Nothing found"),
                    tr("Search looks in names, descriptions, authors and languages."),
                )
            } else {
                (tr("The catalog is empty"), "")
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .text_center()
                .child(icon(IconName::Puzzle, ui.dim).size(px(24.)))
                .child(div().text_color(ui.text_muted).child(title))
                .when(!hint.is_empty(), |column| {
                    column.child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(hint),
                    )
                })
                .into_any_element()
        } else {
            let offers: Vec<Offer> = {
                let store = self.plugins.read(cx);
                listed.iter().map(|entry| offer(entry, store)).collect()
            };
            let rows: Vec<AnyElement> = listed
                .iter()
                .zip(offers)
                .map(|(entry, offer)| self.render_row(entry, offer, cx).into_any_element())
                .collect();
            div()
                .id("catalog-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_1()
                .pb_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .children(rows)
                .into_any_element()
        };
        div()
            .flex_none()
            .w(px(LIST_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(div().flex_none().p_2().child(self.search.clone()))
            .child(self.render_categories(cx))
            .child(body)
    }

    /// A plugin in the list: its icon, name and author, what it is, and what the catalog offers.
    fn render_row(
        &self,
        entry: &IndexEntry,
        offer: Offer,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let ui = Theme::ui(cx);
        let selected = self
            .selected
            .as_ref()
            .is_some_and(|id| id.as_ref() == entry.id);
        let id = SharedString::from(entry.id.clone());
        let installing = progress(&entry.id, cx);
        let language = lang_code();
        let (status, status_color): (String, Hsla) = match (&installing, &offer) {
            (Some(progress), _) => (progress.label(), ui.accent_text),
            (None, Offer::Install) => (
                entry.translate(language, &entry.description).to_string(),
                ui.dim,
            ),
            (None, Offer::Update { .. }) => (
                trf("Update available: {0}", &[&entry.version]),
                ui.accent_text,
            ),
            (None, Offer::Installed { version }) => {
                (trf("Installed: {0}", &[version]), ui.success)
            }
            (None, Offer::Development) => (tr("Under development").to_string(), ui.amber),
            (None, Offer::Incompatible) => (tr("Needs a newer Flux").to_string(), ui.warning),
        };
        let trailing = match (&installing, &offer) {
            (None, Offer::Install) => Some(
                ui::text_button(
                    SharedString::from(format!("catalog-install-{}", entry.id)),
                    tr("Install"),
                    false,
                    ui,
                )
                .on_click(cx.listener({
                    let entry = entry.clone();
                    move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.install(entry.clone(), window, cx)
                    }
                }))
                .into_any_element(),
            ),
            (None, Offer::Update { .. }) => Some(
                ui::text_button(
                    SharedString::from(format!("catalog-update-{}", entry.id)),
                    tr("Update"),
                    false,
                    ui,
                )
                .on_click(cx.listener({
                    let entry = entry.clone();
                    move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.install(entry.clone(), window, cx)
                    }
                }))
                .into_any_element(),
            ),
            _ => None,
        };
        let author = entry.authors.first().cloned().unwrap_or_default();
        div()
            .id(SharedString::from(format!("catalog-row-{}", entry.id)))
            .flex()
            .items_center()
            .gap_2p5()
            .px_2()
            .py_1p5()
            .rounded(px(ui::RADIUS_SM))
            .cursor_pointer()
            .when(selected, |row| row.bg(ui.list_selected))
            .when(!selected, |row| row.hover(move |style| style.bg(ui.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.select(id.clone(), cx)))
            .child(entry_icon(entry, LIST_ICON, ui.text_muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .child(entry.translate(language, &entry.name).to_string()),
                            )
                            .when(!author.is_empty(), |line| {
                                line.child(
                                    div()
                                        .flex_none()
                                        .max_w(px(90.))
                                        .truncate()
                                        .text_size(px(theme::TEXT_XS))
                                        .text_color(ui.dim)
                                        .child(author),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(status_color)
                            .truncate()
                            .child(status),
                    ),
            )
            .children(trailing)
    }

    /// The selected plugin: who it is, Install / Update, the tabs.
    fn render_details(&self, entry: &IndexEntry, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let store = self.plugins.read(cx);
        let offer = offer(entry, store);
        let manifest = entry.parse_manifest().ok();
        let mut meta: Vec<AnyElement> = Vec::new();
        if !entry.authors.is_empty() {
            meta.push(
                div()
                    .min_w_0()
                    .truncate()
                    .child(entry.authors.join(", "))
                    .into_any_element(),
            );
        }
        if let Some(repository) = entry.repository.clone() {
            let shown = repository
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string();
            meta.push(
                div()
                    .id("catalog-repository")
                    .min_w_0()
                    .truncate()
                    .text_color(ui.accent_text)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(move |_, _, cx| cx.open_url(&repository))
                    .child(shown)
                    .into_any_element(),
            );
        }
        let badges = entry
            .categories
            .iter()
            .map(|category| ui::badge(category_label(Some(*category)), ui.text_muted));
        let identity = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .size(px(DETAILS_ICON + 14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RADIUS_MD))
                    .bg(ui.input_background)
                    .border_1()
                    .border_color(ui.island_border)
                    .child(entry_icon(entry, DETAILS_ICON - 8., ui.accent_text)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::TEXT_LG))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(entry.translate(lang_code(), &entry.name).to_string()),
                            )
                            .child(ui::badge(entry.version.clone(), ui.text_muted))
                            .children(badges),
                    )
                    .when(!meta.is_empty(), |column| {
                        column.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(ui.text_muted)
                                .children(plugin_manager::dot_separated(meta, ui)),
                        )
                    }),
            );
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(identity)
                    .child(self.render_buttons(entry, &offer, cx)),
            )
            .child(self.render_tabs(cx))
            .child(ui::divider(ui))
            .child(
                div()
                    .id("catalog-tab-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(match self.tab {
                        Tab::Overview => self.render_overview(entry, cx),
                        Tab::Permissions => match &manifest {
                            Some(manifest) => plugin_manager::permissions_view(manifest, ui),
                            None => broken_manifest(ui),
                        },
                        Tab::Contributions => match &manifest {
                            Some(manifest) => {
                                let names = plugin_manager::ContributionNames {
                                    themes: entry
                                        .themes
                                        .iter()
                                        .map(|theme| (theme.name.clone(), theme.appearance.clone()))
                                        .collect(),
                                    icon_themes: entry.icon_themes.clone(),
                                    problems: Vec::new(),
                                };
                                plugin_manager::contributions_list(
                                    plugin_manager::contribution_sections(
                                        manifest,
                                        &|text| entry.translate(lang_code(), text).to_string(),
                                        names,
                                        ui,
                                    ),
                                    ui,
                                )
                            }
                            None => broken_manifest(ui),
                        },
                    }),
            )
    }

    fn render_buttons(&self, entry: &IndexEntry, offer: &Offer, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let mut items: Vec<AnyElement> = Vec::new();
        if let Some(progress) = progress(&entry.id, cx) {
            items.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(140.))
                            .child(crate::notifications::progress_bar(
                                match progress.fraction() {
                                    Some(fraction) if !progress.installing() => {
                                        crate::notifications::Progress::Fraction(fraction)
                                    }
                                    _ => crate::notifications::Progress::Indeterminate,
                                },
                                ui,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.text_muted)
                            .child(progress.label()),
                    )
                    .into_any_element(),
            );
            if !progress.installing() {
                let id = entry.id.clone();
                items.push(
                    ui::text_button("catalog-cancel", tr("Cancel"), false, ui)
                        .on_click(move |_, _, cx| cancel(&id, cx))
                        .into_any_element(),
                );
            }
        } else {
            match offer {
                Offer::Install => {
                    let entry = entry.clone();
                    items.push(
                        ui::primary_button("catalog-install", tr("Install"), true, ui)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.install(entry.clone(), window, cx)
                            }))
                            .into_any_element(),
                    );
                }
                Offer::Update { from } => {
                    let label = trf("Update to {0}", &[&entry.version]);
                    let entry = entry.clone();
                    items.push(
                        ui::primary_button("catalog-update", label, true, ui)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.install(entry.clone(), window, cx)
                            }))
                            .into_any_element(),
                    );
                    items.push(note(trf("Installed: {0}", &[from]), ui.dim));
                }
                Offer::Installed { version } => {
                    items.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.success)
                            .child(icon(IconName::CheckCircle, ui.success).size(px(14.)))
                            .child(trf("Installed: {0}", &[version]))
                            .into_any_element(),
                    );
                }
                Offer::Development => items.push(note(
                    tr("A version under development is linked: the catalog doesn't replace it")
                        .to_string(),
                    ui.amber,
                )),
                Offer::Incompatible => items.push(note(
                    trf(
                        "Built for plugin API {0}; this Flux has {1}",
                        &[&entry.api, &flux_plugin::API_VERSION],
                    ),
                    ui.warning,
                )),
            }
        }
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(items)
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let tabs = Tab::ALL.into_iter().map(|tab| {
            let active = self.tab == tab;
            let group = SharedString::from(format!("catalog-tab-{tab:?}"));
            div()
                .id(group.clone())
                .group(group.clone())
                .relative()
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .child(
                    ui::section_label(tab.label(), ui)
                        .when(active, |label| label.text_color(ui.foreground))
                        .when(!active, |label| {
                            label.group_hover(group.clone(), move |style| {
                                style.text_color(ui.text_muted)
                            })
                        }),
                )
                .when(active, |tab| {
                    tab.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(6.))
                            .h(px(2.))
                            .rounded(px(1.))
                            .bg(ui.accent),
                    )
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.tab = tab;
                    cx.notify()
                }))
        });
        div()
            .flex_none()
            .h(px(TABS_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_4()
            .children(tabs)
    }

    /// The description and the README, then the facts.
    fn render_overview(&self, entry: &IndexEntry, cx: &mut Context<Self>) -> Div {
        let ui = Theme::ui(cx);
        let theme = Theme::get(cx);
        let mut column = div().flex().flex_col().gap_4();
        if !entry.description.trim().is_empty() {
            column = column.child(
                div()
                    .text_color(ui.foreground)
                    .child(entry.translate(lang_code(), &entry.description).to_string()),
            );
        }
        if let Some(readme) = entry
            .readme
            .as_deref()
            .map(|readme| readme_body(readme, &entry.name, &entry.description))
            .filter(|readme| !readme.trim().is_empty())
        {
            column = column.child(crate::markdown::render(
                &crate::markdown::parse(&readme),
                ui.foreground,
                theme,
            ));
        }
        let mut facts: Vec<(&'static str, AnyElement)> = vec![
            (
                tr("Identifier"),
                plugin_manager::code_text(entry.id.clone(), ui),
            ),
            (
                tr("Plugin API"),
                plugin_manager::code_text(entry.api.clone(), ui),
            ),
            (
                tr("Download"),
                div()
                    .text_color(ui.text_muted)
                    .child(size_label(entry.size))
                    .into_any_element(),
            ),
        ];
        if let Some(updated) = &entry.updated {
            facts.push((
                tr("Updated"),
                div()
                    .text_color(ui.text_muted)
                    .child(updated.clone())
                    .into_any_element(),
            ));
        }
        if !entry.languages.is_empty() {
            let languages = entry
                .languages
                .iter()
                .map(|language| language.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            facts.push((
                tr("Languages"),
                div()
                    .text_color(ui.text_muted)
                    .child(languages)
                    .into_any_element(),
            ));
        }
        column
            .child(ui::divider(ui))
            .child(plugin_manager::facts_view(facts, ui))
    }
}

/// A line of quiet text among the buttons.
fn note(text: String, color: Hsla) -> AnyElement {
    div()
        .text_size(px(theme::TEXT_SM))
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn broken_manifest(ui: UiColors) -> Div {
    div()
        .text_color(ui.warning)
        .child(tr("The catalog has a manifest Flux can't read for this plugin"))
}

impl Render for PluginCatalog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A scenario can't click Install: it names the plugin in the environment.
        #[cfg(feature = "scenario")]
        if let Some(entry) = self.scenario_install.as_ref().and_then(|id| {
            peek(cx).and_then(|index| index.plugin(id).cloned())
        }) {
            self.scenario_install = None;
            cx.defer_in(window, move |this, window, cx| this.install(entry, window, cx));
        }
        #[cfg(not(feature = "scenario"))]
        let _ = window;
        let ui = Theme::ui(cx);
        let listed = self.listed(cx);
        let selected = self
            .selected
            .as_ref()
            .and_then(|id| listed.iter().find(|entry| entry.id == id.as_ref()))
            .cloned();
        let details = match (selected, error(cx)) {
            (Some(entry), _) => self.render_details(&entry, cx).into_any_element(),
            (None, Some(error)) if peek(cx).is_none() => div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .px_6()
                .text_center()
                .rounded(px(ui::RADIUS_MD))
                .border_1()
                .border_color(ui.island_border)
                .child(icon(IconName::Warning, ui.warning).size(px(22.)))
                .child(
                    div()
                        .text_color(ui.foreground)
                        .child(tr("Couldn't load the catalog")),
                )
                .child(
                    div()
                        .max_w(px(420.))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(error),
                )
                .child(
                    ui::primary_button("catalog-retry", tr("Retry"), true, ui).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.retry(cx)),
                    ),
                )
                .into_any_element(),
            (None, _) => div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_MD))
                .border_1()
                .border_color(ui.island_border)
                .text_color(ui.dim)
                .child(if is_loading(cx) {
                    tr("Loading the catalog…")
                } else {
                    tr("Select a plugin to see its details")
                })
                .into_any_element(),
        };
        div()
            .track_focus(&self.focus_handle)
            .flex_1()
            .min_h_0()
            .px_4()
            .pb_4()
            .flex()
            .gap_3()
            .child(self.render_list(&listed, cx))
            .child(details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readme_skips_the_name_and_the_description_shown_above_it() {
        let readme = "# YAML\n\nYAML: highlighting and\nthe server.\n\n## Files\n\n- `*.yaml`\n";
        assert_eq!(
            readme_body(readme, "YAML", "YAML: highlighting and the server."),
            "## Files\n\n- `*.yaml`\n"
        );
        assert_eq!(
            readme_body("# Other\n\nText.", "YAML", "YAML."),
            "# Other\n\nText."
        );
        assert_eq!(readme_body("Just text.", "YAML", ""), "Just text.");
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(820 * 1024), "820 KB");
        assert_eq!(size_label(1_468_006), "1.4 MB");
    }

    #[test]
    fn search_looks_in_names_ids_descriptions_authors_and_languages() {
        let entry = IndexEntry {
            id: "flux.rust".into(),
            name: "Rust".into(),
            version: "0.1.0".into(),
            api: flux_plugin::API_VERSION.into(),
            description: "Highlighting and rust-analyzer.".into(),
            authors: vec!["Egor Ageev".into()],
            repository: None,
            categories: vec![Category::Languages],
            manifest: String::new(),
            download: String::new(),
            sha256: String::new(),
            size: 0,
            icon_svg: None,
            readme: None,
            languages: vec![catalog::IndexLanguage {
                id: "rust".into(),
                name: "Rust".into(),
                extensions: vec!["rs".into()],
                file_names: Vec::new(),
            }],
            themes: Vec::new(),
            icon_themes: Vec::new(),
            updated: None,
            locales: Default::default(),
        };
        for query in ["rust", "analyzer", "egor", "flux.rust"] {
            assert!(matches(&entry, query), "{query}");
        }
        assert!(!matches(&entry, "python"));
    }

    #[test]
    fn icons_of_the_index_are_served_from_memory() {
        let mut entry = IndexEntry {
            id: "test.icon".into(),
            name: "Icon".into(),
            version: "1.0.0".into(),
            api: flux_plugin::API_VERSION.into(),
            description: String::new(),
            authors: Vec::new(),
            repository: None,
            categories: Vec::new(),
            manifest: String::new(),
            download: String::new(),
            sha256: String::new(),
            size: 0,
            icon_svg: Some("<svg viewBox=\"0 0 16 16\"/>".into()),
            readme: None,
            languages: Vec::new(),
            themes: Vec::new(),
            icon_themes: Vec::new(),
            updated: None,
            locales: Default::default(),
        };
        use gpui::AssetSource;
        let path = index_icon(&entry).unwrap();
        assert_eq!(index_icon(&entry).unwrap(), path, "the same SVG keeps its path");
        let served = icons::Assets.load(&path).unwrap().unwrap();
        assert_eq!(&*served, b"<svg viewBox=\"0 0 16 16\"/>");
        entry.icon_svg = Some("<svg viewBox=\"0 0 16 16\"><path/></svg>".into());
        assert_ne!(index_icon(&entry).unwrap(), path, "a changed icon gets a new path");
        entry.icon_svg = None;
        assert!(index_icon(&entry).is_none());
    }
}
