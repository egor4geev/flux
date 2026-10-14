//! The state of a project's window between launches (part 10.1), kept per project as JetBrains IDEs
//! keep theirs: the documents open in tabs (in order, the active one, the caret and the scroll of
//! each), what the left island shows (the tree or the commit window), the terminal panel, the tool
//! window on the right (Claude, Notifications), and the window's frame. Next to them — which project
//! windows are open (Settings → System Settings → "Reopen projects on startup").
//!
//! Kept in `~/Library/Application Support/flux/projects.json` (`FLUX_PROJECTS_FILE` — another
//! file; in scenarios without it — neither read nor written). A window writes only its project's
//! entry: the file is read, changed and written back through a temporary file and a `rename`, so
//! two Flux processes don't erase each other's projects, and keys this version doesn't know stay.
//! The chats of a project are kept apart, in `claude-sessions.json` ([`crate::claude_history`]).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{Bounds, Pixels, WindowBounds, point, px, size};
use serde_json::{Map, Value, json};

/// How many projects are remembered: those saved longest ago are forgotten first (the open ones
/// never).
const MAX_PROJECTS: usize = 50;

/// A project's window as it was left.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectState {
    /// The documents open in tabs, in order (files only: diffs, terminals and chats aren't here).
    pub tabs: Vec<SavedTab>,
    /// The active one among `tabs`.
    pub active: Option<usize>,
    pub left: LeftIsland,
    /// The terminal panel was shown.
    pub terminal: bool,
    /// The tool window in the island on the right.
    pub right: Option<RightIsland>,
    /// The window's frame.
    pub bounds: Option<SavedBounds>,
}

/// A document's tab.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedTab {
    pub path: PathBuf,
    /// The caret: a zero-based line and a column in characters.
    pub line: usize,
    pub column: usize,
    /// The first visible line (fractional: a line scrolled partly off the top).
    pub scroll: f32,
}

/// What the left island shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeftIsland {
    #[default]
    Tree,
    Commit,
    Hidden,
}

/// The tool window of the island on the right (plugins' windows aren't remembered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightIsland {
    Claude,
    Notifications,
}

/// A window's frame: in screen coordinates, and how it is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub mode: WindowMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMode {
    #[default]
    Windowed,
    /// Zoomed (the green button with ⌥, a double-click on the title bar); `x`… — the frame it
    /// goes back to.
    Maximized,
    Fullscreen,
}

impl SavedBounds {
    pub fn from_window(bounds: WindowBounds) -> Self {
        let (frame, mode) = match bounds {
            WindowBounds::Windowed(frame) => (frame, WindowMode::Windowed),
            WindowBounds::Maximized(frame) => (frame, WindowMode::Maximized),
            WindowBounds::Fullscreen(frame) => (frame, WindowMode::Fullscreen),
        };
        Self {
            x: f32::from(frame.origin.x),
            y: f32::from(frame.origin.y),
            width: f32::from(frame.size.width),
            height: f32::from(frame.size.height),
            mode,
        }
    }

    pub fn frame(&self) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(self.x), px(self.y)),
            size: size(px(self.width), px(self.height)),
        }
    }

    pub fn to_window(self) -> WindowBounds {
        let frame = self.frame();
        match self.mode {
            WindowMode::Windowed => WindowBounds::Windowed(frame),
            WindowMode::Maximized => WindowBounds::Maximized(frame),
            WindowMode::Fullscreen => WindowBounds::Fullscreen(frame),
        }
    }
}

/// The project windows that are open (while Flux runs) or were open when it quit: a launch without
/// a project opens them again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenProjects {
    /// Front to back.
    pub roots: Vec<PathBuf>,
    /// The window in front, which takes the keyboard.
    pub active: Option<PathBuf>,
}

/// How the project's window was left; `None` — never saved (or forgotten).
pub fn load(root: &Path) -> Option<ProjectState> {
    let file = store_path()?;
    parse_project(&read(&file)["projects"][key(root)])
}

/// Remembers how the project's window is. Errors go to stderr: the window works on without it.
pub fn save(root: &Path, state: &ProjectState) {
    let Some(file) = store_path() else {
        return;
    };
    let mut value = read(&file);
    put_project(&mut value, root, state, now());
    if let Err(error) = write(&file, &value) {
        eprintln!("flux: {}: {error}", file.display());
    }
}

/// The project windows open now or at the last quit.
pub fn open_projects() -> OpenProjects {
    store_path()
        .map(|file| parse_open(&read(&file)["open"]))
        .unwrap_or_default()
}

/// Remembers which project windows are open (written only when it changed).
pub fn set_open_projects(open: &OpenProjects) {
    let Some(file) = store_path() else {
        return;
    };
    let mut value = read(&file);
    if parse_open(&value["open"]) == *open {
        return;
    }
    put_open(&mut value, open);
    if let Err(error) = write(&file, &value) {
        eprintln!("flux: {}: {error}", file.display());
    }
}

fn key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn store_path() -> Option<PathBuf> {
    store_path_for(
        std::env::var_os("FLUX_PROJECTS_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    )
}

/// Where the projects are kept: an explicit file, otherwise (outside a scenario) Application
/// Support.
fn store_path_for(
    file: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = file.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    let home = PathBuf::from(home?);
    Some(home.join("Library/Application Support/flux/projects.json"))
}

/// The file as JSON; missing or broken — an empty object.
fn read(file: &Path) -> Value {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return json!({}),
        Err(error) => {
            eprintln!("flux: {}: {error}", file.display());
            return json!({});
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

/// `{"tabs": [{"path", "line", "column", "scroll"}], "active", "left", "terminal", "right",
/// "bounds": {"x", "y", "width", "height", "mode"}, "saved_at"}`; what doesn't fit is skipped.
fn parse_project(value: &Value) -> Option<ProjectState> {
    if !value.is_object() {
        return None;
    }
    let tabs: Vec<SavedTab> = value["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tab| {
            let path = PathBuf::from(tab["path"].as_str()?);
            path.is_absolute().then(|| SavedTab {
                path,
                line: tab["line"].as_u64().unwrap_or(0) as usize,
                column: tab["column"].as_u64().unwrap_or(0) as usize,
                scroll: tab["scroll"].as_f64().unwrap_or(0.).max(0.) as f32,
            })
        })
        .collect();
    let active = value["active"]
        .as_u64()
        .map(|active| active as usize)
        .filter(|active| *active < tabs.len());
    let left = match value["left"].as_str() {
        Some("commit") => LeftIsland::Commit,
        Some("hidden") => LeftIsland::Hidden,
        _ => LeftIsland::Tree,
    };
    let right = match value["right"].as_str() {
        Some("claude") => Some(RightIsland::Claude),
        Some("notifications") => Some(RightIsland::Notifications),
        _ => None,
    };
    Some(ProjectState {
        tabs,
        active,
        left,
        terminal: value["terminal"].as_bool().unwrap_or(false),
        right,
        bounds: parse_bounds(&value["bounds"]),
    })
}

fn parse_bounds(value: &Value) -> Option<SavedBounds> {
    let number = |key: &str| value[key].as_f64().map(|n| n as f32);
    let (width, height) = (number("width")?, number("height")?);
    if width < 1. || height < 1. {
        return None;
    }
    let mode = match value["mode"].as_str() {
        Some("maximized") => WindowMode::Maximized,
        Some("fullscreen") => WindowMode::Fullscreen,
        _ => WindowMode::Windowed,
    };
    Some(SavedBounds {
        x: number("x")?,
        y: number("y")?,
        width,
        height,
        mode,
    })
}

fn project_json(state: &ProjectState, saved_at: u64) -> Value {
    let tabs: Vec<Value> = state
        .tabs
        .iter()
        .map(|tab| {
            json!({
                "path": tab.path.to_string_lossy(),
                "line": tab.line,
                "column": tab.column,
                "scroll": tab.scroll,
            })
        })
        .collect();
    let left = match state.left {
        LeftIsland::Tree => "tree",
        LeftIsland::Commit => "commit",
        LeftIsland::Hidden => "hidden",
    };
    let right = state.right.map(|right| match right {
        RightIsland::Claude => "claude",
        RightIsland::Notifications => "notifications",
    });
    let bounds = state.bounds.map(|bounds| {
        let mode = match bounds.mode {
            WindowMode::Windowed => "windowed",
            WindowMode::Maximized => "maximized",
            WindowMode::Fullscreen => "fullscreen",
        };
        json!({
            "x": bounds.x,
            "y": bounds.y,
            "width": bounds.width,
            "height": bounds.height,
            "mode": mode,
        })
    });
    json!({
        "tabs": tabs,
        "active": state.active,
        "left": left,
        "terminal": state.terminal,
        "right": right,
        "bounds": bounds,
        "saved_at": saved_at,
    })
}

/// Puts the project's entry into the file's JSON and forgets the oldest projects beyond
/// [`MAX_PROJECTS`] (an open one stays).
fn put_project(value: &mut Value, root: &Path, state: &ProjectState, saved_at: u64) {
    let open = parse_open(&value["open"]);
    let projects = object(value, "projects");
    projects.insert(key(root), project_json(state, saved_at));
    while projects.len() > MAX_PROJECTS {
        let oldest = projects
            .iter()
            .filter(|(root, _)| !open.roots.iter().any(|open| key(open) == **root))
            .min_by_key(|(_, project)| project["saved_at"].as_u64().unwrap_or(0))
            .map(|(root, _)| root.clone());
        let Some(oldest) = oldest else {
            break;
        };
        projects.remove(&oldest);
    }
}

/// `{"roots": ["/a", "/b"], "active": "/a"}`.
fn parse_open(value: &Value) -> OpenProjects {
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in value["roots"].as_array().into_iter().flatten() {
        let Some(root) = root.as_str().map(PathBuf::from) else {
            continue;
        };
        if root.is_absolute() && !roots.contains(&root) {
            roots.push(root);
        }
    }
    let active = value["active"]
        .as_str()
        .map(PathBuf::from)
        .filter(|active| roots.contains(active));
    OpenProjects { roots, active }
}

fn put_open(value: &mut Value, open: &OpenProjects) {
    let roots: Vec<Value> = open
        .roots
        .iter()
        .map(|root| Value::from(root.to_string_lossy()))
        .collect();
    let active = open.active.as_ref().map(|root| root.to_string_lossy());
    if let Some(object) = value.as_object_mut() {
        object.insert("open".into(), json!({ "roots": roots, "active": active }));
    }
}

/// The object under `key` of the file's JSON, made if there is none.
fn object<'a>(value: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    let map = value.as_object_mut().expect("an object");
    let entry = map.entry(key).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().expect("an object")
}

/// Through a temporary file next to the target and a `rename`: never half-written; the process id
/// in its name keeps two Flux processes apart.
fn write(file: &Path, value: &Value) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value).unwrap_or_default() + "\n";
    let mut temp = file.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    fs::write(&temp, text)?;
    fs::rename(&temp, file).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ProjectState {
        ProjectState {
            tabs: vec![
                SavedTab {
                    path: PathBuf::from("/p/src/main.rs"),
                    line: 12,
                    column: 4,
                    scroll: 3.5,
                },
                SavedTab {
                    path: PathBuf::from("/p/README.md"),
                    line: 0,
                    column: 0,
                    scroll: 0.,
                },
            ],
            active: Some(1),
            left: LeftIsland::Commit,
            terminal: true,
            right: Some(RightIsland::Claude),
            bounds: Some(SavedBounds {
                x: 120.,
                y: 64.,
                width: 1240.,
                height: 820.,
                mode: WindowMode::Maximized,
            }),
        }
    }

    #[test]
    fn a_project_round_trips_through_the_json() {
        let mut value = json!({});
        put_project(&mut value, Path::new("/p"), &state(), 7);
        let text = serde_json::to_string(&value).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parse_project(&back["projects"]["/p"]), Some(state()));
        assert_eq!(back["projects"]["/p"]["saved_at"], 7);
        let empty = ProjectState::default();
        put_project(&mut value, Path::new("/q"), &empty, 8);
        assert_eq!(parse_project(&value["projects"]["/q"]), Some(empty));
        assert_eq!(parse_project(&value["projects"]["/r"]), None);
    }

    #[test]
    fn broken_entries_fall_back_to_defaults() {
        let value = json!({
            "tabs": [{"path": "relative.rs"}, {"path": "/p/a.rs", "line": "x"}, 5],
            "active": 9,
            "left": "nowhere",
            "bounds": {"x": 1, "y": 2, "width": 0, "height": 10},
        });
        let state = parse_project(&value).unwrap();
        assert_eq!(
            state.tabs,
            vec![SavedTab {
                path: PathBuf::from("/p/a.rs"),
                line: 0,
                column: 0,
                scroll: 0.,
            }]
        );
        assert_eq!(state.active, None);
        assert_eq!(state.left, LeftIsland::Tree);
        assert_eq!(state.bounds, None);
    }

    #[test]
    fn the_oldest_projects_are_forgotten_but_not_the_open_ones() {
        let mut value = json!({});
        put_open(
            &mut value,
            &OpenProjects {
                roots: vec![PathBuf::from("/p0")],
                active: None,
            },
        );
        for n in 0..MAX_PROJECTS + 2 {
            let root = PathBuf::from(format!("/p{n}"));
            put_project(&mut value, &root, &ProjectState::default(), n as u64);
        }
        let projects = value["projects"].as_object().unwrap();
        assert_eq!(projects.len(), MAX_PROJECTS);
        // /p0 is the oldest, but open: /p1 and /p2 went instead.
        assert!(projects.contains_key("/p0"));
        assert!(!projects.contains_key("/p1"));
        assert!(!projects.contains_key("/p2"));
        assert!(projects.contains_key(&format!("/p{}", MAX_PROJECTS + 1)));
    }

    #[test]
    fn open_projects_keep_their_order_and_the_active_one() {
        let mut value = json!({"projects": {}});
        let open = OpenProjects {
            roots: vec![PathBuf::from("/b"), PathBuf::from("/a")],
            active: Some(PathBuf::from("/b")),
        };
        put_open(&mut value, &open);
        assert_eq!(parse_open(&value["open"]), open);
        let odd = json!({"roots": ["/a", "/a", "rel", 3], "active": "/c"});
        assert_eq!(
            parse_open(&odd),
            OpenProjects {
                roots: vec![PathBuf::from("/a")],
                active: None,
            }
        );
    }

    #[test]
    fn the_file_keeps_other_projects_and_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("flux-projects-test-{}", std::process::id()));
        let file = dir.join("projects.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&file, r#"{"future": 1, "projects": {"/other": {"tabs": []}}}"#).unwrap();
        let mut value = read(&file);
        put_project(&mut value, Path::new("/p"), &state(), 1);
        write(&file, &value).unwrap();
        let back = read(&file);
        assert_eq!(back["future"], 1);
        assert!(back["projects"]["/other"].is_object());
        assert_eq!(parse_project(&back["projects"]["/p"]), Some(state()));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bounds_convert_to_and_from_the_window() {
        let saved = state().bounds.unwrap();
        assert_eq!(SavedBounds::from_window(saved.to_window()), saved);
        let windowed = SavedBounds {
            mode: WindowMode::Windowed,
            ..saved
        };
        assert!(matches!(windowed.to_window(), WindowBounds::Windowed(_)));
    }

    #[test]
    fn the_file_is_in_application_support_but_not_in_scenarios() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            store_path_for(None, false, home.clone()),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/projects.json"
            ))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(Some(OsString::from("/tmp/p.json")), true, home),
            Some(PathBuf::from("/tmp/p.json"))
        );
    }
}
