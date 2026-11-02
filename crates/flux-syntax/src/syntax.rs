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
//!
//! A language with an injections query (plugin API 0.4) embeds others: after the document's tree,
//! the job finds and parses the embedded documents ([`crate::injection`]) — within the same budget
//! on the UI thread, and whatever doesn't fit (or is a WebAssembly grammar) goes on in the
//! background like the document's own parse.

use std::ops::{ControlFlow, Range};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use flux_core::{ChangeSet, Rope};
use tree_sitter::{InputEdit, ParseOptions, ParseState, Parser, Point, Tree};

use crate::edit::input_edits;
use crate::highlight::{self, HighlightMaps, HighlightSpan};
use crate::injection::{Injector, Layer, edit_bytes, edit_layers, merge_ranges};
use crate::language::Language;
use crate::text::{chunk_from, count_newlines};

/// More edited ranges than this are remembered as one, from the first to the last.
const MAX_EDITED: usize = 32;

/// An embedded document ([`Syntax::injections`]): its language and the text it is made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Injection {
    pub language: Arc<Language>,
    /// 1 for one in the document, 2 for one inside it…
    pub depth: usize,
    /// Byte ranges of the document's text.
    pub ranges: Vec<Range<usize>>,
}

pub struct Syntax {
    language: Arc<Language>,
    /// The latest tree, edited to match the current text. It may be stale (edits made after the
    /// parse), but its node offsets have already been shifted.
    tree: Option<Tree>,
    /// The embedded documents of the latest parse, edited like the tree; parents before children.
    layers: Vec<Layer>,
    /// The bytes of the current text edited since the last job's snapshot: where the next job
    /// looks for injections again (with the places where the tree changed).
    edited: Vec<Range<usize>>,
    /// The next job looks for injections everywhere: the first parse, the languages changed, a
    /// job was lost with its edits.
    reinject: bool,
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
    /// The same for the embedded documents.
    layer_parser: Option<Parser>,
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
    pub fn new(language: Arc<Language>) -> Self {
        Self {
            language,
            tree: None,
            layers: Vec::new(),
            edited: Vec::new(),
            reinject: true,
            newlines: None,
            dirty: true,
            version: 0,
            job: None,
            parser: None,
            layer_parser: None,
        }
    }

    /// The embedded documents of the latest parse, edited to match the current text, in order:
    /// a document's before the ones inside it.
    pub fn injections(&self) -> Vec<Injection> {
        self.layers
            .iter()
            .filter(|layer| layer.tree.is_some())
            .map(|layer| Injection {
                language: layer.language.clone(),
                depth: layer.depth,
                ranges: layer
                    .ranges()
                    .map(|range| range.start_byte..range.end_byte)
                    .collect(),
            })
            .collect()
    }

    /// The document's language and the languages embedded in it now, each once: the highlight
    /// maps a highlighting needs ([`HighlightMaps`]).
    pub fn languages(&self) -> Vec<Arc<Language>> {
        let mut languages = vec![self.language.clone()];
        for layer in &self.layers {
            if !languages
                .iter()
                .any(|known| Arc::ptr_eq(known, &layer.language))
            {
                languages.push(layer.language.clone());
            }
        }
        languages
    }

    /// The registered languages changed (a plugin installed, removed, turned on or off): the
    /// injections are looked up again — a language may have come or gone. Needs a parse.
    pub fn languages_changed(&mut self) {
        if !self.language.has_injections() && self.layers.is_empty() {
            return;
        }
        self.reinject = true;
        self.dirty = true;
        // Its store keeps every WebAssembly grammar it parsed, the removed ones too.
        self.layer_parser = None;
    }

    pub fn language(&self) -> &Arc<Language> {
        &self.language
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
        pending && !self.language.grammar_failed()
    }

    /// A document edit: `old_text` is the text before `changes`. Cheap: it only shifts the tree's
    /// nodes. If a parse is in progress, the edit is remembered so it can be replayed onto the
    /// parse result.
    pub fn edit(&mut self, old_text: &Rope, changes: &ChangeSet) {
        if changes.is_empty() {
            return;
        }
        if self.language.grammar_failed() {
            // A grammar that failed or got stuck is not parsed again: there is nothing to keep up
            // to date, and a stuck parse never hands its job back to replay the edits onto.
            self.tree = None;
            self.job = None;
            self.newlines = None;
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
                edit_layers(&mut self.layers, &batch.edits);
                self.record_edits(&batch.edits);
                self.newlines = Some(batch.newlines);
                if let Some(job) = &mut self.job {
                    job.pending.push(PendingEdit::Ready(batch.edits));
                }
            }
            None => {
                // The line count is unknown, so there is no tree and the first parse is in
                // progress.
                debug_assert!(self.tree.is_none());
                self.reinject = true;
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
        if self.language.grammar_failed() {
            return None;
        }
        let token = Arc::new(());
        self.job = Some(InFlight {
            token: Arc::downgrade(&token),
            version: self.version,
            pending: Vec::new(),
        });
        self.dirty = false;
        let edited = std::mem::take(&mut self.edited);
        let edited = (!std::mem::take(&mut self.reinject)).then_some(edited);
        Some(ParseJob {
            language: self.language.clone(),
            text: text.clone(),
            old_tree: self.tree.clone(),
            parser: self.parser.take(),
            newlines: self.newlines,
            token,
            version: self.version,
            tree: None,
            injector: Box::new(Injector::new(
                self.layers.clone(),
                edited,
                self.layer_parser.take(),
            )),
        })
    }

    /// Installs the result of a job issued by this `Syntax`, replaying the edits made after the job
    /// started; if there were any, the state stays "needs parse". A foreign or stale result (after
    /// [`Syntax::reset`]) is discarded: returns `false`.
    pub fn finish(&mut self, result: ParseResult) -> bool {
        let ParseResult {
            tree,
            layers,
            parser,
            layer_parser,
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
        if let Some(parser) = parser {
            self.parser.get_or_insert(parser);
        }
        if let Some(parser) = layer_parser {
            self.layer_parser.get_or_insert(parser);
        }
        let Some(mut tree) = tree else {
            // The parse failed: the previous (edited) tree and layers stay, and the edits it took
            // are looked at again.
            self.reinject = true;
            return true;
        };
        let mut layers = layers;
        let mut newlines = newlines;
        for pending in job.pending {
            match pending {
                PendingEdit::Ready(edits) => {
                    apply_edits(&mut tree, &edits);
                    edit_layers(&mut layers, &edits);
                }
                PendingEdit::Deferred { old_text, changes } => {
                    let batch = input_edits(&old_text, &changes, newlines);
                    apply_edits(&mut tree, &batch.edits);
                    edit_layers(&mut layers, &batch.edits);
                    newlines = batch.newlines;
                }
            }
        }
        self.tree = Some(tree);
        self.layers = layers;
        self.newlines.get_or_insert(newlines);
        true
    }

    /// Forgets the tree: the next parse starts from scratch, and there is no highlighting until
    /// then. For edits that can't be expressed through a [`ChangeSet`] (the file was re-read from
    /// disk). The result of an already issued job will be discarded.
    pub fn reset(&mut self) {
        self.tree = None;
        self.layers.clear();
        self.edited.clear();
        self.reinject = true;
        self.newlines = None;
        self.job = None;
        self.dirty = true;
        self.version += 1;
    }

    /// Highlighting for the lines `lines` (ropey lines): a vector of spans for each existing line
    /// in the range. Columns are in characters within the line, excluding the newline. Before the
    /// first parse, the vectors are empty. `maps` has the document language's map — a
    /// [`HighlightMap`](crate::HighlightMap) — and those of the embedded languages; an embedded
    /// document without one keeps the host's highlighting.
    pub fn highlight_lines(
        &self,
        text: &Rope,
        lines: Range<usize>,
        maps: &(impl HighlightMaps + ?Sized),
    ) -> Vec<Vec<HighlightSpan>> {
        highlight::highlight_lines(
            self.tree.as_ref(),
            &self.language,
            &self.layers,
            text,
            lines,
            maps,
        )
    }

    /// Remembers where the edits of one change put new text, in the current text's coordinates:
    /// the next job looks for injections there.
    fn record_edits(&mut self, edits: &[InputEdit]) {
        if !self.language.has_injections() {
            return;
        }
        for edit in edits.iter().rev() {
            for range in &mut self.edited {
                *range = edit_bytes(range, edit);
            }
        }
        // The edits are in the old text's coordinates: each moves by those before it.
        let mut shift = 0isize;
        for edit in edits {
            let start = edit.start_byte.saturating_add_signed(shift);
            let end = edit.new_end_byte.saturating_add_signed(shift);
            self.edited.push(start..end);
            shift += edit.new_end_byte as isize - edit.old_end_byte as isize;
        }
        let mut edited = merge_ranges(std::mem::take(&mut self.edited));
        if edited.len() > MAX_EDITED {
            edited = std::iter::once(edited[0].start..edited[edited.len() - 1].end).collect();
        }
        self.edited = edited;
    }

    /// The job was abandoned before reaching `finish`: its snapshot was never parsed.
    fn forget_abandoned_job(&mut self) {
        if self.job.take_if(|job| !job.is_alive()).is_some() {
            self.dirty = true;
            // The edits it took are gone with it.
            self.reinject = true;
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
    language: Arc<Language>,
    text: Rope,
    /// The edited tree for incremental parsing.
    old_tree: Option<Tree>,
    /// A parser carrying the state of an interrupted parse, if there was one.
    parser: Option<Parser>,
    newlines: Option<usize>,
    token: Arc<()>,
    version: u64,
    /// The document's new tree, once parsed: then the embedded documents are.
    tree: Option<Tree>,
    /// Boxed: the job comes back in `Err` from a parse within a budget.
    injector: Box<Injector>,
}

/// The job's result; pass it to [`Syntax::finish`].
pub struct ParseResult {
    tree: Option<Tree>,
    layers: Vec<Layer>,
    /// None when no parser could be made (the grammar didn't load).
    parser: Option<Parser>,
    layer_parser: Option<Parser>,
    /// The number of `\n` in the snapshot text.
    newlines: usize,
    token: Arc<()>,
    version: u64,
}

impl ParseJob {
    /// The language being parsed.
    pub fn language(&self) -> &Arc<Language> {
        &self.language
    }

    /// Whether the job may parse a WebAssembly grammar — the document's, or an embedded one's (a
    /// language with injections may embed any): run it on a thread of its own
    /// ([`ParseJob::spawn`]), its scanner is a plugin's code.
    pub fn needs_own_thread(&self) -> bool {
        self.language.is_wasm() || self.language.has_injections() || self.injector.has_wasm()
    }

    /// Parses to completion. Also loads the grammar and compiles the language's queries, so the
    /// first highlighting on the UI thread doesn't pay milliseconds for them. A WebAssembly grammar
    /// is parsed under the watchdog ([`crate::wasm::PARSE_LIMIT`]), the embedded ones too.
    pub fn run(mut self) -> ParseResult {
        if self.tree.is_none() {
            let _watch = self
                .language
                .is_wasm()
                .then(|| crate::wasm::Watch::start(&self.language));
            self.language.query();
            self.language.injections();
            self.tree = self.parse(None);
        }
        // Without a deadline it always finishes.
        let _ = self.inject(None);
        self.into_result()
    }

    /// Runs the job ([`ParseJob::run`]) on a thread of its own and hands the result to `done`
    /// there. For a WebAssembly grammar ([`ParseJob::needs_own_thread`]): its scanner is a plugin's
    /// code that may never return, and then only this thread is stuck, not a thread of a pool. If
    /// the thread can't be made, the job is dropped and `done` is never called.
    pub fn spawn(self, done: impl FnOnce(ParseResult) + Send + 'static) {
        let thread = std::thread::Builder::new().name("flux-parse".into());
        if let Err(err) = thread.spawn(move || done(self.run())) {
            eprintln!("flux: can't start a parse: {err}");
        }
    }

    /// Parses for no longer than `budget`. If it doesn't finish in time, returns `Err` with the
    /// same job: the parser remembers where it stopped, and the next run continues from the same
    /// place. The budget is checked once every hundred parser steps, so a small text finishes even
    /// with a zero budget. A grammar that isn't loaded yet is not loaded here (it may take a while),
    /// and a WebAssembly grammar is never parsed here — this is for the UI thread, and its scanner
    /// may never return ([`crate::wasm`]): the job comes back for [`ParseJob::run`]. The same goes
    /// for the embedded documents: the job comes back at the first one it can't parse here.
    pub fn run_with_budget(mut self, budget: Duration) -> Result<ParseResult, ParseJob> {
        let not_loaded = self.parser.is_none() && self.language.loaded_grammar().is_none();
        let injections_not_ready = self.language.has_injections() && !self.language.is_ready();
        if self.language.is_wasm() || not_loaded || injections_not_ready {
            return Err(self);
        }
        // A budget that doesn't fit in an Instant is the same as no budget.
        let deadline = Instant::now().checked_add(budget);
        if self.tree.is_none() {
            match self.parse(deadline) {
                Some(tree) => self.tree = Some(tree),
                None => return Err(self),
            }
        }
        match self.inject(deadline) {
            ControlFlow::Continue(()) => Ok(self.into_result()),
            ControlFlow::Break(()) => Err(self),
        }
    }

    /// Finds and parses the embedded documents, once the document's tree is there.
    fn inject(&mut self, deadline: Option<Instant>) -> ControlFlow<()> {
        let Some(tree) = &self.tree else {
            return ControlFlow::Continue(());
        };
        match self.injector.run(
            &self.text,
            &self.language,
            tree,
            self.old_tree.as_ref(),
            deadline,
        ) {
            ControlFlow::Continue(()) => ControlFlow::Continue(()),
            ControlFlow::Break(_) => ControlFlow::Break(()),
        }
    }

    /// `deadline` is when to interrupt the parse; `None` means parse to completion.
    fn parse(&mut self, deadline: Option<Instant>) -> Option<Tree> {
        let language = self.language.clone();
        let Self {
            text,
            old_tree,
            parser,
            ..
        } = self;
        if parser.is_none() {
            *parser = new_parser(&language);
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

    fn into_result(self) -> ParseResult {
        let newlines = self.newlines.unwrap_or_else(|| count_newlines(&self.text));
        let (layers, layer_parser) = self.injector.into_parts();
        ParseResult {
            tree: self.tree,
            layers,
            parser: self.parser,
            layer_parser,
            newlines,
            token: self.token,
            version: self.version,
        }
    }
}

/// A parser of the language; one of a WebAssembly grammar gets a store of its own.
fn new_parser(language: &Language) -> Option<Parser> {
    let grammar = language.grammar()?;
    let mut parser = Parser::new();
    if language.is_wasm() {
        parser.set_wasm_store(crate::wasm::new_store().ok()?).ok()?;
    }
    parser.set_language(grammar).ok()?;
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
        crate::standard::register();
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
        crate::standard::register();
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
        crate::standard::register();
        let mut syntax = Syntax::new(language_by_name("rust").unwrap());
        let job = syntax.parse_job(&text).unwrap();
        assert!(job.parser.is_none());
        syntax.finish(job.run());
        syntax.reset();
        let job = syntax.parse_job(&text).unwrap();
        assert!(job.parser.is_some());
    }
}
