//! The document parse tree as a synchronous state machine with no threads.
//!
//! Lifecycle:
//! 1. [`Syntax::edit`] is called on every edit, on the UI thread: just `Tree::edit`, microseconds.
//!    The tree stays usable for highlighting while a parse is in progress.
//! 2. [`Syntax::parse_job`] takes a snapshot of the text and the tree ([`ParseJob`], `Send +
//!    'static`).
//! 3. [`ParseJob::run`] anywhere (usually on a background executor) or
//!    [`ParseJob::run_with_budget`] directly on the UI thread with a budget of ~1 ms.
//! 4. [`Syntax::finish`] installs the new tree, replaying onto it the edits made after the job
//!    started.
//!
//! At most one job runs at a time: edits made during a parse accumulate and go into the next parse,
//! so a long parse doesn't build up a queue.

use std::ops::{ControlFlow, Range};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use flux_core::{ChangeSet, Rope};
use tree_sitter::{InputEdit, ParseOptions, ParseState, Parser, Point, Tree};

use crate::edit::input_edits;
use crate::highlight::{self, HighlightMap, HighlightSpan};
use crate::language::Language;
use crate::text::{chunk_from, count_newlines};

pub struct Syntax {
    language: &'static Language,
    /// The latest tree, edited to match the current text. It may be stale (edits made after the
    /// parse), but its node offsets have already been shifted.
    tree: Option<Tree>,
    /// The number of `\n` in the current text, if known. Always known when there is a tree: edits
    /// use it to tell whether ropey lines match tree-sitter lines.
    newlines: Option<usize>,
    /// The text has changed since the snapshot of the last job (or there have been no jobs yet).
    dirty: bool,
    /// Text version: the number of edits since creation.
    version: u64,
    job: Option<InFlight>,
    /// The parser kept between jobs, so memory isn't reallocated for each one.
    parser: Option<Parser>,
}

/// The job handed out to the application.
struct InFlight {
    /// Whether the job is alive: the reference is held by [`ParseJob`], then by [`ParseResult`].
    token: Weak<()>,
    /// Text version in the snapshot.
    version: u64,
    /// Edits made after the snapshot, to be replayed onto the job's result.
    pending: Vec<PendingEdit>,
}

enum PendingEdit {
    /// The computed tree edits (in ascending order of position).
    Ready(Vec<InputEdit>),
    /// There is no tree yet and the line count is unknown: `finish` will compute the edits from the
    /// line count in the job's result.
    Deferred { old_text: Rope, changes: ChangeSet },
}

impl Syntax {
    /// Empty state: no highlighting until the first parse.
    pub fn new(language: &'static Language) -> Self {
        Self {
            language,
            tree: None,
            newlines: None,
            dirty: true,
            version: 0,
            job: None,
            parser: None,
        }
    }

    pub fn language(&self) -> &'static Language {
        self.language
    }

    /// The current tree; after edits with no re-parse, it is the edited, stale one.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Whether a job is in progress (handed out and neither finished nor abandoned).
    pub fn is_parsing(&self) -> bool {
        self.job.as_ref().is_some_and(InFlight::is_alive)
    }

    /// Whether [`Syntax::parse_job`] would return a job right now.
    pub fn needs_parse(&self) -> bool {
        let pending = match &self.job {
            Some(job) if job.is_alive() => false,
            // An abandoned job: its snapshot was never parsed.
            Some(_) => true,
            None => self.dirty,
        };
        pending && self.language.grammar().is_some()
    }

    /// A document edit: `old_text` is the text before `changes`. Cheap: it only shifts the tree's
    /// nodes. If a parse is in progress, the edit is remembered so it can be replayed onto the
    /// parse result.
    pub fn edit(&mut self, old_text: &Rope, changes: &ChangeSet) {
        if changes.is_empty() {
            return;
        }
        if changes.len() != old_text.len_chars() {
            // The edit is not for this text: the tree can no longer be trusted.
            self.reset();
            return;
        }
        self.version += 1;
        self.dirty = true;
        self.forget_abandoned_job();
        if self.tree.is_none() && self.job.is_none() {
            // Nothing to edit: the next parse will start from scratch anyway.
            self.newlines = None;
            return;
        }
        match self.newlines {
            Some(newlines) => {
                let batch = input_edits(old_text, changes, newlines);
                if let Some(tree) = &mut self.tree {
                    apply_edits(tree, &batch.edits);
                }
                self.newlines = Some(batch.newlines);
                if let Some(job) = &mut self.job {
                    job.pending.push(PendingEdit::Ready(batch.edits));
                }
            }
            None => {
                // The line count is unknown, so there is no tree and the first parse is in
                // progress.
                debug_assert!(self.tree.is_none());
                if let Some(job) = &mut self.job {
                    job.pending.push(PendingEdit::Deferred {
                        old_text: old_text.clone(),
                        changes: changes.clone(),
                    });
                }
            }
        }
    }

    /// A job to parse `text`, the document's current text. `None` if no parse is needed or one is
    /// already in progress (in that case, ask again after [`Syntax::finish`]).
    pub fn parse_job(&mut self, text: &Rope) -> Option<ParseJob> {
        if self.is_parsing() {
            return None;
        }
        self.forget_abandoned_job();
        if !self.dirty {
            return None;
        }
        self.language.grammar()?;
        let token = Arc::new(());
        self.job = Some(InFlight {
            token: Arc::downgrade(&token),
            version: self.version,
            pending: Vec::new(),
        });
        self.dirty = false;
        Some(ParseJob {
            language: self.language,
            text: text.clone(),
            old_tree: self.tree.clone(),
            parser: self.parser.take(),
            newlines: self.newlines,
            token,
            version: self.version,
        })
    }

    /// Installs the result of a job issued by this `Syntax`, replaying the edits made after the job
    /// started; if there were any, the state stays "needs parse". A foreign or stale result (after
    /// [`Syntax::reset`]) is discarded: returns `false`.
    pub fn finish(&mut self, result: ParseResult) -> bool {
        let ParseResult {
            tree,
            parser,
            newlines,
            token,
            version,
        } = result;
        let Some(job) = self
            .job
            .take_if(|job| std::ptr::eq(job.token.as_ptr(), Arc::as_ptr(&token)))
        else {
            return false;
        };
        debug_assert_eq!(job.version, version);
        debug_assert_eq!(job.version + job.pending.len() as u64, self.version);
        self.parser.get_or_insert(parser);
        let Some(mut tree) = tree else {
            // The parse failed: the previous (edited) tree stays.
            return true;
        };
        let mut newlines = newlines;
        for pending in job.pending {
            match pending {
                PendingEdit::Ready(edits) => apply_edits(&mut tree, &edits),
                PendingEdit::Deferred { old_text, changes } => {
                    let batch = input_edits(&old_text, &changes, newlines);
                    apply_edits(&mut tree, &batch.edits);
                    newlines = batch.newlines;
                }
            }
        }
        self.tree = Some(tree);
        self.newlines.get_or_insert(newlines);
        true
    }

    /// Forgets the tree: the next parse starts from scratch, and there is no highlighting until
    /// then. For edits that can't be expressed through a [`ChangeSet`] (the file was re-read from
    /// disk). The result of an already issued job will be discarded.
    pub fn reset(&mut self) {
        self.tree = None;
        self.newlines = None;
        self.job = None;
        self.dirty = true;
        self.version += 1;
    }

    /// Highlighting for the lines `lines` (ropey lines): a vector of spans for each existing line
    /// in the range. Columns are in characters within the line, excluding the newline. Before the
    /// first parse, the vectors are empty.
    pub fn highlight_lines(
        &self,
        text: &Rope,
        lines: Range<usize>,
        map: &HighlightMap,
    ) -> Vec<Vec<HighlightSpan>> {
        let tree = self
            .tree
            .as_ref()
            .filter(|_| std::ptr::eq(map.language(), self.language));
        debug_assert!(
            std::ptr::eq(map.language(), self.language),
            "highlight map is for another language"
        );
        highlight::highlight_lines(tree, text, lines, map)
    }

    /// The job was abandoned before reaching `finish`: its snapshot was never parsed.
    fn forget_abandoned_job(&mut self) {
        if self.job.take_if(|job| !job.is_alive()).is_some() {
            self.dirty = true;
        }
    }
}

impl InFlight {
    fn is_alive(&self) -> bool {
        self.token.strong_count() > 0
    }
}

fn apply_edits(tree: &mut Tree, edits: &[InputEdit]) {
    for edit in edits.iter().rev() {
        tree.edit(edit);
    }
}

/// A parse of a text snapshot. `Send + 'static`: can be handed to a background thread.
pub struct ParseJob {
    language: &'static Language,
    text: Rope,
    /// The edited tree for incremental parsing.
    old_tree: Option<Tree>,
    /// A parser carrying the state of an interrupted parse, if there was one.
    parser: Option<Parser>,
    newlines: Option<usize>,
    token: Arc<()>,
    version: u64,
}

/// The job's result; pass it to [`Syntax::finish`].
pub struct ParseResult {
    tree: Option<Tree>,
    parser: Parser,
    /// The number of `\n` in the snapshot text.
    newlines: usize,
    token: Arc<()>,
    version: u64,
}

impl ParseJob {
    /// Parses to completion. Also compiles the language's highlighting query, so the first
    /// highlighting on the UI thread doesn't pay milliseconds for it.
    pub fn run(mut self) -> ParseResult {
        self.language.query();
        let tree = self.parse(None);
        self.into_result(tree)
    }

    /// Parses for no longer than `budget`. If it doesn't finish in time, returns `Err` with the
    /// same job: the parser remembers where it stopped, and the next run continues from the same
    /// place. The budget is checked once every hundred parser steps, so a small text finishes even
    /// with a zero budget.
    pub fn run_with_budget(mut self, budget: Duration) -> Result<ParseResult, ParseJob> {
        // A budget that doesn't fit in an Instant is the same as no budget.
        match self.parse(Instant::now().checked_add(budget)) {
            Some(tree) => Ok(self.into_result(Some(tree))),
            None => Err(self),
        }
    }

    /// `deadline` is when to interrupt the parse; `None` means parse to completion.
    fn parse(&mut self, deadline: Option<Instant>) -> Option<Tree> {
        let language = self.language;
        let Self {
            text,
            old_tree,
            parser,
            ..
        } = self;
        if parser.is_none() {
            *parser = new_parser(language);
        }
        let parser = parser.as_mut()?;
        let mut read = |byte: usize, _: Point| chunk_from(text, byte);
        let Some(deadline) = deadline else {
            return parser.parse_with_options(&mut read, old_tree.as_ref(), None);
        };
        let mut progress = |_: &ParseState| {
            if Instant::now() >= deadline {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        parser.parse_with_options(&mut read, old_tree.as_ref(), Some(options))
    }

    fn into_result(self, tree: Option<Tree>) -> ParseResult {
        let newlines = self.newlines.unwrap_or_else(|| count_newlines(&self.text));
        // The parser is always there except for an unsupported grammar, which parse_job never lets
        // into a job.
        let parser = self.parser.unwrap_or_default();
        ParseResult {
            tree,
            parser,
            newlines,
            token: self.token,
            version: self.version,
        }
    }
}

fn new_parser(language: &Language) -> Option<Parser> {
    let mut parser = Parser::new();
    parser.set_language(language.grammar()?).ok()?;
    Some(parser)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::language_by_name;

    fn assert_send_static<T: Send + 'static>() {}

    #[test]
    fn job_and_result_can_move_to_another_thread() {
        assert_send_static::<ParseJob>();
        assert_send_static::<ParseResult>();
        assert_send_static::<Syntax>();
    }

    #[test]
    fn job_runs_on_another_thread() {
        let text = Rope::from_str("fn main() { let x = 1; }\n");
        let mut syntax = Syntax::new(language_by_name("rust").unwrap());
        let job = syntax.parse_job(&text).unwrap();
        let result = std::thread::spawn(move || job.run()).join().unwrap();
        assert!(syntax.finish(result));
        let root = syntax.tree().unwrap().root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(!root.has_error());
        assert_eq!(syntax.newlines, Some(1));
    }

    #[test]
    fn no_job_when_nothing_changed() {
        let text = Rope::from_str("a = 1\n");
        let mut syntax = Syntax::new(language_by_name("python").unwrap());
        assert!(syntax.needs_parse());
        let job = syntax.parse_job(&text).unwrap();
        assert!(syntax.is_parsing());
        assert!(syntax.parse_job(&text).is_none(), "one job at a time");
        assert!(syntax.finish(job.run()));
        assert!(!syntax.needs_parse());
        assert!(syntax.parse_job(&text).is_none());

        let mut new_text = text.clone();
        let empty = ChangeSet::identity(text.len_chars());
        syntax.edit(&text, &empty);
        assert!(!syntax.needs_parse(), "identity change is not an edit");
        let cs = ChangeSet::from_changes(text.len_chars(), [(0, 1, Some("b".into()))]);
        syntax.edit(&text, &cs);
        cs.apply(&mut new_text);
        assert!(syntax.needs_parse());
        assert!(syntax.parse_job(&new_text).is_some());
    }

    #[test]
    fn parser_is_reused_between_jobs() {
        let text = Rope::from_str("x\n");
        let mut syntax = Syntax::new(language_by_name("rust").unwrap());
        let job = syntax.parse_job(&text).unwrap();
        assert!(job.parser.is_none());
        syntax.finish(job.run());
        syntax.reset();
        let job = syntax.parse_job(&text).unwrap();
        assert!(job.parser.is_some());
    }
}
