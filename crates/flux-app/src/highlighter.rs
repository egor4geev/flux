//! Подсветка документа редактора: дерево разбора, его разбор в фоне и
//! отображение capture на области темы.
//!
//! Цикл разбора ([`parse`]):
//! - после правки — сначала синхронно, с бюджетом [`SYNC_PARSE_BUDGET`]: обычно
//!   успевает, и кадр сразу рисуется со свежей подсветкой;
//! - не успел, или это открытие файла, смена языка, повтор — разбор уходит в
//!   фоновый executor; по готовности `finish`, повтор, если за это время были
//!   правки, и перерисовка;
//! - до готовности разбора подсветка берётся по старому дереву, сдвинутому
//!   правками (`Syntax::edit`), — без мелькания.
//!
//! Запрос подсветки компилируется в фоне (`ParseJob::run`), поэтому
//! [`HighlightMap`] строится только после фонового разбора, через `try_new`.

use std::ops::Range;
use std::path::Path;
use std::time::Duration;

use flux_core::{Rope, TextChange};
use flux_syntax::{HighlightMap, HighlightSpan, Language, Syntax, language_for_path};
use gpui::{AppContext, Context, Task};

use crate::editor::Editor;
use crate::theme::Theme;

/// Файлы больше этого — без подсветки: время разбора и память дерева растут
/// с размером файла.
pub const MAX_HIGHLIGHT_BYTES: usize = 10 * 1024 * 1024;

/// Сколько разбор может занять в UI-потоке сразу после правки.
pub const SYNC_PARSE_BUDGET: Duration = Duration::from_millis(1);

/// Когда запускается разбор.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Сразу после правки: сначала синхронно с бюджетом, не успел — в фоне.
    AfterEdit,
    /// Открытие файла, смена языка, повтор после фонового разбора: только в фоне.
    Background,
}

pub struct Highlighter {
    /// Язык по пути файла — даже если подсветка выключена из-за размера.
    language: Option<&'static Language>,
    syntax: Option<Syntax>,
    /// capture → область текущей темы; появляется после первого фонового разбора.
    map: Option<HighlightMap>,
    /// Фоновый разбор. Сброс отменяет его: вкладку закрыли или сменился язык.
    parse_task: Option<Task<()>>,
}

impl Highlighter {
    /// Язык — по пути; безымянный документ и файлы больше
    /// [`MAX_HIGHLIGHT_BYTES`] — без подсветки.
    pub fn new(path: Option<&Path>, text: &Rope) -> Self {
        let language = path.and_then(language_for_path);
        let syntax = language
            .filter(|_| text.len_bytes() <= MAX_HIGHLIGHT_BYTES)
            .map(Syntax::new);
        Self {
            language,
            syntax,
            map: None,
            parse_task: None,
        }
    }

    /// Подпись для статус-бара.
    pub fn status(&self) -> String {
        match (self.language, &self.syntax) {
            (None, _) => "Plain Text".into(),
            (Some(language), Some(_)) => language.display_name().into(),
            (Some(language), None) => format!("{} (no highlighting)", language.display_name()),
        }
    }

    /// Изменение текста документа: дёшево, только сдвигает узлы дерева.
    pub fn edit(&mut self, change: &TextChange) {
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&change.old_text, &change.changes);
        }
    }

    /// Документ получил путь («Сохранить как»). Если язык сменился —
    /// синтаксис заводится заново; `true` — нужен разбор.
    pub fn set_path(&mut self, path: &Path, text: &Rope) -> bool {
        if language_for_path(path) == self.language {
            return false;
        }
        *self = Self::new(Some(path), text);
        self.syntax.is_some()
    }

    /// Спаны видимых строк; пусто, пока нет дерева или отображения на тему.
    pub fn highlight_lines(&self, text: &Rope, lines: Range<usize>) -> Vec<Vec<HighlightSpan>> {
        match (&self.syntax, &self.map) {
            (Some(syntax), Some(map)) => syntax.highlight_lines(text, lines, map),
            _ => Vec::new(),
        }
    }

    /// Пересобирает отображение capture на области темы (сменилась тема).
    /// Пока запрос языка не скомпилирован в фоне, отображения нет.
    pub fn refresh_map(&mut self, theme: &Theme) {
        self.map = self
            .syntax
            .as_ref()
            .and_then(|syntax| HighlightMap::try_new(syntax.language(), &theme.syntax_scopes()));
    }

    fn ensure_map(&mut self, theme: &Theme) {
        if self.map.is_none() {
            self.refresh_map(theme);
        }
    }
}

/// Запускает разбор документа редактора, если он нужен (см. описание модуля).
/// Пока идёт фоновый разбор, новый не начинается: правки копятся в `Syntax`
/// и уходят в следующий разбор по готовности текущего.
pub fn parse(editor: &mut Editor, mode: ParseMode, cx: &mut Context<Editor>) {
    let highlighter = &mut editor.highlighter;
    let Some(syntax) = &mut highlighter.syntax else {
        return;
    };
    let Some(job) = syntax.parse_job(editor.document.text()) else {
        return;
    };
    let job = match mode {
        ParseMode::AfterEdit => match job.run_with_budget(SYNC_PARSE_BUDGET) {
            Ok(result) => {
                syntax.finish(result);
                highlighter.ensure_map(Theme::get(cx));
                return;
            }
            Err(job) => job,
        },
        ParseMode::Background => job,
    };
    let parsing = cx.background_spawn(async move { job.run() });
    highlighter.parse_task = Some(cx.spawn(async move |editor, cx| {
        let result = parsing.await;
        editor
            .update(cx, |editor, cx| {
                let highlighter = &mut editor.highlighter;
                if let Some(syntax) = &mut highlighter.syntax {
                    syntax.finish(result);
                }
                highlighter.ensure_map(Theme::get(cx));
                // Правки, пришедшие за время разбора, — в следующий разбор.
                parse(editor, ParseMode::Background, cx);
                cx.notify();
            })
            .ok();
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlighter(path: Option<&str>, text: &str) -> Highlighter {
        Highlighter::new(path.map(Path::new), &Rope::from_str(text))
    }

    #[test]
    fn language_comes_from_the_path() {
        assert_eq!(
            highlighter(Some("/a/main.rs"), "fn main() {}").status(),
            "Rust"
        );
        assert_eq!(highlighter(Some("/a/App.tsx"), "").status(), "TSX");
        assert_eq!(highlighter(Some("/a/notes.txt"), "").status(), "Plain Text");
        assert_eq!(highlighter(None, "fn main() {}").status(), "Plain Text");
        assert!(highlighter(None, "").syntax.is_none());
    }

    #[test]
    fn huge_files_are_not_highlighted() {
        let big = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        let huge = highlighter(Some("/a/big.rs"), &big);
        assert!(huge.syntax.is_none());
        assert_eq!(huge.status(), "Rust (no highlighting)");
        assert!(highlighter(Some("/a/ok.rs"), &big[1..]).syntax.is_some());
    }

    #[test]
    fn new_path_replaces_syntax_only_when_language_changes() {
        let text = Rope::from_str("fn main() {}\n");
        let mut h = Highlighter::new(Some(Path::new("/a/notes.txt")), &text);
        assert!(h.syntax.is_none());
        assert!(
            h.set_path(Path::new("/a/main.rs"), &text),
            "txt → rs needs a parse"
        );
        assert_eq!(h.status(), "Rust");
        assert!(
            !h.set_path(Path::new("/b/other.rs"), &text),
            "same language"
        );
        assert!(
            !h.set_path(Path::new("/a/notes.md.txt"), &text),
            "to plain text"
        );
        assert!(h.syntax.is_none());
    }

    #[test]
    fn no_spans_before_the_first_parse() {
        let text = Rope::from_str("fn main() {}\n");
        let h = Highlighter::new(Some(Path::new("/a/main.rs")), &text);
        assert!(h.highlight_lines(&text, 0..2).is_empty());
    }
}
