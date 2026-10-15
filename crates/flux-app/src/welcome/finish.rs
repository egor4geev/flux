//! "All set" (the guide's last slide): Open Folder…, the start screen, and the recent projects —
//! Flux's own and those other IDEs on this Mac remember, so the first project is a click away.
//!
//! Other IDEs keep their recent projects in their settings folders under
//! `~/Library/Application Support`: the IDEs of one family in `<IDE><version>/options/
//! recentProjects.xml` (paths with `$USER_HOME$`, the times they were opened), the editors of
//! another in `User/globalStorage/storage.json` (the folders of their Open Recent menu, newest
//! first). Only folders that still exist count; the slide names no IDE. `FLUX_OTHER_IDES_DIR`
//! reads another folder instead (a scenario without it reads none, as it doesn't read Flux's own
//! recent projects).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use gpui::{
    AnyElement, App, BoxShadow, Context, FontWeight, PathPromptOptions, Task, Window, WindowBounds,
    div, img, point, prelude::*, px, size,
};

use super::WelcomeGuide;
use super::claude::front_workspace;
use crate::i18n::{tr, trf};
use crate::icons::{self, IconName, icon};
use crate::project_popup::{branch_label, head_branch, project_tile};
use crate::start_screen::{display_name, shorten_path};
use crate::theme::{self, Theme, UiColors};
use crate::ui::{self, RADIUS_MD};
use crate::workspace::{self, tilde};
use crate::{command_palette, recent};

/// How many recent projects the slide offers.
const MAX_SHOWN: usize = 5;
const ROW_HEIGHT: f32 = 44.;
const LIST_WIDTH: f32 = 460.;
const PATH_CHARS: usize = 44;
const LOGO_SIZE: f32 = 64.;
/// The size of a start window opened from the slide, as a new Flux window gets one.
const START_WINDOW: (f32, f32) = (1240., 820.);
/// The settings folders of the editors whose recent folders are their `storage.json`'s Open Recent
/// menu.
const MENU_EDITORS: [&str; 5] = ["Code", "Code - Insiders", "Cursor", "VSCodium", "Windsurf"];
/// The folders that hold the IDEs whose recent projects are `recentProjects.xml`: one family's
/// folder of IDEs, and another vendor's IDE folders named by version (`<prefix>2025.1`).
const XML_IDE_FAMILY: &str = "JetBrains";
const XML_IDE_VENDOR: (&str, &str) = ("Google", "AndroidStudio");

/// A project the slide offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub path: PathBuf,
    /// The git branch it is on, as the start screen shows it.
    pub branch: Option<String>,
}

/// The projects the slide offers, read in the background when the guide opens.
#[derive(Default)]
pub struct Finish {
    /// Flux's recent projects first, then the other IDEs' ones not among them.
    pub projects: Vec<Project>,
    _load: Option<Task<()>>,
}

impl Finish {
    /// Reads the recent projects (in the background; the slide redraws when they are there).
    pub fn load(cx: &mut Context<WelcomeGuide>) -> Self {
        let read = cx.background_spawn(async move { recent_projects() });
        let load = cx.spawn(async move |this, cx| {
            let projects = read.await;
            this.update(cx, |guide, cx| {
                guide.finish.projects = projects;
                cx.notify();
            })
            .ok();
        });
        Self {
            projects: Vec::new(),
            _load: Some(load),
        }
    }
}

pub fn render(
    guide: &mut WelcomeGuide,
    _window: &mut Window,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let ui = Theme::ui(cx);
    let palette = ui::shortcut_for(&command_palette::Toggle, cx).unwrap_or_default();
    let open_keys = ui::shortcut_for(&workspace::Open, cx);
    let logo = div()
        .relative()
        .size(px(LOGO_SIZE))
        .flex()
        .items_center()
        .justify_center()
        // The glow behind the spark, in the accent.
        .child(
            div()
                .absolute()
                .top(px(LOGO_SIZE * 0.2))
                .left(px(LOGO_SIZE * 0.2))
                .size(px(LOGO_SIZE * 0.6))
                .rounded_full()
                .shadow(vec![BoxShadow {
                    color: UiColors::tint(ui.accent, 0.55),
                    offset: point(px(0.), px(6.)),
                    blur_radius: px(36.),
                    spread_radius: px(6.),
                }]),
        )
        .child(img(icons::LOGO).size(px(LOGO_SIZE)));
    let open_folder = big_button("welcome-open-folder", true, ui)
        .child(icon(IconName::FolderOpen, ui.accent_text).size(px(15.)))
        .child(tr("Open Folder…"))
        .children(open_keys.map(|keys| ui::keys(&keys, ui)))
        .on_click(cx.listener(|_, _, window, cx| open_folder(window, cx)));
    let start_screen = big_button("welcome-start-screen", false, ui)
        .child(tr("Start Screen"))
        .on_click(cx.listener(|guide, _, window, cx| {
            if front_workspace(cx).is_none() {
                open_start_window(cx);
            }
            guide.close(window, cx);
        }));
    let mut rows = Vec::new();
    for (index, project) in guide.finish.projects.clone().into_iter().enumerate() {
        rows.push(project_row(index, project, ui, cx));
    }
    let list = (!rows.is_empty()).then(|| {
        div()
            .mt(px(28.))
            .w(px(LIST_WIDTH))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_2p5()
                    .pb_1p5()
                    .child(ui::section_label(tr("Recent Projects"), ui)),
            )
            .children(rows)
    });
    div()
        .id("welcome-finish")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .px_10()
        .py_8()
        .child(div().flex_1().min_h(px(0.)))
        .child(logo)
        .child(
            div()
                .mt_4()
                .text_size(px(28.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr("All Set")),
        )
        .child(
            div()
                .mt_2()
                .max_w(px(480.))
                .text_center()
                .text_size(px(14.))
                .line_height(px(21.))
                .text_color(ui.text_muted)
                .child(trf(
                    "Open a project folder — Flux finds its git, languages and Claude sessions by itself. The guide is always at hand: Help → Welcome Guide, or {0}.",
                    &[&palette],
                )),
        )
        .child(
            div()
                .mt(px(22.))
                .flex()
                .items_center()
                .gap_2p5()
                .child(open_folder)
                .child(start_screen),
        )
        .children(list)
        .child(div().flex_1().min_h(px(0.)))
        .into_any_element()
}

/// A recent project: its tile, name and path, its branch; a click opens it.
fn project_row(
    index: usize,
    project: Project,
    ui: UiColors,
    cx: &mut Context<WelcomeGuide>,
) -> AnyElement {
    let Project { path, branch } = project;
    div()
        .id(("welcome-recent", index))
        .h(px(ROW_HEIGHT))
        .px_2p5()
        .flex()
        .items_center()
        .gap_3()
        .rounded(px(RADIUS_MD))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .active(move |style| style.bg(ui.pressed))
        .child(project_tile(&path, &ui))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(display_name(&path)),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(shorten_path(&tilde(&path), PATH_CHARS)),
                ),
        )
        .children(branch.map(|branch| branch_label(&branch, ui)))
        .on_click(cx.listener(move |guide, _, window, cx| {
            open_project(path.clone(), cx);
            guide.close(window, cx);
        }))
        .into_any_element()
}

/// A large button of the slide: the accent's tint for the main one (as Flux's main buttons), a
/// bordered one for the other.
fn big_button(id: &'static str, primary: bool, ui: UiColors) -> gpui::Stateful<gpui::Div> {
    let button = div()
        .id(id)
        .h(px(34.))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_MD))
        .border_1()
        .text_size(px(theme::TEXT_MD + 0.5))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer();
    if primary {
        button
            .border_color(UiColors::tint(ui.accent, 0.55))
            .bg(UiColors::tint(ui.accent, 0.24))
            .text_color(ui.accent_text)
            .hover(move |style| style.bg(UiColors::tint(ui.accent, 0.34)))
            .active(move |style| style.bg(UiColors::tint(ui.accent, 0.42)))
    } else {
        button
            .border_color(ui.input_border)
            .text_color(ui.foreground)
            .hover(move |style| style.bg(ui.hover))
            .active(move |style| style.bg(ui.pressed))
    }
}

/// Open Folder…: the system's folder panel; the chosen folder opens as a project and the guide
/// closes.
fn open_folder(window: &mut Window, cx: &mut Context<WelcomeGuide>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: None,
    });
    cx.spawn_in(window, async move |this, cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let Some(root) = paths.into_iter().next() else {
            return;
        };
        this.update_in(cx, |guide, window, cx| {
            open_project(root, cx);
            guide.close(window, cx);
        })
        .ok();
    })
    .detach();
}

/// Opens a project as the Flux window in front opens one (the window that has it comes forward,
/// an empty window takes it, otherwise as Settings → System says); with no Flux window — the guide
/// alone, on the first launch — in a new one.
fn open_project(root: PathBuf, cx: &mut App) {
    match front_workspace(cx) {
        Some(handle) => {
            // The click comes while the guide's window is being updated: the other window after it.
            cx.defer(move |cx| {
                handle
                    .update(cx, |workspace, window, cx| {
                        window.activate_window();
                        workspace.open_project(root, Vec::new(), window, cx);
                    })
                    .ok();
            });
        }
        None => {
            workspace::open_project_window(root, Vec::new(), None, cx);
        }
    }
}

/// A Flux window with the start screen, where a new window opens.
fn open_start_window(cx: &mut App) {
    let (width, height) = START_WINDOW;
    let bounds = gpui::Bounds::centered(None, size(px(width), px(height)), cx);
    workspace::open_window(None, Vec::new(), WindowBounds::Windowed(bounds), cx);
}

// --- Recent projects ---

/// Flux's recent projects, then the other IDEs' ones not among them, newest first, with their
/// branches: up to [`MAX_SHOWN`].
fn recent_projects() -> Vec<Project> {
    let mut seen = HashSet::new();
    let own = recent::stored().unwrap_or_default();
    let others = other_ides_dir().map_or_else(Vec::new, |dir| other_projects(&dir));
    own.into_iter()
        .chain(others)
        .filter(|path| path.is_dir())
        .filter(|path| seen.insert(fs::canonicalize(path).unwrap_or_else(|_| path.clone())))
        .take(MAX_SHOWN)
        .map(|path| Project {
            branch: head_branch(&path),
            path,
        })
        .collect()
}

/// Where other IDEs keep their settings: `~/Library/Application Support`, or `FLUX_OTHER_IDES_DIR`;
/// none in a scenario without it.
fn other_ides_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FLUX_OTHER_IDES_DIR").filter(|dir| !dir.is_empty()) {
        return Some(dir.into());
    }
    if std::env::var_os("FLUX_SCENARIO").is_some() {
        return None;
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join("Library/Application Support"))
}

/// The recent projects the IDEs under `support` remember, newest first, no repeats.
fn other_projects(support: &Path) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut found: Vec<(u64, PathBuf)> = Vec::new();
    for file in xml_lists(support) {
        if let Ok(text) = fs::read_to_string(&file) {
            let when = modified_ms(&file);
            found.extend(read_recent_xml(&text, when, home.as_deref()));
        }
    }
    for editor in MENU_EDITORS {
        let file = support.join(editor).join("User/globalStorage/storage.json");
        if let Ok(text) = fs::read_to_string(&file) {
            found.extend(read_recent_menu(&text, modified_ms(&file)));
        }
    }
    found.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
    let mut seen = HashSet::new();
    found
        .into_iter()
        .map(|(_, path)| path)
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// The `recentProjects.xml` files of the IDEs under `support`.
fn xml_lists(support: &Path) -> Vec<PathBuf> {
    let mut ides: Vec<PathBuf> = Vec::new();
    let dirs = |dir: PathBuf| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    ides.extend(dirs(support.join(XML_IDE_FAMILY)));
    let (vendor, prefix) = XML_IDE_VENDOR;
    ides.extend(dirs(support.join(vendor)).into_iter().filter(|dir| {
        dir.file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
    }));
    ides.into_iter()
        .map(|ide| ide.join("options/recentProjects.xml"))
        .filter(|file| file.is_file())
        .collect()
}

/// A file's last change, in milliseconds since 1970 (0 when unknown).
fn modified_ms(file: &Path) -> u64 {
    fs::metadata(file)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis() as u64)
}

/// The projects of a `recentProjects.xml`, with when they were last used (milliseconds since 1970):
/// its entries' activation (or opening) time; entries without one — and the older plain list of
/// paths — just before `modified` (the file's change), in their order. `$USER_HOME$` is `home`; a
/// path with another macro is skipped.
fn read_recent_xml(text: &str, modified: u64, home: Option<&Path>) -> Vec<(u64, PathBuf)> {
    let mut found = Vec::new();
    let mut untimed = 0;
    let mut rest = text;
    while let Some(at) = rest.find("<entry key=\"") {
        rest = &rest[at + "<entry key=\"".len()..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let key = unescape(&rest[..end]);
        // The entry's own options: up to the next entry (or the end).
        let body_end = rest.find("<entry key=\"").unwrap_or(rest.len());
        let body = &rest[..body_end];
        let time = ["activationTimestamp", "projectOpenTimestamp"]
            .iter()
            .find_map(|name| option_value(body, name)?.parse::<u64>().ok());
        if let Some(path) = expand(&key, home) {
            let when = time.unwrap_or_else(|| {
                untimed += 1;
                modified.saturating_sub(untimed)
            });
            found.push((when, path));
        }
    }
    // The format before the entries: `<option name="recentPaths"><list><option value="…"/>`.
    if let Some(at) = text.find("<option name=\"recentPaths\">") {
        let list = &text[at..];
        let list = &list[..list.find("</list>").unwrap_or(list.len())];
        let mut rest = list;
        while let Some(at) = rest.find("<option value=\"") {
            rest = &rest[at + "<option value=\"".len()..];
            let Some(end) = rest.find('"') else {
                break;
            };
            if let Some(path) = expand(&unescape(&rest[..end]), home) {
                untimed += 1;
                found.push((modified.saturating_sub(untimed), path));
            }
        }
    }
    found
}

/// `<option name="{name}" value="…"/>` in `body`.
fn option_value<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("name=\"{name}\" value=\"");
    let at = body.find(&marker)? + marker.len();
    let value = &body[at..];
    Some(&value[..value.find('"')?])
}

/// A path of the list: `$USER_HOME$` is the home folder; other macros (the IDE's own folders) and
/// relative paths aren't projects to offer.
fn expand(path: &str, home: Option<&Path>) -> Option<PathBuf> {
    let path = match path.strip_prefix("$USER_HOME$") {
        Some(rest) => {
            let home = home?.to_string_lossy();
            format!("{home}{rest}")
        }
        None => path.to_string(),
    };
    (path.starts_with('/') && !path.contains('$')).then(|| PathBuf::from(path))
}

/// XML's escapes in an attribute.
fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The folders of a `storage.json`'s Open Recent menu, newest first, timed just before `modified`
/// (the file's change) in their order.
fn read_recent_menu(text: &str, modified: u64) -> Vec<(u64, PathBuf)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let menu = &value["lastKnownMenubarData"]["menus"]["File"]["items"];
    let Some(items) = menu.as_array() else {
        return Vec::new();
    };
    let recent = items
        .iter()
        .find(|item| item["id"] == "submenuitem.MenubarRecentMenu")
        .and_then(|item| item["submenu"]["items"].as_array());
    let Some(recent) = recent else {
        return Vec::new();
    };
    recent
        .iter()
        .filter(|item| item["id"] == "openRecentFolder" && item["uri"]["scheme"] == "file")
        .filter_map(|item| item["uri"]["path"].as_str())
        .enumerate()
        .map(|(index, path)| {
            (
                modified.saturating_sub(index as u64 + 1),
                PathBuf::from(path),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<application>
  <component name="RecentProjectsManager">
    <option name="additionalInfo">
      <map>
        <entry key="$USER_HOME$/dev/old">
          <value>
            <RecentProjectMetaInfo frameTitle="old" projectWorkspaceId="1">
              <option name="activationTimestamp" value="1700000000000" />
              <option name="projectOpenTimestamp" value="1690000000000" />
            </RecentProjectMetaInfo>
          </value>
        </entry>
        <entry key="$USER_HOME$/dev/R&amp;D tools">
          <value>
            <RecentProjectMetaInfo opened="true" projectWorkspaceId="2">
              <option name="projectOpenTimestamp" value="1760000000000" />
            </RecentProjectMetaInfo>
          </value>
        </entry>
        <entry key="$APPLICATION_CONFIG_DIR$/scratches">
          <value><RecentProjectMetaInfo /></value>
        </entry>
        <entry key="/Volumes/work/api">
          <value><RecentProjectMetaInfo /></value>
        </entry>
      </map>
    </option>
    <option name="lastOpenedProject" value="$USER_HOME$/dev/R&amp;D tools" />
  </component>
</application>"#;

    #[test]
    fn xml_entries_carry_their_times_and_the_home_folder() {
        let home = Path::new("/Users/someone");
        let found = read_recent_xml(XML, 1_750_000_000_000, Some(home));
        assert_eq!(
            found,
            vec![
                (1_700_000_000_000, PathBuf::from("/Users/someone/dev/old")),
                (
                    1_760_000_000_000,
                    PathBuf::from("/Users/someone/dev/R&D tools")
                ),
                // No time of its own: just before the file's change.
                (1_749_999_999_999, PathBuf::from("/Volumes/work/api")),
            ]
        );
    }

    #[test]
    fn the_older_plain_list_of_paths_is_read_in_order() {
        let xml = r#"<application><component name="RecentDirectoryProjectsManager">
    <option name="recentPaths">
      <list>
        <option value="$USER_HOME$/a" />
        <option value="$USER_HOME$/b" />
      </list>
    </option>
  </component></application>"#;
        let found = read_recent_xml(xml, 1000, Some(Path::new("/h")));
        assert_eq!(
            found,
            vec![(999, PathBuf::from("/h/a")), (998, PathBuf::from("/h/b"))]
        );
        // Without a home folder, `$USER_HOME$` paths can't be told.
        assert!(read_recent_xml(xml, 1000, None).is_empty());
    }

    #[test]
    fn the_open_recent_menu_gives_its_folders_newest_first() {
        let json = r#"{"lastKnownMenubarData": {"menus": {"File": {"items": [
            {"id": "workbench.action.files.newUntitledFile", "label": "New"},
            {"id": "submenuitem.MenubarRecentMenu", "label": "Open Recent", "submenu": {"items": [
                {"id": "workbench.action.reopenClosedEditor"},
                {"id": "openRecentFolder", "uri": {"$mid": 1, "path": "/Users/someone/dev/flux", "scheme": "file"}},
                {"id": "openRecentWorkspace", "uri": {"path": "/Users/someone/w.code-workspace", "scheme": "file"}},
                {"id": "openRecentFolder", "uri": {"path": "/remote/x", "scheme": "vscode-remote"}},
                {"id": "openRecentFolder", "uri": {"path": "/Users/someone/dev/api", "scheme": "file"}}
            ]}}
        ]}}}}"#;
        assert_eq!(
            read_recent_menu(json, 5000),
            vec![
                (4999, PathBuf::from("/Users/someone/dev/flux")),
                (4998, PathBuf::from("/Users/someone/dev/api")),
            ]
        );
        assert!(read_recent_menu("not json", 5000).is_empty());
        assert!(read_recent_menu("{}", 5000).is_empty());
    }

    #[test]
    fn the_ides_lists_merge_newest_first_without_repeats() {
        let support = std::env::temp_dir().join(format!("flux-other-ides-{}", std::process::id()));
        let _ = fs::remove_dir_all(&support);
        let options = support.join(XML_IDE_FAMILY).join("Ide2026.1/options");
        fs::create_dir_all(&options).unwrap();
        fs::write(
            options.join("recentProjects.xml"),
            r#"<map><entry key="/p/older"><option name="activationTimestamp" value="10" /></entry>
<entry key="/p/shared"><option name="activationTimestamp" value="30" /></entry></map>"#,
        )
        .unwrap();
        let (vendor, prefix) = XML_IDE_VENDOR;
        let studio = support.join(vendor).join(format!("{prefix}2025.1/options"));
        fs::create_dir_all(&studio).unwrap();
        fs::write(
            studio.join("recentProjects.xml"),
            r#"<entry key="/p/newest"><option name="projectOpenTimestamp" value="40" /></entry>"#,
        )
        .unwrap();
        // Another vendor folder that isn't an IDE of the list.
        let other = support.join(vendor).join("Chrome/options");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join("recentProjects.xml"),
            r#"<entry key="/p/not-an-ide"><option name="projectOpenTimestamp" value="99" /></entry>"#,
        )
        .unwrap();
        let found = other_projects(&support);
        assert_eq!(
            found,
            vec![
                PathBuf::from("/p/newest"),
                PathBuf::from("/p/shared"),
                PathBuf::from("/p/older"),
            ]
        );
        fs::remove_dir_all(&support).ok();
    }

    #[test]
    fn macros_other_than_the_home_folder_are_skipped() {
        assert_eq!(expand("/abs/path", None), Some(PathBuf::from("/abs/path")));
        assert_eq!(expand("$PROJECT_DIR$/x", Some(Path::new("/h"))), None);
        assert_eq!(expand("relative/path", None), None);
        assert_eq!(unescape("a &amp; b &quot;c&quot;"), "a & b \"c\"");
    }
}
