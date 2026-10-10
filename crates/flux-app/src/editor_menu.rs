//! The editor's context menu (part 9.2): the right button over the text, ⇧F10 — the items of
//! JetBrains IDEs in their order (Show Context Actions, the clipboard, Copy Path/Reference ▸, Find
//! Usages, Refactor ▸, Go To ▸, Reformat Code, Claude ▸, Git ▸, Open In ▸), each with its
//! shortcut. Over the gutter the right button keeps its own menu (Annotate, [`crate::blame`]).
//!
//! The right button moves the caret to the click unless it lands in the selection (the menu acts
//! on the selection then), as in JetBrains IDEs. A commit message and other fields without a file
//! get only the clipboard.

use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext, ClipboardItem, Context, DismissEvent, Div, Entity, Focusable, InteractiveElement,
    KeyBinding, MouseDownEvent, Pixels, Point, Subscription, Window, actions, point,
};

use crate::claude_actions::{self, Ask};
use crate::context_menu::ContextMenu;
use crate::editor::{self, Editor};
use crate::i18n::tr;
use crate::workspace::Workspace;

actions!(
    editor_menu,
    [
        /// ⇧F10: the context menu at the caret.
        ShowContextMenu,
        /// The active file's absolute path to the clipboard.
        CopyAbsolutePath,
        /// The active file's path from the project root.
        CopyPathFromRoot,
        /// The active file's name.
        CopyFileName,
        /// `src/main.rs:12`: the active file and the caret's line.
        CopyReference,
        /// The active file in the Finder.
        RevealInFinder,
        /// A new terminal in the active file's folder.
        OpenInTerminal,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "shift-f10",
        ShowContextMenu,
        Some("Editor"),
    )]);
}

/// The open menu of an editor (`Editor::menu`).
#[derive(Default)]
pub struct MenuState {
    open: Option<OpenMenu>,
}

struct OpenMenu {
    menu: Entity<ContextMenu>,
    /// Window coordinates.
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

/// What the menu depends on.
#[derive(Debug, Clone, Default, PartialEq)]
struct Facts {
    selection: bool,
    read_only: bool,
    /// A field without a file: a commit message.
    message: bool,
    file: bool,
    claude: bool,
    /// Problems in the selection or on the caret's line: Fix with Claude.
    problems: bool,
    /// The file is in a repository, with a version in HEAD.
    tracked: bool,
    /// A change against HEAD on the caret's line: Rollback Lines.
    change_at_caret: bool,
}

/// The menu's groups, separated, in the order of JetBrains IDEs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    ContextActions,
    Clipboard,
    CopyPath,
    Code,
    Reformat,
    Claude,
    Git,
    OpenIn,
}

fn groups(facts: &Facts) -> Vec<Group> {
    if facts.message || !facts.file {
        return vec![Group::Clipboard];
    }
    let mut groups = Vec::new();
    if !facts.read_only {
        groups.push(Group::ContextActions);
    }
    groups.extend([Group::Clipboard, Group::CopyPath, Group::Code]);
    if !facts.read_only {
        groups.push(Group::Reformat);
    }
    if facts.claude {
        groups.push(Group::Claude);
    }
    if facts.tracked {
        groups.push(Group::Git);
    }
    groups.push(Group::OpenIn);
    groups
}

fn facts(editor: &Editor, cx: &App) -> Facts {
    let text = editor.document.text();
    let head = editor.document.selection().primary().head.min(text.len_chars());
    let line = text.char_to_line(head) as u32;
    let file = editor.document.path().is_some();
    Facts {
        selection: editor
            .document
            .selection()
            .ranges()
            .iter()
            .any(|range| range.from() != range.to()),
        read_only: editor.read_only,
        message: editor.message.is_some(),
        file,
        claude: file && claude_actions::offered(cx),
        problems: !claude_actions::problems_here(editor).is_empty(),
        tracked: editor.git.base.is_some(),
        change_at_caret: editor.git.hunks.iter().any(|hunk| {
            hunk.new.contains(&line) || (hunk.new.is_empty() && hunk.new.start == line)
        }),
    }
}

fn build(mut menu: ContextMenu, facts: &Facts) -> ContextMenu {
    for group in groups(facts) {
        menu = match group {
            Group::ContextActions => menu.entry(
                tr("Show Context Actions"),
                crate::code_actions::ShowContextActions,
            ),
            Group::Clipboard => {
                if facts.selection && !facts.read_only {
                    menu = menu.entry(tr("Cut"), editor::Cut);
                }
                if facts.selection {
                    menu = menu.entry(tr("Copy"), editor::Copy);
                }
                if !facts.read_only {
                    menu = menu.entry(tr("Paste"), editor::Paste);
                }
                menu
            }
            Group::CopyPath => menu.submenu(tr("Copy Path/Reference"), |copy| {
                copy.entry(tr("Absolute Path"), CopyAbsolutePath)
                    .entry(tr("Path From Project Root"), CopyPathFromRoot)
                    .entry(tr("File Name"), CopyFileName)
                    .separator()
                    .entry(tr("Reference"), CopyReference)
            }),
            Group::Code => {
                menu = menu.entry(tr("Find Usages"), crate::navigation::FindUsages);
                if !facts.read_only {
                    menu = menu.submenu(tr("Refactor"), |refactor| {
                        refactor.entry(tr("Rename…"), crate::navigation::RenameSymbol)
                    });
                }
                menu.submenu(tr("Go To"), |go| {
                    go.entry(
                        tr("Declaration or Usages"),
                        crate::navigation::GoToDefinition,
                    )
                    .entry(tr("Line…"), crate::go_to_line::Toggle)
                    .separator()
                    .entry(tr("Back"), crate::navigation::NavigateBack)
                    .entry(tr("Forward"), crate::navigation::NavigateForward)
                })
            }
            Group::Reformat => menu.entry(tr("Reformat Code"), crate::navigation::ReformatCode),
            Group::Claude => menu.submenu("Claude", |claude| {
                let send = if facts.selection {
                    tr("Send Selection to Claude")
                } else {
                    tr("Send File to Claude")
                };
                let mut claude = claude
                    .entry(send, crate::claude::AddSelectionToClaude)
                    .separator();
                for ask in [
                    Ask::Explain,
                    Ask::Fix,
                    Ask::FindProblems,
                    Ask::WriteTests,
                    Ask::Document,
                ] {
                    let enabled = ask != Ask::Fix || facts.problems;
                    claude = claude.boxed_entry_if(
                        enabled,
                        claude_actions::label_of(ask),
                        claude_actions::action_of(ask),
                    );
                }
                claude
            }),
            Group::Git => menu.submenu("Git", |git| {
                git.entry(tr("Annotate with Git Blame"), crate::git::Annotate)
                    .entry(tr("Show Diff"), crate::git::ShowDiff)
                    .separator()
                    .entry(tr("Show History"), crate::git::ShowFileHistory)
                    .entry_if(
                        facts.selection,
                        tr("Show History for Selection"),
                        crate::git::ShowSelectionHistory,
                    )
                    .separator()
                    .entry_if(
                        facts.change_at_caret && !facts.read_only,
                        tr("Rollback Lines"),
                        crate::git_gutter::RollbackLines,
                    )
            }),
            Group::OpenIn => menu.submenu(tr("Open In"), |open| {
                open.entry(tr("Finder"), RevealInFinder)
                    .entry(tr("Terminal"), OpenInTerminal)
            }),
        };
        // Groups as JetBrains separates them: the clipboard with Copy Path, the code actions with
        // Reformat Code, Git with Open In.
        let separated = match group {
            Group::Clipboard | Group::Git => false,
            Group::Code => facts.read_only,
            _ => true,
        };
        if separated {
            menu = menu.separator();
        }
    }
    menu
}

/// The right button in the editor: the gutter's menu or the text's.
pub fn secondary_click(
    editor: &mut Editor,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let Some(layout) = editor.layout.as_ref() else {
        return;
    };
    if event.position.x < layout.text_bounds.left() {
        return crate::blame::secondary_click(editor, event, window, cx);
    }
    let position = layout.position_for_point(editor.document.text(), event.position);
    let in_selection = editor
        .document
        .selection()
        .ranges()
        .iter()
        .any(|range| range.from() != range.to() && (range.from()..=range.to()).contains(&position));
    window.focus(&editor.focus_handle);
    if !in_selection {
        editor.select_range(position..position, cx);
    }
    open(editor, event.position, window, cx);
}

/// ⇧F10: the menu under the caret.
fn show_at_caret(editor: &mut Editor, window: &mut Window, cx: &mut Context<Editor>) {
    let head = editor.document.selection().primary().head;
    let position = match crate::popup::anchor(editor, head) {
        Some(anchor) => point(anchor.x, anchor.line_bottom),
        None => match editor.layout.as_ref() {
            Some(layout) => layout.text_bounds.origin,
            None => return,
        },
    };
    open(editor, position, window, cx);
}

fn open(editor: &mut Editor, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Editor>) {
    let facts = facts(editor, cx);
    let menu = cx.new(|cx| build(ContextMenu::new(window, cx), &facts));
    let focus = menu.focus_handle(cx);
    let subscriptions = [
        cx.subscribe_in(
            &menu,
            window,
            |editor, menu, _: &DismissEvent, window, cx| close(editor, menu, window, cx),
        ),
        cx.on_focus_out(&focus, window, {
            let menu = menu.clone();
            move |editor, _, window, cx| close(editor, &menu, window, cx)
        }),
    ];
    window.focus(&focus);
    editor.menu.open = Some(OpenMenu {
        menu,
        position,
        _subscriptions: subscriptions,
    });
    cx.notify();
}

/// Closes the menu (if it is still the same one); Esc in the menu returns focus to the editor.
fn close(editor: &mut Editor, menu: &Entity<ContextMenu>, window: &mut Window, cx: &mut Context<Editor>) {
    if editor
        .menu
        .open
        .as_ref()
        .is_none_or(|open| open.menu != *menu)
    {
        return;
    }
    let had_focus = menu.focus_handle(cx).contains_focused(window, cx);
    editor.menu.open = None;
    if had_focus {
        window.focus(&editor.focus_handle);
    }
    cx.notify();
}

/// The editor's handlers: ⇧F10 (a preview never has the focus).
pub fn actions(root: Div, _editor: &Editor, cx: &mut Context<Editor>) -> Div {
    root.on_action(cx.listener(|editor, _: &ShowContextMenu, window, cx| {
        show_at_caret(editor, window, cx)
    }))
}

/// The open menu over the window.
pub fn render(editor: &Editor) -> Option<gpui::AnyElement> {
    let open = editor.menu.open.as_ref()?;
    Some(ContextMenu::overlay(&open.menu, open.position))
}

/// The window's handlers of the menu's file items: they act on the active document.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|this, _: &CopyAbsolutePath, _, cx| {
        copy(this, cx, |path, _, _| Some(path.to_string_lossy().into_owned()))
    }))
    .on_action(cx.listener(|this, _: &CopyPathFromRoot, _, cx| {
        copy(this, cx, |path, root, _| {
            Some(claude_actions::cli_path(path, root))
        })
    }))
    .on_action(cx.listener(|this, _: &CopyFileName, _, cx| {
        copy(this, cx, |path, _, _| {
            Some(path.file_name()?.to_string_lossy().into_owned())
        })
    }))
    .on_action(cx.listener(|this, _: &CopyReference, _, cx| {
        copy(this, cx, |path, root, line| {
            Some(reference(path, root, line?))
        })
    }))
    .on_action(cx.listener(|this, _: &RevealInFinder, _, cx| {
        if let Some(path) = this.active_path(cx) {
            cx.reveal_path(&path);
        }
    }))
    .on_action(cx.listener(|this, _: &OpenInTerminal, window, cx| {
        if let Some(dir) = this.active_path(cx).and_then(|path| Some(path.parent()?.to_path_buf())) {
            this.open_terminal_in(dir, window, cx);
        }
    }))
}

/// `src/main.rs:12`: the file as from the project root and the line (1-based).
fn reference(path: &Path, root: Option<&Path>, line: usize) -> String {
    format!("{}:{line}", claude_actions::cli_path(path, root))
}

/// Copies what `text` makes of the active document's path, the project root and the caret's line.
fn copy(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
    text: impl FnOnce(&Path, Option<&Path>, Option<usize>) -> Option<String>,
) {
    let Some(path) = workspace.active_path(cx) else {
        return;
    };
    let line = workspace.active_editor().map(|editor| {
        let editor = editor.read(cx);
        let text = editor.document.text();
        let head = editor.document.selection().primary().head.min(text.len_chars());
        text.char_to_line(head) + 1
    });
    let root: Option<PathBuf> = workspace.root().map(Path::to_path_buf);
    if let Some(text) = text(&path, root.as_deref(), line) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_gets_every_group_in_the_order_of_jetbrains() {
        let file = Facts {
            file: true,
            claude: true,
            tracked: true,
            ..Facts::default()
        };
        assert_eq!(
            groups(&file),
            [
                Group::ContextActions,
                Group::Clipboard,
                Group::CopyPath,
                Group::Code,
                Group::Reformat,
                Group::Claude,
                Group::Git,
                Group::OpenIn,
            ]
        );
    }

    #[test]
    fn read_only_untracked_and_message_editors_get_what_applies() {
        let read_only = Facts {
            file: true,
            read_only: true,
            ..Facts::default()
        };
        assert_eq!(
            groups(&read_only),
            [Group::Clipboard, Group::CopyPath, Group::Code, Group::OpenIn]
        );
        let message = Facts {
            message: true,
            ..Facts::default()
        };
        assert_eq!(groups(&message), [Group::Clipboard]);
        let untitled = Facts::default();
        assert_eq!(groups(&untitled), [Group::Clipboard]);
    }

    #[test]
    fn references_are_relative_with_the_line() {
        assert_eq!(
            reference(Path::new("/p/src/a.rs"), Some(Path::new("/p")), 12),
            "src/a.rs:12"
        );
    }
}
