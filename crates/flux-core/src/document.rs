//! Документ: текст, выделение, история и связь с файлом на диске.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ropey::Rope;

use crate::history::{History, Revision};
use crate::selection::Selection;
use crate::text::detect_line_ending;
use crate::transaction::{ChangeSet, Transaction};

/// Правки одного вида, сделанные быстро и подряд, отменяются вместе.
const COALESCE_WINDOW: Duration = Duration::from_secs(1);

/// Вид правки — по нему решаем, склеивать ли её с предыдущей в истории.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    /// Никогда не склеивается: вставка из буфера, перевод строки и т.п.
    Other,
}

/// Изменение текста документа — для тех, кто следит за текстом со стороны
/// (подсветка синтаксиса, в будущем LSP): `changes`, применённый к `old_text`,
/// даёт текст после изменения.
#[derive(Debug, Clone)]
pub struct TextChange {
    /// Текст до изменения (клон rope — O(1)).
    pub old_text: Rope,
    pub changes: ChangeSet,
}

#[derive(Debug)]
struct LastEdit {
    kind: EditKind,
    at: Instant,
    selection_after: Selection,
}

#[derive(Debug)]
pub struct Document {
    text: Rope,
    selection: Selection,
    history: History,
    path: Option<PathBuf>,
    line_ending: &'static str,
    saved_state: u64,
    last_edit: Option<LastEdit>,
}

impl Document {
    pub fn from_text(text: &str) -> Self {
        Self::new(Rope::from_str(text), None)
    }

    /// Открывает файл. Несуществующий файл — пустой документ, который
    /// создастся при сохранении.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let text = match File::open(path) {
            Ok(file) => Rope::from_reader(BufReader::new(file))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Rope::new(),
            Err(err) => return Err(err),
        };
        Ok(Self::new(text, Some(path.to_path_buf())))
    }

    fn new(text: Rope, path: Option<PathBuf>) -> Self {
        Self {
            line_ending: detect_line_ending(&text),
            text,
            selection: Selection::point(0),
            history: History::default(),
            path,
            saved_state: 0,
            last_edit: None,
        }
    }

    pub fn text(&self) -> &Rope {
        &self.text
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Привязывает документ к файлу (для «Сохранить как»).
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub fn line_ending(&self) -> &'static str {
        self.line_ending
    }

    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned())
    }

    pub fn is_modified(&self) -> bool {
        self.history.state_id() != self.saved_state
    }

    pub fn set_selection(&mut self, selection: Selection) {
        let len = self.text.len_chars();
        self.selection = selection.transform(|range| {
            let mut range = *range;
            range.anchor = range.anchor.min(len);
            range.head = range.head.min(len);
            range
        });
    }

    /// Применяет правку и записывает её в историю. `None` — текст не изменился
    /// (правка двигала только выделение).
    pub fn apply(&mut self, transaction: Transaction, kind: EditKind) -> Option<TextChange> {
        let selection_before = self.selection.clone();
        let Transaction { changes, selection } = transaction;
        let selection_after = selection.unwrap_or_else(|| selection_before.map(&changes));

        if changes.is_empty() {
            self.selection = selection_after;
            return None;
        }

        let inversion = Transaction::new(changes.invert(&self.text))
            .with_selection(selection_before.clone());
        let change = TextChange {
            old_text: self.text.clone(),
            changes: changes.clone(),
        };
        changes.apply(&mut self.text);
        self.selection = selection_after.clone();

        let now = Instant::now();
        let coalesce = kind != EditKind::Other
            && self.last_edit.as_ref().is_some_and(|last| {
                last.kind == kind
                    && now.duration_since(last.at) < COALESCE_WINDOW
                    && last.selection_after == selection_before
            });
        self.history.commit(
            Revision {
                transaction: Transaction::new(changes).with_selection(selection_after.clone()),
                inversion,
            },
            coalesce,
        );
        self.last_edit = Some(LastEdit {
            kind,
            at: now,
            selection_after,
        });
        Some(change)
    }

    /// Отменяет последнюю группу правок. Изменения текста — по одному на ревизию,
    /// в порядке применения. `None` — отменять нечего.
    pub fn undo(&mut self) -> Option<Vec<TextChange>> {
        let txs = self.history.undo()?;
        Some(self.replay(txs))
    }

    /// Повторяет отменённую группу правок; `None` — повторять нечего.
    pub fn redo(&mut self) -> Option<Vec<TextChange>> {
        let txs = self.history.redo()?;
        Some(self.replay(txs))
    }

    fn replay(&mut self, txs: Vec<Transaction>) -> Vec<TextChange> {
        let mut changes = Vec::with_capacity(txs.len());
        for tx in txs {
            if !tx.changes.is_empty() {
                let old_text = self.text.clone();
                tx.changes.apply(&mut self.text);
                changes.push(TextChange {
                    old_text,
                    changes: tx.changes,
                });
            }
            if let Some(selection) = tx.selection {
                self.selection = selection;
            }
        }
        self.last_edit = None;
        changes
    }

    /// Сохраняет атомарно: пишет во временный файл рядом и переименовывает.
    /// При сбое посреди записи старый файл остаётся целым.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::other("document has no path"))?;
        // Пишем в файл, на который указывает симлинк, а не поверх самого симлинка.
        let target = fs::canonicalize(&path).unwrap_or(path);
        let permissions = fs::metadata(&target).ok().map(|m| m.permissions());

        let tmp = target.with_file_name(format!(
            ".{}.flux-tmp",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
        let result = (|| {
            let mut writer = BufWriter::new(File::create(&tmp)?);
            self.text.write_to(&mut writer)?;
            writer.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            if let Some(permissions) = permissions {
                fs::set_permissions(&tmp, permissions)?;
            }
            fs::rename(&tmp, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;

        self.saved_state = self.history.state_id();
        self.last_edit = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit;
    use crate::selection::Range;

    fn type_str(doc: &mut Document, s: &str) {
        for c in s.chars() {
            let tx = edit::insert_text(doc.text(), doc.selection(), &c.to_string());
            doc.apply(tx, EditKind::Insert);
        }
    }

    /// `changes` по очереди от `old_text` первого шага дают текст `after`,
    /// а `old_text` каждого шага — текст перед этим шагом.
    fn check_changes(before: &Rope, changes: &[TextChange], after: &Rope) {
        let mut text = before.clone();
        for change in changes {
            assert_eq!(change.old_text, text);
            change.changes.apply(&mut text);
        }
        assert_eq!(&text, after);
    }

    #[test]
    fn text_changes_describe_edit_undo_and_redo() {
        let mut doc = Document::from_text("один\nдва");
        let before = doc.text().clone();
        let cursors = Selection::new(vec![Range::point(0), Range::point(5)], 0);
        let change = doc.apply(
            edit::insert_text(doc.text(), &cursors, "- "),
            EditKind::Other,
        );
        check_changes(&before, &[change.unwrap()], doc.text());
        assert_eq!(doc.text(), "- один\n- два");

        // Правка одного выделения текст не меняет.
        let only_selection = Transaction::new(ChangeSet::identity(doc.text().len_chars()))
            .with_selection(Selection::point(0));
        assert!(doc.apply(only_selection, EditKind::Other).is_none());

        // Группа undo из нескольких ревизий: подряд набранные символы.
        type_str(&mut doc, "abc");
        let typed = doc.text().clone();
        let changes = doc.undo().unwrap();
        assert_eq!(changes.len(), 3);
        check_changes(&typed, &changes, doc.text());
        assert_eq!(doc.text(), "- один\n- два");

        let undone = doc.text().clone();
        let changes = doc.redo().unwrap();
        assert_eq!(changes.len(), 3);
        check_changes(&undone, &changes, doc.text());
        assert_eq!(doc.text(), &typed);
        assert!(doc.redo().is_none());

        while doc.undo().is_some() {}
        assert_eq!(doc.text(), "один\nдва");
        assert!(doc.undo().is_none());
    }

    #[test]
    fn undo_groups_typing() {
        let mut doc = Document::from_text("");
        type_str(&mut doc, "hello");
        assert_eq!(doc.text(), "hello");
        assert!(doc.undo().is_some());
        assert_eq!(doc.text(), "");
        assert_eq!(doc.selection().primary().head, 0);
        assert!(doc.redo().is_some());
        assert_eq!(doc.text(), "hello");
        assert_eq!(doc.selection().primary().head, 5);
    }

    #[test]
    fn cursor_move_breaks_undo_group() {
        let mut doc = Document::from_text("");
        type_str(&mut doc, "ab");
        doc.set_selection(Selection::point(0));
        type_str(&mut doc, "x");
        assert_eq!(doc.text(), "xab");
        doc.undo();
        assert_eq!(doc.text(), "ab");
        doc.undo();
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut doc = Document::from_text("");
        type_str(&mut doc, "a");
        doc.undo();
        type_str(&mut doc, "b");
        assert!(doc.redo().is_none());
        assert_eq!(doc.text(), "b");
    }

    #[test]
    fn modified_tracks_saved_state() {
        let dir = std::env::temp_dir().join(format!("flux-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.txt");
        fs::write(&path, "one\r\ntwo\r\n").unwrap();

        let mut doc = Document::open(&path).unwrap();
        assert_eq!(doc.line_ending(), "\r\n");
        assert!(!doc.is_modified());

        type_str(&mut doc, "x");
        assert!(doc.is_modified());
        doc.save().unwrap();
        assert!(!doc.is_modified());
        assert_eq!(fs::read_to_string(&path).unwrap(), "xone\r\ntwo\r\n");

        // Сохранение закрывает группу undo: "y" отменяется отдельно от "x".
        type_str(&mut doc, "y");
        assert!(doc.is_modified());
        doc.undo();
        assert!(!doc.is_modified(), "back to exactly the saved state");
        doc.undo();
        assert!(doc.is_modified(), "undo past the saved state");
        doc.redo();
        doc.redo();
        assert_eq!(doc.text(), "xyone\r\ntwo\r\n");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_missing_file_is_empty() {
        let doc = Document::open("/definitely/not/here.rs").unwrap();
        assert_eq!(doc.text().len_chars(), 0);
        assert_eq!(doc.display_name(), "here.rs");
    }
}
