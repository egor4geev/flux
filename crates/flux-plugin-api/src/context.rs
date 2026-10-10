//! What a command acts on: [`CommandContext`] — where it was run from (the palette, its keys, a
//! context menu, a notification, a status bar item), the document and its selections, the files.

use crate::{CommandContext, CommandSource, Range};

impl CommandContext {
    /// The primary selection of the document; none without a document. An empty range is a
    /// cursor.
    pub fn selection(&self) -> Option<Range> {
        self.editor.as_ref()?;
        self.selections.first().copied()
    }

    /// The text of the primary selection of the document (as it is now); none without a document,
    /// empty for a cursor.
    pub fn selected_text(&self) -> Option<String> {
        let editor = self.editor.as_ref()?;
        let selection = self.selection()?;
        let text = crate::host::editors::text(editor.id)?;
        Some(crate::slice(&text, selection).to_string())
    }

    /// The first of the paths: the file (or the folder) the command acts on.
    pub fn path(&self) -> Option<&str> {
        self.paths.first().map(String::as_str)
    }

    /// Run from a context menu (the editor's, the tree's, a tab's).
    pub fn from_menu(&self) -> bool {
        matches!(
            self.source,
            CommandSource::EditorMenu | CommandSource::TreeMenu | CommandSource::TabMenu
        )
    }
}

impl CommandSource {
    /// The source as the API names it: "palette", "keys", "editor-menu", "tree-menu", "tab-menu",
    /// "notification", "status-bar" (`Debug` gives the Rust path).
    pub fn name(self) -> &'static str {
        match self {
            CommandSource::Palette => "palette",
            CommandSource::Keys => "keys",
            CommandSource::EditorMenu => "editor-menu",
            CommandSource::TreeMenu => "tree-menu",
            CommandSource::TabMenu => "tab-menu",
            CommandSource::Notification => "notification",
            CommandSource::StatusBar => "status-bar",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditorInfo, Position};

    fn context(source: CommandSource, editor: bool) -> CommandContext {
        CommandContext {
            source,
            editor: editor.then(|| EditorInfo {
                id: 1,
                path: Some("/p/main.rs".into()),
                language: "Rust".into(),
                modified: false,
            }),
            selections: vec![Range::new(Position::new(1, 0), Position::new(1, 4))],
            paths: vec!["/p/main.rs".into()],
        }
    }

    #[test]
    fn reads_the_context() {
        let menu = context(CommandSource::TreeMenu, false);
        assert!(menu.from_menu());
        assert_eq!(menu.path(), Some("/p/main.rs"));
        assert_eq!(menu.selection(), None);
        let palette = context(CommandSource::Palette, true);
        assert!(!palette.from_menu());
        assert_eq!(palette.source.name(), "palette");
        assert_eq!(CommandSource::TreeMenu.name(), "tree-menu");
        assert_eq!(palette.selection().map(|range| range.end.column), Some(4));
    }
}
