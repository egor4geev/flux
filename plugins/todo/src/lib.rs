//! The TODO tool window, a plugin bundled into Flux (`flux.todo`): the TODO and FIXME comments of
//! the project, as in JetBrains IDEs — grouped by file, with a jump to the place. The host searches
//! the project (`project.search`, the rules of Find in Files), so the plugin needs no permission to
//! read files. The results follow saves, the settings and the project; while the window is hidden
//! nothing is searched, and a stale result is refreshed when it shows again.

mod model;

use flux_plugin_api::host::project::{self, Query};
use flux_plugin_api::host::{editors, ui};
use flux_plugin_api::view::*;
use flux_plugin_api::{Event, Plugin, Position, Range, UiEvent, log, register_plugin};
use flux_plugin_api::{notify, setting, tr, trf};

use model::File;

/// The tool window of the manifest.
const WINDOW: &str = "todo";
/// More matches than this are not shown: the summary says so.
const MAX_MATCHES: u32 = 5000;

/// Which TODO items the window shows, as the tabs of the JetBrains TODO window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Project,
    CurrentFile,
}

/// The last search.
enum Scan {
    /// Not searched yet.
    None,
    /// The search runs: the window shows a progress bar meanwhile.
    Running,
    Done {
        files: Vec<File>,
        truncated: bool,
    },
    /// Why there is nothing to show: no project, no patterns, an invalid pattern.
    Failed(String),
}

struct Todo {
    scope: Scope,
    scan: Scan,
    /// The result is out of date (a save, new settings): search again when the window shows.
    stale: bool,
    shown: bool,
    root: Option<String>,
    /// The active document's absolute path: the Current File scope.
    active: Option<String>,
    /// Expand All and Collapse All give the tree a new id, so the rows' initial expansion applies
    /// again instead of the user's.
    generation: u32,
    /// Whether file rows start expanded.
    expanded: bool,
}

impl Plugin for Todo {
    fn new() -> Self {
        Todo {
            scope: Scope::Project,
            scan: Scan::None,
            stale: true,
            shown: false,
            root: None,
            active: None,
            generation: 0,
            expanded: true,
        }
    }

    fn activate(&mut self) {
        self.root = project::root();
        self.active = editors::active().and_then(|editor| editor.path);
        self.render();
    }

    fn run_command(&mut self, command: &str) {
        if command == "refresh" {
            self.stale = true;
            if self.shown {
                self.refresh();
            } else {
                // `tool-window-shown` comes next and finds the result stale.
                ui::show(WINDOW);
            }
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::ToolWindowShown(window) if window == WINDOW => {
                self.shown = true;
                if self.stale {
                    self.refresh();
                }
            }
            Event::ToolWindowHidden(window) if window == WINDOW => self.shown = false,
            Event::EditorSaved(_) | Event::SettingsChanged => self.invalidate(),
            Event::ProjectChanged(root) => {
                self.root = root;
                self.invalidate();
            }
            Event::ActiveEditorChanged(editor) => {
                self.active = editor.and_then(|editor| editor.path);
                if self.scope == Scope::CurrentFile {
                    self.render();
                }
            }
            Event::Ui(input) if input.window == WINDOW => self.on_ui(&input.element, input.event),
            _ => {}
        }
    }
}

impl Todo {
    /// Something changed what the search finds: search now if the window shows, later otherwise.
    fn invalidate(&mut self) {
        self.stale = true;
        if self.shown {
            self.refresh();
        }
    }

    /// Searches the project again.
    fn refresh(&mut self) {
        self.stale = false;
        self.scan = Scan::Running;
        self.render();
        self.scan = self.search();
        self.render();
    }

    fn search(&self) -> Scan {
        if self.root.is_none() {
            return Scan::Failed(tr("Open a folder to see its TODO items"));
        }
        let patterns: Vec<String> = setting("patterns").unwrap_or_default();
        let Some(text) = model::pattern(&patterns) else {
            return Scan::Failed(tr("No patterns: add them in Settings → TODO"));
        };
        let query = Query {
            text,
            case_sensitive: setting("case-sensitive").unwrap_or(false),
            whole_word: false,
            regex: true,
        };
        match project::search(&query, MAX_MATCHES) {
            Ok(result) => Scan::Done {
                files: model::group(
                    result
                        .files
                        .into_iter()
                        .map(|file| model::FileLines {
                            path: file.path,
                            lines: file
                                .lines
                                .into_iter()
                                .map(|line| model::Line {
                                    line: line.line,
                                    text: line.text,
                                    column_offset: line.column_offset,
                                    ranges: line.ranges,
                                })
                                .collect(),
                        })
                        .collect(),
                ),
                truncated: result.truncated,
            },
            Err(error) => {
                log::warn(&format!("search failed: {error}"));
                // flux-search names a mistake in a pattern this way; other errors are said as is.
                Scan::Failed(match error.strip_prefix("regex parse error: ") {
                    Some(reason) => trf("Invalid pattern: {0}", &[&reason]),
                    None => error,
                })
            }
        }
    }

    fn on_ui(&mut self, element: &str, event: UiEvent) {
        match (element, event) {
            ("refresh", UiEvent::Clicked) => self.refresh(),
            ("expand-all", UiEvent::Clicked) => self.expand_all(true),
            ("collapse-all", UiEvent::Clicked) => self.expand_all(false),
            ("scope-project", UiEvent::Clicked) => self.set_scope(Scope::Project),
            ("scope-file", UiEvent::Clicked) => self.set_scope(Scope::CurrentFile),
            (tree, UiEvent::Activated(key)) if tree.starts_with("items") => self.open(&key),
            _ => {}
        }
    }

    fn expand_all(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.generation += 1;
        self.render();
    }

    fn set_scope(&mut self, scope: Scope) {
        if self.scope != scope {
            self.scope = scope;
            self.render();
        }
    }

    /// Opens the place of a row: an item selects its match, a file opens as it was.
    fn open(&self, key: &str) {
        let (Some(root), Scan::Done { files, .. }) = (&self.root, &self.scan) else {
            return;
        };
        for file in files {
            let selection = if key == model::file_key(&file.path) {
                None
            } else if let Some(item) = file
                .items
                .iter()
                .find(|item| key == model::item_key(&file.path, item))
            {
                Some(Range {
                    start: Position {
                        line: item.line,
                        column: item.start,
                    },
                    end: Position {
                        line: item.line,
                        column: item.end,
                    },
                })
            } else {
                continue;
            };
            let path = model::absolute(root, &file.path);
            if let Err(error) = editors::open(&path, selection) {
                notify::error(&trf("Couldn't open {0}", &[&file.path]), Some(&error));
            }
            return;
        }
    }

    /// The files the scope shows, or why there are none.
    fn visible(&self) -> Result<Vec<&File>, String> {
        let Scan::Done { files, .. } = &self.scan else {
            return Ok(Vec::new());
        };
        match self.scope {
            Scope::Project => Ok(files.iter().collect()),
            Scope::CurrentFile => {
                let Some(active) = &self.active else {
                    return Err(tr("No file is open"));
                };
                let Some(path) = self
                    .root
                    .as_deref()
                    .and_then(|root| model::relative(root, active))
                else {
                    return Err(tr("The file is not in the project"));
                };
                Ok(files.iter().filter(|file| file.path == path).collect())
            }
        }
    }

    fn render(&self) {
        let toolbar = toolbar(
            "toolbar",
            [
                icon_button("refresh", "refresh", &tr("Refresh")),
                icon_button("expand-all", "expand-all", &tr("Expand All")),
                icon_button("collapse-all", "collapse-all", &tr("Collapse All")),
            ],
        );
        let scope = |id: &str, label: &str, scope: Scope| {
            let button = button(id, label);
            if self.scope == scope {
                button.primary().into()
            } else {
                button.into()
            }
        };
        let mut summary = String::new();
        let body = match &self.scan {
            Scan::Running => column(
                "searching",
                [
                    progress("progress", None),
                    text("searching-text", [span(&tr("Searching…")).tone(Tone::Dim)]),
                ],
            ),
            _ => {
                let mut tree = Tree::new();
                let visible = self.visible();
                let empty = match (&self.scan, &visible) {
                    (Scan::Failed(reason), _) | (_, Err(reason)) => reason.clone(),
                    (Scan::None, _) => String::new(),
                    (_, Ok(files)) => {
                        for file in files {
                            add_file(&mut tree, file, self.expanded);
                        }
                        let (items, count) = model::count(files.iter().copied());
                        summary = trf("TODO: {0} · files: {1}", &[&items, &count]);
                        if let Scan::Done {
                            truncated: true, ..
                        } = &self.scan
                        {
                            summary.push_str(" · ");
                            summary.push_str(&trf("Showing the first {0}", &[&MAX_MATCHES]));
                        }
                        match self.scope {
                            Scope::Project => tr("No TODO items"),
                            Scope::CurrentFile => tr("No TODO items in this file"),
                        }
                    }
                };
                tree.empty_text(&empty)
                    .into_element(&format!("items-{}", self.generation))
            }
        };
        let mut elements = vec![
            toolbar,
            row(
                "scope",
                [
                    scope("scope-project", &tr("Project"), Scope::Project),
                    scope("scope-file", &tr("Current File"), Scope::CurrentFile),
                ],
            ),
        ];
        // On a line of its own: next to the scope buttons it doesn't fit a narrow island.
        if !summary.is_empty() {
            elements.push(text("summary", [span(&summary).tone(Tone::Dim)]));
        }
        elements.push(body);
        let view = column("root", elements).into_view();
        ui::set_view(WINDOW, &view);
    }
}

/// A file's row and the rows of its items.
fn add_file(tree: &mut Tree, file: &File, expanded: bool) {
    let (name, folder) = model::split_path(&file.path);
    let mut row = RowSpec::new(&model::file_key(&file.path), name)
        .icon(&format!("file:{name}"))
        .badge(&file.items.len().to_string());
    if !folder.is_empty() {
        row = row.detail(folder);
    }
    if expanded {
        row = row.expanded();
    }
    let parent = tree.add(row);
    for item in &file.items {
        let label = model::pieces(item)
            .into_iter()
            .map(|(text, highlighted)| {
                let piece = span(&text);
                if highlighted {
                    piece.highlight()
                } else {
                    piece
                }
            })
            .collect();
        tree.add_child(
            parent,
            RowSpec::spans(&model::item_key(&file.path, item), label)
                .detail(&trf("line {0}", &[&(item.line + 1)])),
        );
    }
}

register_plugin!(Todo);
