//! The project popup (part 10.4): a click on the project in the title bar shows the recent projects
//! and Open… under it, as the project widget of JetBrains IDEs, instead of the system's folder panel.
//!
//! The rows: the current project (marked; it stays where it is), then up to [`MAX_OTHERS`] other
//! recent projects, most recent first — a tile with the project's initials in a color of its own,
//! the name, the path and the git branch; a project open in another Flux window is marked. ↵ or a
//! click opens a project as the start screen does ([`Workspace::open_project`]: the window that has
//! it comes forward, otherwise this window or a new one, as Settings → System says); ⌥↵ or ⌥-click
//! opens it in a new window; × or ⌫ forgets it ("Remove from Recent Projects" of JetBrains IDEs).
//! «Open…» below is ⌘O: the system's folder panel. The palette opens the popup too
//! (`workspace::RecentProjects`, no shortcut — JetBrains' macOS keymap has none).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, DismissEvent, Div, EventEmitter, FocusHandle,
    Focusable, FontWeight, Hsla, KeyBinding, MouseButton, Pixels, Render, Stateful, WeakEntity,
    Window, WindowId, actions, div, point, prelude::*, px,
};

use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::notifications::Notification;
use crate::start_screen::{display_name, shorten_path};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD, RADIUS_SM};
use crate::workspace::{self, Workspace, tilde};

actions!(
    project_popup,
    [
        SelectNext,
        SelectPrevious,
        Confirm,
        /// ⌥↵: the selected project in a new window.
        ConfirmInNewWindow,
        /// ⌫: the selected project leaves the recent projects.
        Remove,
        Dismiss,
    ]
);

// In the window's namespace: the palette shows it as "Workspace: Recent Projects".
actions!(workspace, [RecentProjects]);

/// How many recent projects besides the current one the popup offers.
pub const MAX_OTHERS: usize = 5;
const WIDTH: f32 = 400.;
/// The rows are inset from the popup's edges (as the context menu's items); a row's radius is the
/// popup's minus the inset.
const INSET: f32 = 6.;
/// A project: the tile, the name over the path.
const ROW_HEIGHT: f32 = 46.;
/// «Open…», as a context menu item.
const ACTION_HEIGHT: f32 = 30.;
const TILE_SIZE: f32 = 28.;
const FOOTER_HEIGHT: f32 = 34.;
/// Longer paths are shortened in the middle (the end of a path tells more); the branch after the
/// path takes its share of the line (gpui doesn't draw "…" where a flexible text is clipped).
const PATH_CHARS: usize = 44;
const MIN_PATH_CHARS: usize = 16;
/// Where the popup goes under the chip.
const CHIP_GAP: f32 = 4.;

thread_local! {
    /// The project chip of each window, as last drawn: the palette's Recent Projects opens the
    /// popup under it too.
    static CHIPS: RefCell<HashMap<WindowId, Bounds<Pixels>>> = RefCell::new(HashMap::new());
}

pub fn init(cx: &mut App) {
    let popup = Some("ProjectPopup");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, popup),
        KeyBinding::new("up", SelectPrevious, popup),
        KeyBinding::new("ctrl-n", SelectNext, popup),
        KeyBinding::new("ctrl-p", SelectPrevious, popup),
        KeyBinding::new("enter", Confirm, popup),
        KeyBinding::new("alt-enter", ConfirmInNewWindow, popup),
        KeyBinding::new("backspace", Remove, popup),
        KeyBinding::new("delete", Remove, popup),
        KeyBinding::new("escape", Dismiss, popup),
    ]);
}

/// The popup's window-level action: Recent Projects from the palette.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, _: &RecentProjects, window, cx| open(this, window, cx)))
}

/// Opens the popup under the project chip (or closes it, when open).
pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let projects = projects_of(workspace, window, cx);
    let this = cx.weak_entity();
    let anchor = CHIPS
        .with_borrow(|chips| chips.get(&window.window_handle().window_id()).copied())
        .map(|chip| point(chip.left(), chip.bottom() + px(CHIP_GAP)))
        .unwrap_or_else(|| crate::popup::window_middle(window, WIDTH));
    workspace.toggle_modal(window, cx, move |_, cx| {
        ProjectPopup::new(this, projects, cx)
    });
    // A second click closed the popup: then there is nothing to place.
    workspace.anchor_modal(anchor);
}

/// The project in the title bar: its folder, its name and ▾; a press opens the popup under it (as
/// a macOS pop-up button), a press while it is open closes it. A window without a project shows
/// «Open Folder…» in its place, opening the same popup.
pub fn project_chip(workspace: &Workspace, cx: &Context<Workspace>) -> AnyElement {
    let ui = Theme::ui(cx);
    let (glyph, label, tooltip, named) = match workspace.root() {
        Some(root) => (IconName::Folder, display_name(root), tilde(root), true),
        None => (
            IconName::FolderPlus,
            tr("Open Folder…").to_string(),
            tr("Recent Projects").to_string(),
            false,
        ),
    };
    let was_open = Rc::new(Cell::new(false));
    let this = cx.weak_entity();
    let chip = div()
        .id("title-project")
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(26.))
        .pl_2()
        .pr_1p5()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .when(named, |chip| chip.hover(move |style| style.bg(ui.hover)))
        .when(!named, |chip| {
            chip.text_color(ui.text_muted)
                .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
        })
        .active(crate::motion::press)
        .tooltip(ui::tooltip(tooltip, None))
        // A press while the popup is open only closes it (the popup's own "click outside"): the
        // capture phase sees the popup before it goes.
        .capture_any_mouse_down({
            let (was_open, this) = (was_open.clone(), this.clone());
            move |_, _, cx| {
                let open = this
                    .upgrade()
                    .is_some_and(|workspace| workspace.read(cx).modal_is::<ProjectPopup>());
                was_open.set(open);
            }
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            if was_open.get() {
                return;
            }
            this.update(cx, |workspace, cx| open(workspace, window, cx))
                .ok();
        })
        .child(icon(glyph, ui.folder).size(px(14.)))
        .child(
            div()
                .when(named, |name| {
                    name.font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui.foreground)
                })
                .child(label),
        )
        .child(icon(IconName::ChevronDown, ui.dim).size(px(10.)));
    div()
        // The chip's place on screen, for the popup to open right under it.
        .on_children_prepainted(|children, window, _| {
            if let Some(bounds) = children.first() {
                let id = window.window_handle().window_id();
                CHIPS.with_borrow_mut(|chips| chips.insert(id, *bounds));
            }
        })
        .child(chip)
        .into_any_element()
}

// --- The list ---

/// A project of the popup, before it is looked at on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    path: PathBuf,
    /// The window's own project.
    current: bool,
    /// Another Flux window has it: opening it brings that window forward.
    open_elsewhere: bool,
}

/// The projects the popup shows: the current one first (even when the recent list doesn't have
/// it — a scenario without the list's file), then up to [`MAX_OTHERS`] other recent projects
/// whose folders `exist`, most recent first.
fn rows(
    recent: &[PathBuf],
    current: Option<&Path>,
    open_elsewhere: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Vec<Row> {
    let current_row = current.map(|path| Row {
        path: path.to_path_buf(),
        current: true,
        open_elsewhere: false,
    });
    let others = recent
        .iter()
        .filter(|path| Some(path.as_path()) != current && exists(path))
        .take(MAX_OTHERS)
        .map(|path| Row {
            path: path.clone(),
            current: false,
            open_elsewhere: open_elsewhere.contains(path),
        });
    current_row.into_iter().chain(others).collect()
}

/// A project as the popup draws it.
#[derive(Debug, Clone)]
struct Project {
    path: PathBuf,
    name: String,
    initials: String,
    hue: Hsla,
    branch: Option<String>,
    current: bool,
    open_elsewhere: bool,
}

impl Project {
    fn new(row: Row, ui: &UiColors) -> Self {
        let name = display_name(&row.path);
        Self {
            initials: initials(&name),
            hue: hue(&row.path, ui),
            branch: head_branch(&row.path),
            name,
            path: row.path,
            current: row.current,
            open_elsewhere: row.open_elsewhere,
        }
    }
}

/// The popup's projects for the window `workspace` is in: the recent list as the file has it now.
fn projects_of(workspace: &mut Workspace, window: &Window, cx: &mut App) -> Vec<Project> {
    let ui = Theme::ui(cx);
    let current = workspace.root().map(Path::to_path_buf);
    let elsewhere = open_roots(window, cx);
    let recent = workspace.recent_projects();
    rows(recent, current.as_deref(), &elsewhere, Path::is_dir)
        .into_iter()
        .map(|row| Project::new(row, &ui))
        .collect()
}

/// The projects of the other Flux windows.
fn open_roots(window: &Window, cx: &App) -> Vec<PathBuf> {
    let this = window.window_handle().window_id();
    cx.windows()
        .into_iter()
        .filter(|handle| handle.window_id() != this)
        .filter_map(|handle| handle.downcast::<Workspace>())
        .filter_map(|handle| Some(handle.read(cx).ok()?.root()?.to_path_buf()))
        .collect()
}

/// A project's tile letters, as JetBrains IDEs draw them: the first letters of its first two words
/// ("flux-plugins" → "FP", "MyApp" → "MA"), or of its only word ("flux" → "F").
fn initials(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for part in name.split(|c: char| !c.is_alphanumeric()) {
        // camelCase: a capital after a small letter starts a word, and so does the last capital of
        // an acronym before a small letter ("XMLParser" → "XML", "Parser").
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let previous = i.checked_sub(1).map(|p| chars[p]);
            let next = chars.get(i + 1).copied();
            let boundary = c.is_uppercase()
                && previous.is_some_and(|p| {
                    p.is_lowercase() || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
                });
            if boundary && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c);
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    let letters: String = words
        .iter()
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        name.chars()
            .next()
            .map_or("?".to_string(), |c| c.to_string())
    } else {
        letters
    }
}

/// A project's own color, the same in every window and every launch: a shade of the palette chosen
/// by its path (red is left out — it reads as an error).
fn hue(path: &Path, ui: &UiColors) -> Hsla {
    let shades = [
        ui.blue, ui.indigo, ui.violet, ui.pink, ui.orange, ui.amber, ui.lime, ui.green, ui.teal,
        ui.cyan,
    ];
    shades[shade_index(path, shades.len())]
}

/// FNV-1a of the path: stable, unlike the standard hasher's seed.
fn shade_index(path: &Path, count: usize) -> usize {
    let hash = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    (hash % count as u64) as usize
}

/// The branch a project's repository is on, from its `.git/HEAD` (a worktree's `.git` file points
/// to its git directory); a detached HEAD gives the short commit. Only a repository at the project's
/// root counts: reading it is cheap enough to do as the popup opens.
pub(crate) fn head_branch(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_file() {
        let text = fs::read_to_string(&dot_git).ok()?;
        let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        if dir.is_absolute() {
            dir
        } else {
            root.join(dir)
        }
    } else {
        dot_git
    };
    parse_head(&fs::read_to_string(git_dir.join("HEAD")).ok()?)
}

/// `ref: refs/heads/main` → "main"; a commit → its first 7 characters.
fn parse_head(text: &str) -> Option<String> {
    let head = text.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!branch.is_empty()).then(|| branch.to_string());
    }
    (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_string())
}

// --- The popup ---

pub struct ProjectPopup {
    workspace: WeakEntity<Workspace>,
    projects: Vec<Project>,
    /// The highlighted row: a project, or «Open…» after them (`projects.len()`). The pointer
    /// moves it too, as in JetBrains' popups.
    selected: usize,
    focus_handle: FocusHandle,
}

impl ProjectPopup {
    fn new(workspace: WeakEntity<Workspace>, projects: Vec<Project>, cx: &mut App) -> Self {
        // The first project to switch to; without one, «Open…».
        let selected = projects
            .iter()
            .position(|project| !project.current)
            .unwrap_or(projects.len());
        Self {
            workspace,
            projects,
            selected,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Projects and «Open…».
    fn row_count(&self) -> usize {
        self.projects.len() + 1
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.row_count();
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected != index {
            self.selected = index;
            cx.notify();
        }
    }

    /// Opens the row: a project (in this window or a new one, as the settings say; in a new one
    /// with ⌥), «Open…». The current project only closes the popup.
    fn confirm(
        &mut self,
        index: usize,
        new_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let project = self.projects.get(index).cloned();
        cx.emit(DismissEvent);
        match project {
            Some(project) if project.current => {}
            // After the popup is gone and focus is back where it was: the question "This Window
            // or New Window" and the new window come over a settled window.
            Some(project) => window.defer(cx, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| match new_window {
                        true => open_in_new_window(workspace, project.path, window, cx),
                        false => workspace.open_project(project.path, Vec::new(), window, cx),
                    })
                    .ok();
            }),
            None => window.defer(cx, |window, cx| {
                window.dispatch_action(Box::new(workspace::Open), cx)
            }),
        }
    }

    /// Forgets a project: it leaves the recent list (and the start screen), the next one moves up.
    /// The current project stays.
    fn remove(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.get(index).filter(|project| !project.current) else {
            return;
        };
        let path = project.path.clone();
        let projects = self
            .workspace
            .update(cx, |workspace, cx| {
                workspace.forget_recent(&path, cx);
                projects_of(workspace, window, cx)
            })
            .ok();
        if let Some(projects) = projects {
            self.projects = projects;
        } else {
            self.projects.remove(index);
        }
        // The row below takes the place; past the last project — the one above, else «Open…».
        let last_project = self.projects.len().checked_sub(1);
        self.selected = match last_project {
            Some(last) if index > last => last,
            Some(_) => index,
            None => 0,
        };
        if self
            .projects
            .get(self.selected)
            .is_some_and(|project| project.current)
        {
            self.selected = (self.selected + 1).min(self.projects.len());
        }
        cx.notify();
    }

    fn render_project(
        &self,
        index: usize,
        project: &Project,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let selected = self.selected == index;
        let badge = if project.current {
            Some(ui::badge(tr("current"), ui.accent_text))
        } else if project.open_elsewhere {
            Some(ui::badge(tr("in another window"), ui.success))
        } else {
            None
        };
        let remove = (!project.current && selected).then(|| {
            ui::icon_button(("project-popup-remove", index), IconName::Close, ui)
                .flex_none()
                .tooltip(ui::tooltip(
                    tr("Remove from Recent Projects"),
                    Some("⌫".into()),
                ))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.remove(index, window, cx)
                }))
        });
        let path = shorten_path(
            &tilde(&project.path),
            path_budget(project.branch.as_deref()),
        );
        div()
            .id(("project-popup-project", index))
            .h(px(ROW_HEIGHT))
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .rounded(px(RADIUS_MD))
            .cursor_pointer()
            .when(selected, |row| row.bg(ui.list_selected))
            // The whole path, when the row had to shorten it.
            .tooltip(ui::tooltip(tilde(&project.path), None))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.select(index, cx)
                }
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.confirm(index, event.modifiers().alt, window, cx)
            }))
            .child(tile(&project.initials, project.hue))
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
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(ui.foreground)
                                    .child(project.name.clone()),
                            )
                            .children(badge),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(theme::TEXT_SM))
                            .child(div().min_w_0().truncate().text_color(ui.dim).child(path))
                            .children(
                                project
                                    .branch
                                    .as_deref()
                                    .map(|branch| branch_label(branch, ui)),
                            ),
                    ),
            )
            .children(remove)
    }

    /// «Open…» with ⌘O: the system's folder panel.
    fn render_open(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let ui = Theme::ui(cx);
        let index = self.projects.len();
        let selected = self.selected == index;
        let keys = ui::shortcut_for(&workspace::Open, cx);
        div()
            .id("project-popup-open")
            .h(px(ACTION_HEIGHT))
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .rounded(px(RADIUS_SM))
            .cursor_pointer()
            .when(selected, |row| row.bg(ui.list_selected))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.select(index, cx)
                }
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.confirm(index, false, window, cx)
            }))
            // Level with the tiles' middle: the icon sits where a tile's letters would.
            .child(
                div()
                    .flex_none()
                    .w(px(TILE_SIZE))
                    .flex()
                    .justify_center()
                    .child(icon(IconName::FolderOpen, ui.text_muted).size(px(15.))),
            )
            .child(div().flex_1().text_color(ui.foreground).child(tr("Open…")))
            .children(keys.map(|keys| ui::keys(&keys, ui)))
    }

    fn render_empty(&self, ui: UiColors) -> Div {
        div()
            .h(px(ROW_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .text_color(ui.dim)
            .child(
                div()
                    .flex_none()
                    .w(px(TILE_SIZE))
                    .flex()
                    .justify_center()
                    .child(icon(IconName::Clock, ui.dim).size(px(15.))),
            )
            .child(tr("Projects you open will appear here"))
    }
}

/// How many characters the path gets on its line, next to the branch.
fn path_budget(branch: Option<&str>) -> usize {
    match branch {
        // The branch's glyph and the gap are about four characters wide.
        Some(branch) => PATH_CHARS
            .saturating_sub(branch.chars().count() + 4)
            .max(MIN_PATH_CHARS),
        None => PATH_CHARS,
    }
}

/// A project's tile, as the popup and the start screen's recent projects draw it (JetBrains IDEs
/// show the same tile in their project widget and on their welcome screen).
pub fn project_tile(path: &Path, ui: &UiColors) -> Div {
    tile(&initials(&display_name(path)), hue(path, ui))
}

/// A colored tile with the project's letters: the backing and the letters are the same shade (as
/// the start screen's tiles).
fn tile(initials: &str, hue: Hsla) -> Div {
    div()
        .flex_none()
        .size(px(TILE_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .bg(UiColors::tint(hue, 0.18))
        .text_size(px(theme::TEXT_SM))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hue)
        .child(initials.to_string())
}

/// The branch after the path: the branch glyph and its name, in the branches' violet.
pub(crate) fn branch_label(branch: &str, ui: UiColors) -> Div {
    div()
        .flex_none()
        .max_w(px(140.))
        .flex()
        .items_center()
        .gap_1()
        .text_color(ui.violet)
        .child(icon(IconName::Branch, ui.violet).size(px(11.)).flex_none())
        .child(div().min_w_0().truncate().child(branch.to_string()))
}

/// ⌥↵ / ⌥-click: the project in a new window, whatever the settings say. A window that has it
/// comes forward instead, as with any opening.
fn open_in_new_window(
    workspace: &mut Workspace,
    path: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let root = fs::canonicalize(&path).unwrap_or(path);
    if !root.is_dir() {
        // A folder from the recent list may have disappeared since.
        let message = trf("Folder not found: {0}", &[&tilde(&root)]);
        return workspace.notify(Notification::warning(message), cx);
    }
    if workspace.root() == Some(root.as_path()) {
        return;
    }
    let this = window.window_handle().window_id();
    let other = cx
        .windows()
        .into_iter()
        .filter(|handle| handle.window_id() != this)
        .filter_map(|handle| handle.downcast::<Workspace>())
        .find(|handle| {
            handle
                .read(cx)
                .is_ok_and(|other| other.root() == Some(root.as_path()))
        });
    if let Some(handle) = other {
        handle
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        #[cfg(feature = "scenario")]
        crate::scenario::target(handle.into());
        return;
    }
    workspace::open_project_window(root, Vec::new(), Some(window.bounds()), cx);
}

impl EventEmitter<DismissEvent> for ProjectPopup {}

impl Focusable for ProjectPopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProjectPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let projects: Vec<AnyElement> = self
            .projects
            .clone()
            .iter()
            .enumerate()
            .map(|(index, project)| self.render_project(index, project, cx).into_any_element())
            .collect();
        let others = self.projects.iter().any(|project| !project.current);
        let hints: &[(&str, &str)] = if others {
            &[
                ("↵", tr("open")),
                ("⌥↵", tr("new window")),
                ("⌫", tr("remove")),
            ]
        } else {
            &[("↵", tr("open")), ("esc", tr("close"))]
        };
        crate::popup::panel(ui)
            .id("project-popup")
            .key_context("ProjectPopup")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.confirm(this.selected, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ConfirmInNewWindow, window, cx| {
                this.confirm(this.selected, true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &Remove, window, cx| this.remove(this.selected, window, cx)),
            )
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .px(px(INSET + 8.))
                    .pt_3()
                    .pb_1p5()
                    .child(ui::section_label(tr("Recent Projects"), ui)),
            )
            .child(
                div()
                    .px(px(INSET))
                    .pb(px(INSET))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(projects)
                    .when(!others, |list| list.child(self.render_empty(ui))),
            )
            .child(ui::divider(ui))
            .child(div().p(px(INSET)).child(self.render_open(cx)))
            .child(ui::divider(ui))
            .child(
                div()
                    .flex_none()
                    .h(px(FOOTER_HEIGHT))
                    .px(px(INSET + 8.))
                    .flex()
                    .items_center()
                    .child(ui::hint_bar(hints, ui)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    fn shown(rows: &[Row]) -> Vec<(&str, bool, bool)> {
        rows.iter()
            .map(|row| (row.path.to_str().unwrap(), row.current, row.open_elsewhere))
            .collect()
    }

    #[test]
    fn the_current_project_comes_first_then_five_others() {
        let recent = paths(&["/p/a", "/p/b", "/p/c", "/p/d", "/p/e", "/p/f", "/p/g"]);
        let rows = rows(&recent, Some(Path::new("/p/c")), &[], |_| true);
        assert_eq!(
            shown(&rows),
            [
                ("/p/c", true, false),
                ("/p/a", false, false),
                ("/p/b", false, false),
                ("/p/d", false, false),
                ("/p/e", false, false),
                ("/p/f", false, false),
            ]
        );
    }

    #[test]
    fn missing_folders_are_skipped_and_others_marked() {
        let recent = paths(&["/p/a", "/gone", "/p/b", "/p/c"]);
        let open = paths(&["/p/b"]);
        let rows = rows(&recent, None, &open, |path| !path.starts_with("/gone"));
        assert_eq!(
            shown(&rows),
            [
                ("/p/a", false, false),
                ("/p/b", false, true),
                ("/p/c", false, false),
            ]
        );
    }

    #[test]
    fn a_current_project_missing_from_the_list_is_still_shown() {
        let rows = rows(&[], Some(Path::new("/p/new")), &[], |_| true);
        assert_eq!(shown(&rows), [("/p/new", true, false)]);
        assert!(super::rows(&[], None, &[], |_| true).is_empty());
    }

    #[test]
    fn a_branch_takes_its_share_of_the_path_line() {
        assert_eq!(path_budget(None), PATH_CHARS);
        assert_eq!(path_budget(Some("main")), PATH_CHARS - 8);
        assert_eq!(path_budget(Some(&"x".repeat(60))), MIN_PATH_CHARS);
    }

    #[test]
    fn initials_take_the_first_letters_of_two_words() {
        assert_eq!(initials("flux"), "F");
        assert_eq!(initials("flux-plugins"), "FP");
        assert_eq!(initials("lsp_sandbox two"), "LS");
        assert_eq!(initials("MyApp"), "MA");
        assert_eq!(initials("XMLParser"), "XP");
        assert_eq!(initials("проект"), "П");
        assert_eq!(initials("~"), "~");
    }

    #[test]
    fn a_project_keeps_its_shade() {
        let path = Path::new("/Users/me/dev/flux");
        assert_eq!(shade_index(path, 10), shade_index(path, 10));
        assert!(shade_index(path, 10) < 10);
        let spread: std::collections::HashSet<usize> = (0..40)
            .map(|n| shade_index(Path::new(&format!("/p/project-{n}")), 10))
            .collect();
        assert!(spread.len() > 5, "{spread:?}");
    }

    #[test]
    fn head_names_the_branch_or_the_commit() {
        assert_eq!(
            parse_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            parse_head("ref: refs/heads/feature/login").as_deref(),
            Some("feature/login")
        );
        assert_eq!(
            parse_head("3b15ed6c0f7603e4008e5aa1\n").as_deref(),
            Some("3b15ed6")
        );
        assert_eq!(parse_head("garbage"), None);
        assert_eq!(parse_head("ref: "), None);
    }

    #[test]
    fn the_branch_comes_from_the_repository_and_a_worktree_file() {
        let dir = std::env::temp_dir().join(format!("flux-project-popup-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let repo = dir.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/develop\n").unwrap();
        assert_eq!(head_branch(&repo).as_deref(), Some("develop"));
        // A worktree: `.git` is a file pointing to the worktree's git directory.
        let worktree = dir.join("worktree");
        let git_dir = dir.join("repo/.git/worktrees/wt");
        fs::create_dir_all(&git_dir).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/hotfix\n").unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        assert_eq!(head_branch(&worktree).as_deref(), Some("hotfix"));
        assert_eq!(head_branch(&dir), None);
        fs::remove_dir_all(&dir).ok();
    }
}
