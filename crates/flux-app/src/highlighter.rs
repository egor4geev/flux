//! Highlighting of the editor's document: the parse tree, its parsing in the background, and the
//! mapping of captures to theme scopes.
//!
//! The parse cycle ([`parse`]):
//! - after an edit — first synchronously, with the budget [`SYNC_PARSE_BUDGET`]: it usually
//!   finishes in time, and the frame is drawn right away with fresh highlighting;
//! - if it didn't finish in time, or this is a file open, a language change, or a re-parse — the
//!   parse goes to the background executor; when it completes: `finish`, a re-parse if there were
//!   edits in the meantime, and a redraw;
//! - until the parse is ready, highlighting is taken from the old tree shifted by the edits
//!   (`Syntax::edit`), so there is no flicker.
//!
//! A plugin's WebAssembly grammar (stage 8.3) is never parsed on the UI thread, and each of its
//! parses runs on a thread of its own instead of the background executor: its scanner is a plugin's
//! code that may never return, and then flux-syntax's watchdog marks the grammar failed and only
//! that thread is stuck.
//!
//! The highlight query is compiled in the background (`ParseJob::run`), so [`HighlightMap`] is
//! built only after a background parse, via `try_new`.

use std::future::Future;
use std::ops::Range;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use flux_core::{Rope, TextChange};
use flux_syntax::{
    HighlightMap, HighlightSpan, Language, ParseJob, ParseResult, Syntax, language_for_path,
};
use futures::channel::oneshot;
use gpui::{AppContext, Context, Task};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::theme::Theme;

/// Files larger than this get no highlighting: parse time and tree memory grow with the file size.
pub const MAX_HIGHLIGHT_BYTES: usize = 10 * 1024 * 1024;

/// How long a parse may take on the UI thread right after an edit.
pub const SYNC_PARSE_BUDGET: Duration = Duration::from_millis(1);

/// When a parse is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Right after an edit: first synchronously within the budget; if it doesn't finish in time, in
    /// the background.
    AfterEdit,
    /// Opening a file, a language change, a re-parse after a background parse: background only.
    Background,
}

pub struct Highlighter {
    /// The language from the file path, even if highlighting is off because of the file size.
    language: Option<Arc<Language>>,
    /// [`flux_syntax::generation`] when the language was looked up: when plugins add or remove
    /// languages, the document looks its language up again ([`Highlighter::languages_changed`]).
    generation: u64,
    syntax: Option<Syntax>,
    /// capture → scope of the current theme; appears after the first background parse.
    map: Option<HighlightMap>,
    /// The background parse. Dropping it cancels it: the tab was closed or the language changed.
    parse_task: Option<Task<()>>,
}

impl Highlighter {
    /// The language is determined by the path; an unnamed document and files larger than
    /// [`MAX_HIGHLIGHT_BYTES`] have no highlighting.
    pub fn new(path: Option<&Path>, text: &Rope) -> Self {
        let generation = flux_syntax::generation();
        let language = path.and_then(language_for_path);
        let syntax = language
            .clone()
            .filter(|_| text.len_bytes() <= MAX_HIGHLIGHT_BYTES)
            .map(Syntax::new);
        Self {
            language,
            generation,
            syntax,
            map: None,
            parse_task: None,
        }
    }

    /// The label for the status bar.
    pub fn status(&self) -> String {
        match (&self.language, &self.syntax) {
            (None, _) => tr("Plain Text").into(),
            // A grammar that couldn't be loaded or got stuck: the plugin's log says why.
            (Some(language), Some(_)) if language.grammar_failed() => {
                trf("{0} (no highlighting)", &[&language.display_name()])
            }
            (Some(language), Some(_)) => language.display_name().into(),
            (Some(language), None) => trf("{0} (no highlighting)", &[&language.display_name()]),
        }
    }

    /// A change to the document text: cheap, it only shifts tree nodes.
    pub fn edit(&mut self, change: &TextChange) {
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&change.old_text, &change.changes);
        }
    }

    /// The document got a path ("Save As"). If the language changed, the syntax is set up anew;
    /// `true` means a parse is needed.
    pub fn set_path(&mut self, path: &Path, text: &Rope) -> bool {
        if language_for_path(path) == self.language {
            return false;
        }
        *self = Self::new(Some(path), text);
        self.syntax.is_some()
    }

    /// The document's language as registered now (plugins turned on or off, installed, removed).
    /// If it changed, the syntax is set up anew; `true` means a parse is needed.
    pub fn languages_changed(&mut self, path: Option<&Path>, text: &Rope) -> bool {
        let generation = flux_syntax::generation();
        if generation == self.generation {
            return false;
        }
        self.generation = generation;
        if path.and_then(language_for_path) == self.language {
            return false;
        }
        *self = Self::new(path, text);
        self.syntax.is_some()
    }

    /// Spans of the visible lines; empty until there is a tree or a mapping to the theme.
    pub fn highlight_lines(&self, text: &Rope, lines: Range<usize>) -> Vec<Vec<HighlightSpan>> {
        match (&self.syntax, &self.map) {
            (Some(syntax), Some(map)) => syntax.highlight_lines(text, lines, map),
            _ => Vec::new(),
        }
    }

    /// Rebuilds the mapping of captures to theme scopes (the theme changed). Until the language
    /// query is compiled in the background, there is no mapping.
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

/// Starts parsing the editor's document if needed (see the module description). While a background
/// parse is running, a new one doesn't start: edits accumulate in `Syntax` and go into the next
/// parse when the current one finishes.
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
    let parsing = run_in_background(job, cx);
    highlighter.parse_task = Some(cx.spawn(async move |editor, cx| {
        // A stuck WebAssembly parse never answers: the task waits until the tab closes.
        let Some(result) = parsing.await else {
            return;
        };
        editor
            .update(cx, |editor, cx| {
                let highlighter = &mut editor.highlighter;
                if let Some(syntax) = &mut highlighter.syntax {
                    syntax.finish(result);
                }
                highlighter.ensure_map(Theme::get(cx));
                // Edits that arrived during the parse go into the next parse.
                parse(editor, ParseMode::Background, cx);
                cx.notify();
            })
            .ok();
    }));
}

/// Runs a parse off the UI thread: a native grammar on the background executor, a WebAssembly one on
/// a thread of its own ([`ParseJob::spawn`]). `None` if the parse never finished (the thread
/// couldn't start).
fn run_in_background(
    job: ParseJob,
    cx: &mut Context<Editor>,
) -> Pin<Box<dyn Future<Output = Option<ParseResult>>>> {
    if job.language().is_wasm() {
        let (sender, receiver) = oneshot::channel();
        job.spawn(move |result| {
            sender.send(result).ok();
        });
        Box::pin(async move { receiver.await.ok() })
    } else {
        let task = cx.background_spawn(async move { job.run() });
        Box::pin(async move { Some(task.await) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlighter(path: Option<&str>, text: &str) -> Highlighter {
        flux_syntax::standard::register();
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
        flux_syntax::standard::register();
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
        flux_syntax::standard::register();
        let text = Rope::from_str("fn main() {}\n");
        let h = Highlighter::new(Some(Path::new("/a/main.rs")), &text);
        assert!(h.highlight_lines(&text, 0..2).is_empty());
    }
}
