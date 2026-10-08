//! Links in terminal output: with ⌘ held, a URL or a file location under the mouse is underlined
//! and the pointer becomes a hand; ⌘-click opens it — a file in the editor at its line and column,
//! a directory in the file tree, a URL in the browser ([`TerminalViewEvent::OpenLink`]).
//!
//! What counts as a link is the terminal's business ([`flux_term::Terminal::link_at`]: OSC 8
//! hyperlinks, URLs, `path:line:col` and the like in the text). Here a path is resolved against the
//! shell's directory, then the project; only an existing file or directory becomes a link. While
//! the mouse stays on the hovered link, nothing is looked up again; a click always looks at what is
//! under it now, so a link that scrolled away with new output is never opened.

use std::path::PathBuf;

use flux_term::{GridPoint, LinkTarget};
use gpui::{
    Context, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point,
    Window, px,
};

use crate::terminal_element::TerminalLayout;
use crate::terminal_view::{TerminalLink, TerminalView, TerminalViewEvent};

/// The links of one terminal: what is under the mouse while ⌘ is held.
#[derive(Debug, Default)]
pub struct LinkState {
    /// The element underlines it and the pointer becomes a hand.
    pub(crate) hovered: Option<HoveredLink>,
}

/// The link under the mouse while ⌘ is held: the element underlines its cells (inclusive), a click
/// opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoveredLink {
    pub start: GridPoint,
    pub end: GridPoint,
    pub link: TerminalLink,
}

impl HoveredLink {
    fn contains(&self, point: GridPoint) -> bool {
        self.start <= point && point <= self.end
    }
}

/// The mouse moved over the terminal text. While a button is held the mouse selects text, so only
/// ⌘ alone points at links.
pub fn mouse_moved(
    view: &mut TerminalView,
    event: &MouseMoveEvent,
    _: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    let pointing = event.modifiers.platform && event.pressed_button.is_none();
    hover(view, pointing.then_some(event.position), cx);
}

/// ⌘ pressed or released (the event comes to the focused terminal): the link under the mouse
/// appears or goes away.
pub fn modifiers_changed(
    view: &mut TerminalView,
    event: &ModifiersChangedEvent,
    window: &mut Window,
    cx: &mut Context<TerminalView>,
) {
    let position = event.modifiers.platform.then(|| window.mouse_position());
    hover(view, position, cx);
}

/// ⌘-click: opens the link under the mouse. `true` if there was one: the click doesn't select.
pub fn cmd_click(
    view: &mut TerminalView,
    event: &MouseDownEvent,
    _: &mut Window,
    cx: &mut Context<TerminalView>,
) -> bool {
    if !event.modifiers.platform || event.button != MouseButton::Left {
        return false;
    }
    let Some(found) = point_at(view, event.position).and_then(|point| find(view, point)) else {
        return false;
    };
    view.links.hovered = None;
    cx.emit(TerminalViewEvent::OpenLink(found.link));
    cx.notify();
    true
}

/// Shows the link at `position` (`None` — ⌘ is up or the mouse left: none).
fn hover(view: &mut TerminalView, position: Option<Point<Pixels>>, cx: &mut Context<TerminalView>) {
    let point = position.and_then(|position| point_at(view, position));
    let hovered = match point {
        Some(point)
            if view
                .links
                .hovered
                .as_ref()
                .is_some_and(|link| link.contains(point)) =>
        {
            return;
        }
        Some(point) => find(view, point),
        None => None,
    };
    if hovered != view.links.hovered {
        view.links.hovered = hovered;
        cx.notify();
    }
}

/// The grid cell under a window point, if the point is over the grid (not its padding or the
/// search bar).
fn point_at(view: &TerminalView, position: Point<Pixels>) -> Option<GridPoint> {
    let layout = view.layout?;
    over_grid(&layout, position).then(|| layout.grid_point(position).0)
}

fn over_grid(layout: &TerminalLayout, position: Point<Pixels>) -> bool {
    let x = position.x - layout.origin.x;
    let y = position.y - layout.origin.y;
    x >= px(0.)
        && y >= px(0.)
        && x < layout.cell_width * layout.columns as f32
        && y < layout.line_height * layout.rows as f32
}

/// The link at a cell, resolved; `None` if there is none or its path doesn't exist.
fn find(view: &TerminalView, point: GridPoint) -> Option<HoveredLink> {
    let found = view.terminal.link_at(point)?;
    let cwd = view.cwd();
    let root = view.root.as_deref();
    let link = resolve(&found.target, |path| {
        flux_term::links::resolve(path, cwd.as_deref(), root)
    })?;
    Some(HoveredLink {
        start: found.start,
        end: found.end,
        link,
    })
}

/// Where a link from the output leads: a URL as it is; a path through `resolve_path` (`None` — it
/// doesn't exist), a directory without its line and column.
fn resolve(
    target: &LinkTarget,
    resolve_path: impl FnOnce(&str) -> Option<PathBuf>,
) -> Option<TerminalLink> {
    match target {
        LinkTarget::Url(url) => Some(TerminalLink::Url(url.clone())),
        LinkTarget::Path { path, line, column } => {
            let path = resolve_path(path)?;
            Some(if path.is_dir() {
                TerminalLink::Directory(path)
            } else {
                TerminalLink::File {
                    path,
                    line: *line,
                    column: *column,
                }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    /// A directory with `src/main.rs` in it, removed at the end of the test.
    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("flux-terminal-links-{name}-{}", std::process::id()));
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
            Self(dir)
        }

        fn lookup(&self, path: &str) -> Option<PathBuf> {
            let path = self.0.join(path);
            path.exists().then_some(path)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn path(path: &str, line: Option<u32>, column: Option<u32>) -> LinkTarget {
        LinkTarget::Path {
            path: path.into(),
            line,
            column,
        }
    }

    #[test]
    fn files_keep_their_line_and_column() {
        let sandbox = Sandbox::new("file");
        let link = resolve(&path("src/main.rs", Some(12), Some(5)), |p| {
            sandbox.lookup(p)
        });
        assert_eq!(
            link,
            Some(TerminalLink::File {
                path: sandbox.0.join("src/main.rs"),
                line: Some(12),
                column: Some(5),
            })
        );
    }

    #[test]
    fn directories_and_missing_paths() {
        let sandbox = Sandbox::new("dir");
        assert_eq!(
            resolve(&path("src", Some(3), None), |p| sandbox.lookup(p)),
            Some(TerminalLink::Directory(sandbox.0.join("src")))
        );
        assert_eq!(
            resolve(&path("src/missing.rs", Some(1), None), |p| sandbox
                .lookup(p)),
            None
        );
    }

    /// The way of a ⌘-click without the mouse: a location a program printed, found in the real grid
    /// by the terminal, resolved against the shell's directory.
    #[test]
    fn a_location_printed_by_a_program_opens_its_file() {
        let sandbox = Sandbox::new("pty");
        let script = "printf 'error: src/main.rs:12:5: oops'; sleep 2";
        let options = flux_term::TerminalOptions {
            command: Some(("/bin/sh".into(), vec!["-c".into(), script.into()])),
            cwd: Some(sandbox.0.clone()),
            ..Default::default()
        };
        let (terminal, _events) = flux_term::Terminal::spawn(options).unwrap();
        // Inside "src/main.rs", which starts after "error: ".
        let point = GridPoint::new(0, 10);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let found = loop {
            if let Some(found) = terminal.link_at(point) {
                break found;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no link in the output"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(found.start, GridPoint::new(0, 7));
        assert!(found.end >= GridPoint::new(0, 17), "{found:?}");
        let cwd = Some(sandbox.0.as_path());
        let link = resolve(&found.target, |path| {
            flux_term::links::resolve(path, cwd, None)
        });
        assert_eq!(
            link,
            Some(TerminalLink::File {
                path: sandbox.0.join("src/main.rs"),
                line: Some(12),
                column: Some(5),
            })
        );
    }

    #[test]
    fn urls_need_no_lookup() {
        let url = "https://example.com/a?b=c".to_string();
        assert_eq!(
            resolve(&LinkTarget::Url(url.clone()), |_| unreachable!()),
            Some(TerminalLink::Url(url))
        );
    }

    #[test]
    fn only_points_over_the_grid_count() {
        let layout = TerminalLayout {
            origin: point(px(10.), px(20.)),
            cell_width: px(8.),
            line_height: px(18.),
            columns: 10,
            rows: 5,
            display_offset: 0,
            cursor: None,
            mode: Default::default(),
        };
        assert!(over_grid(&layout, point(px(10.), px(20.))));
        assert!(over_grid(&layout, point(px(89.5), px(109.5))));
        // Right of the last column, below the last row, in the padding before the first.
        assert!(!over_grid(&layout, point(px(90.), px(30.))));
        assert!(!over_grid(&layout, point(px(20.), px(110.))));
        assert!(!over_grid(&layout, point(px(9.), px(30.))));
    }

    #[test]
    fn a_hovered_link_spans_its_cells() {
        let hovered = HoveredLink {
            start: GridPoint::new(2, 70),
            end: GridPoint::new(3, 4),
            link: TerminalLink::Url("https://example.com".into()),
        };
        assert!(hovered.contains(GridPoint::new(2, 79)));
        assert!(hovered.contains(GridPoint::new(3, 0)));
        assert!(!hovered.contains(GridPoint::new(3, 5)));
        assert!(!hovered.contains(GridPoint::new(2, 69)));
    }
}
