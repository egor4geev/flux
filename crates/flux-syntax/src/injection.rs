//! Injections (plugin API 0.4): documents of other languages inside a document — the JavaScript of
//! an HTML `<script>`, the Rust of a Markdown code block, the TypeScript of a Vue file's
//! `<script lang="ts">`, the expressions of a template.
//!
//! A language's injections query ([`LanguageConfig::injections`](crate::LanguageConfig)) marks
//! them as tree-sitter's injections queries do: `@injection.content` is the node whose text is the
//! embedded document; `@injection.language` is a node whose text names its language, or
//! `#set! injection.language "css"` names it ([`injected_language`]: by id, alias, extension);
//! `#set! injection.combined` makes all the matches of the pattern one document (a template's
//! `{{ }}` expressions); `#set! injection.include-children` keeps the content node's named children
//! in it (by default they are cut out: the `>` of a quoted code block). Text predicates (`#eq?`,
//! `#match?`, `#any-of?` and their `not-` forms) filter matches; captures starting with `_` are
//! helpers. A language that isn't installed leaves its text to the host.
//!
//! Each embedded document is a [`Layer`]: its language, the ranges of the text it is made of, its
//! tree — parsed with `Parser::set_included_ranges`, so its positions are the document's — and the
//! layers embedded in it in turn, down to [`MAX_DEPTH`].
//!
//! Incrementality. Edits shift the layers like the tree ([`edit_layers`]) and mark the ones whose
//! text they touch. A parse job ([`Injector`]) looks for injections again only where the host's
//! tree changed (its changed ranges and the edited text); elsewhere the old layers stay as they
//! are, trees and all. A layer found again with the same ranges and untouched keeps its tree; one
//! whose text changed is reparsed incrementally from its old tree. So typing in a file with a
//! hundred code blocks reparses one of them.

use std::collections::VecDeque;
use std::ops::{ControlFlow, Range};
use std::sync::Arc;
use std::time::Instant;

use flux_core::Rope;
use tree_sitter::{
    InputEdit, Node, ParseOptions, ParseState, Parser, Point, Query, QueryCursor,
    StreamingIterator, Tree,
};

use crate::language::{Language, injected_language};
use crate::text::{byte_chunks, chunk_from};

/// How deep injections nest: a document's own are depth 1, those inside them 2, and so on.
pub(crate) const MAX_DEPTH: usize = 4;

/// At most this many layers in a document: a guard against pathological files.
const MAX_LAYERS: usize = 4096;

/// At most this many unfinished matches of an injections query, as for highlighting.
const MATCH_LIMIT: u32 = 256;

/// More regions than this are searched as one, from the first to the last.
const MAX_REGIONS: usize = 16;

/// A compiled injections query.
pub(crate) struct InjectionQuery {
    query: Query,
    content: u32,
    language: Option<u32>,
    patterns: Vec<PatternSettings>,
}

/// What a pattern's `#set!` says.
#[derive(Default)]
struct PatternSettings {
    language: Option<String>,
    combined: bool,
    include_children: bool,
}

impl InjectionQuery {
    /// `None` if the query doesn't compile or captures no `@injection.content`.
    pub(crate) fn new(grammar: &tree_sitter::Language, source: &str) -> Option<Self> {
        let query = Query::new(grammar, source).ok()?;
        let content = query.capture_index_for_name("injection.content")?;
        let language = query.capture_index_for_name("injection.language");
        let patterns = (0..query.pattern_count())
            .map(|pattern| {
                let mut settings = PatternSettings::default();
                for property in query.property_settings(pattern) {
                    match property.key.as_ref() {
                        "injection.language" => {
                            settings.language = property.value.as_deref().map(str::to_string);
                        }
                        "injection.combined" => settings.combined = true,
                        "injection.include-children" => settings.include_children = true,
                        _ => {}
                    }
                }
                settings
            })
            .collect();
        Some(Self {
            query,
            content,
            language,
            patterns,
        })
    }
}

/// An embedded document.
#[derive(Clone)]
pub(crate) struct Layer {
    pub(crate) language: Arc<Language>,
    /// 1 for a layer of the document, 2 for one inside it…
    pub(crate) depth: usize,
    /// The layer it is embedded in: an index in the list, always smaller than its own; `None` —
    /// the document.
    parent: Option<usize>,
    /// What the host's matches gave: one part, or one per match of a combined injection.
    parts: Vec<Part>,
    /// The pattern of a combined injection.
    combined: Option<usize>,
    /// `None` if its parse failed.
    pub(crate) tree: Option<Tree>,
    /// An edit touched its text since it was parsed.
    dirty: bool,
}

/// One match of an injection.
#[derive(Clone, Debug, PartialEq)]
struct Part {
    /// From the first to the last byte of the match's captured nodes: an edit there may change
    /// the injection (the fence's language, a `lang` attribute) even outside its text.
    extent: Range<usize>,
    /// From the start of the first content node to the end of the last one: the host's captures
    /// within it are the embedded text's, and give way to its own highlighting.
    content: Range<usize>,
    /// The text of the embedded document: the content nodes, without their children unless they
    /// are included, within the host's own ranges.
    ranges: Vec<tree_sitter::Range>,
}

impl Layer {
    /// The ranges of the text it is made of, in order.
    pub(crate) fn ranges(&self) -> impl Iterator<Item = &tree_sitter::Range> {
        self.parts.iter().flat_map(|part| &part.ranges)
    }

    /// The layer it is embedded in; `None` — the document.
    pub(crate) fn parent(&self) -> Option<usize> {
        self.parent
    }

    /// The spans of its content nodes, one per match.
    pub(crate) fn contents(&self) -> impl Iterator<Item = &Range<usize>> {
        self.parts.iter().map(|part| &part.content)
    }

    /// From the start of its first range to the end of its last one.
    pub(crate) fn span(&self) -> Range<usize> {
        let start = self.ranges().map(|range| range.start_byte).min();
        let end = self.ranges().map(|range| range.end_byte).max();
        start.unwrap_or(0)..end.unwrap_or(0)
    }

    /// The ranges, sorted and without overlaps, as a parser takes them.
    fn included_ranges(&self) -> Vec<tree_sitter::Range> {
        let mut ranges: Vec<tree_sitter::Range> = self
            .ranges()
            .filter(|range| range.start_byte < range.end_byte)
            .copied()
            .collect();
        ranges.sort_by_key(|range| range.start_byte);
        let mut end = 0;
        ranges.retain(|range| {
            let keep = range.start_byte >= end;
            if keep {
                end = range.end_byte;
            }
            keep
        });
        ranges
    }
}

/// Shifts the layers by the edits of one change (ascending, in the old text's coordinates; applied
/// last first, as to the tree) and marks those whose text they touch.
pub(crate) fn edit_layers(layers: &mut [Layer], edits: &[InputEdit]) {
    for layer in layers {
        for edit in edits.iter().rev() {
            if layer.ranges().any(|range| {
                edit.start_byte <= range.end_byte && edit.old_end_byte >= range.start_byte
            }) {
                layer.dirty = true;
            }
            for part in &mut layer.parts {
                part.extent = edit_bytes(&part.extent, edit);
                part.content = edit_bytes(&part.content, edit);
                for range in &mut part.ranges {
                    let (start_byte, start_point) =
                        edit_position(range.start_byte, range.start_point, edit);
                    let (end_byte, end_point) =
                        edit_position(range.end_byte, range.end_point, edit);
                    *range = tree_sitter::Range {
                        start_byte,
                        end_byte,
                        start_point,
                        end_point,
                    };
                }
            }
            if let Some(tree) = &mut layer.tree {
                tree.edit(edit);
            }
        }
    }
}

/// A byte range after an edit, as tree-sitter moves included ranges.
pub(crate) fn edit_bytes(range: &Range<usize>, edit: &InputEdit) -> Range<usize> {
    let start = edit_byte(range.start, edit);
    start..edit_byte(range.end, edit).max(start)
}

/// A byte after an edit: one after the replaced text moves with it, one inside it goes to its start.
fn edit_byte(byte: usize, edit: &InputEdit) -> usize {
    if byte >= edit.old_end_byte {
        byte - edit.old_end_byte + edit.new_end_byte
    } else {
        byte.min(edit.start_byte)
    }
}

/// [`edit_byte`] with the point.
fn edit_position(byte: usize, point: Point, edit: &InputEdit) -> (usize, Point) {
    if byte >= edit.old_end_byte {
        let old_end = edit.old_end_position;
        let new_end = edit.new_end_position;
        let moved = if point.row > old_end.row {
            Point {
                row: new_end.row + (point.row - old_end.row),
                column: point.column,
            }
        } else {
            Point {
                row: new_end.row,
                column: new_end.column + point.column.saturating_sub(old_end.column),
            }
        };
        (edit_byte(byte, edit), moved)
    } else if byte > edit.start_byte {
        (edit.start_byte, edit.start_position)
    } else {
        (byte, point)
    }
}

/// Byte ranges sorted, with the overlapping and adjacent ones merged.
pub(crate) fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The layers' part of a parse job: after the document's tree, finds the injections in it and
/// parses them, then the injections in those, and so on. Resumable: a parse within a budget stops
/// before a layer it can't finish (or mustn't parse on the UI thread) and the background run goes
/// on from there.
pub(crate) struct Injector {
    /// The layers before the job, edited to match its text.
    old: Vec<Layer>,
    /// Where the text changed since the old trees were parsed; `None` — look everywhere (the
    /// first parse, the languages changed).
    edited: Option<Vec<Range<usize>>>,
    new: Vec<Layer>,
    /// For each new layer: the old one it continues, and its parse.
    state: Vec<NewState>,
    /// The new layers to parse (when needed) and look into, in order: parents before children.
    queue: VecDeque<usize>,
    /// The document's own injections are found.
    started: bool,
    /// The parser of the layers, kept between jobs.
    parser: Option<Parser>,
    /// It has a store for WebAssembly grammars.
    wasm_store: bool,
}

struct NewState {
    /// The old layer it continues (the same language): its tree, its embedded layers.
    origin: Option<usize>,
    /// It is to be parsed.
    parse: bool,
    /// It was parsed by this job: the layers in it are looked for again.
    parsed: bool,
}

/// Where a job stopped.
pub(crate) struct Interrupted;

/// What layers are looked into: the document or one of the new layers.
#[derive(Clone, Copy)]
enum HostRef {
    Document,
    Layer(usize),
}

/// A match turned into an injection.
struct Found {
    language: Arc<Language>,
    combined: Option<usize>,
    part: Part,
}

impl Injector {
    pub(crate) fn new(
        old: Vec<Layer>,
        edited: Option<Vec<Range<usize>>>,
        parser: Option<Parser>,
    ) -> Self {
        Self {
            old,
            edited,
            new: Vec::new(),
            state: Vec::new(),
            queue: VecDeque::new(),
            started: false,
            wasm_store: false,
            parser,
        }
    }

    /// Whether the job may parse a WebAssembly grammar: an old or a found layer's.
    pub(crate) fn has_wasm(&self) -> bool {
        self.old
            .iter()
            .chain(&self.new)
            .any(|layer| layer.language.is_wasm())
    }

    /// The new layers and the parser, once [`Injector::run`] has finished.
    pub(crate) fn into_parts(self) -> (Vec<Layer>, Option<Parser>) {
        (self.new, self.parser)
    }

    /// Finds and parses the layers of the document whose new tree is `tree` (`old_tree` — its
    /// edited old one). With a `deadline` (on the UI thread) it stops at a layer of a WebAssembly
    /// grammar, at a language not loaded yet and at the deadline.
    pub(crate) fn run(
        &mut self,
        text: &Rope,
        language: &Arc<Language>,
        tree: &Tree,
        old_tree: Option<&Tree>,
        deadline: Option<Instant>,
    ) -> ControlFlow<Interrupted> {
        if !self.started {
            self.started = true;
            if language.has_injections() {
                self.inject(text, HostRef::Document, language, tree, old_tree);
            }
        }
        while let Some(&index) = self.queue.front() {
            if self.state[index].parse {
                let language = &self.new[index].language;
                if deadline.is_some() && (language.is_wasm() || !language.is_ready()) {
                    return ControlFlow::Break(Interrupted);
                }
                self.parse(text, index, deadline)?;
            }
            self.queue.pop_front();
            let layer = &self.new[index];
            if let Some(tree) = layer.tree.clone() {
                let language = layer.language.clone();
                let old_tree = self.state[index]
                    .origin
                    .and_then(|origin| self.old[origin].tree.clone());
                self.inject(
                    text,
                    HostRef::Layer(index),
                    &language,
                    &tree,
                    old_tree.as_ref(),
                );
            }
        }
        ControlFlow::Continue(())
    }

    /// Parses the new layer `index`, from its old tree when it continues one.
    fn parse(
        &mut self,
        text: &Rope,
        index: usize,
        deadline: Option<Instant>,
    ) -> ControlFlow<Interrupted> {
        let language = self.new[index].language.clone();
        let ranges = self.new[index].included_ranges();
        let old_tree = self.state[index]
            .origin
            .and_then(|origin| self.old[origin].tree.as_ref());
        let parser = self.parser.get_or_insert_with(Parser::new);
        let mut tree = None;
        let mut timed_out = false;
        let ready = !ranges.is_empty()
            && (!language.is_wasm()
                || self.wasm_store
                || crate::wasm::new_store()
                    .ok()
                    .and_then(|store| parser.set_wasm_store(store).ok())
                    .is_some())
            && language
                .grammar()
                .is_some_and(|grammar| parser.set_language(grammar).is_ok())
            && parser.set_included_ranges(&ranges).is_ok();
        if language.is_wasm() && ready {
            self.wasm_store = true;
        }
        if ready {
            // Compiled here, off the UI thread, for the highlighting and the layers inside.
            language.query();
            language.injections();
            let _watch = language
                .is_wasm()
                .then(|| crate::wasm::Watch::start(&language));
            let mut read = |byte: usize, _: Point| chunk_from(text, byte);
            tree = match deadline {
                None => parser.parse_with_options(&mut read, old_tree, None),
                Some(deadline) => {
                    let mut progress = |_: &ParseState| {
                        if Instant::now() >= deadline {
                            timed_out = true;
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    };
                    let options = ParseOptions::new().progress_callback(&mut progress);
                    parser.parse_with_options(&mut read, old_tree, Some(options))
                }
            };
        }
        if timed_out {
            // The next run parses it again from the start.
            parser.reset();
            return ControlFlow::Break(Interrupted);
        }
        let layer = &mut self.new[index];
        layer.tree = tree;
        layer.dirty = false;
        self.state[index].parse = false;
        self.state[index].parsed = true;
        ControlFlow::Continue(())
    }

    /// Finds the layers embedded in a host — the document or a new layer — and queues them.
    fn inject(
        &mut self,
        text: &Rope,
        host: HostRef,
        language: &Arc<Language>,
        tree: &Tree,
        old_tree: Option<&Tree>,
    ) {
        let (parent, depth, host_ranges, origin, reparsed) = match host {
            HostRef::Document => (None, 1, None, Some(None), true),
            HostRef::Layer(index) => {
                let layer = &self.new[index];
                let state = &self.state[index];
                (
                    Some(index),
                    layer.depth + 1,
                    Some(layer.included_ranges()),
                    state.origin.map(Some),
                    state.parsed,
                )
            }
        };
        // The host's old layers: those of the document, or of the old layer it continues.
        let old_children: Vec<usize> = match origin {
            Some(old_parent) => (0..self.old.len())
                .filter(|&old| self.old[old].parent == old_parent)
                .collect(),
            None => Vec::new(),
        };
        if depth > MAX_DEPTH || self.new.len() >= MAX_LAYERS {
            return;
        }
        // The layers found, with the old ones they continue and whether they are to be parsed:
        // queued in the order of their text.
        let mut children: Vec<(Layer, Option<usize>, bool)> = Vec::new();
        if !reparsed && self.edited.is_some() {
            // The host is as it was: so are the layers in it.
            for old in old_children {
                children.push((self.old[old].clone(), Some(old), false));
            }
            self.push_all(children, parent);
            return;
        }
        let Some(query) = language.injections() else {
            return;
        };
        let regions = match (&self.edited, old_tree) {
            (Some(edited), Some(old_tree)) => Some(merge_ranges(
                old_tree
                    .changed_ranges(tree)
                    .map(|range| range.start_byte..range.end_byte)
                    .chain(edited.iter().cloned())
                    .collect(),
            )),
            _ => None,
        };
        let touched = |extent: &Range<usize>| {
            regions.as_ref().is_none_or(|regions| {
                regions
                    .iter()
                    .any(|region| region.start <= extent.end && extent.start <= region.end)
            })
        };
        let found = find(
            query,
            tree,
            text,
            host_ranges.as_deref(),
            regions.as_deref(),
        );
        let overlaps_found = |layer: &Layer| {
            let span = layer.span();
            found.iter().any(|found| {
                found
                    .part
                    .ranges
                    .iter()
                    .any(|range| range.start_byte < span.end && span.start < range.end_byte)
            })
        };

        // The old layers the changes don't reach stay as they are; the others may lend their trees
        // to the layers found again.
        let mut lenders = Vec::new();
        let mut combined_old: Vec<(usize, Vec<Part>)> = Vec::new();
        for &old in &old_children {
            let layer = &self.old[old];
            if layer.combined.is_some() {
                let kept = layer
                    .parts
                    .iter()
                    .filter(|part| !touched(&part.extent))
                    .cloned()
                    .collect();
                combined_old.push((old, kept));
            } else if layer.parts.iter().any(|part| touched(&part.extent)) || overlaps_found(layer)
            {
                lenders.push(old);
            } else {
                children.push((layer.clone(), Some(old), false));
            }
        }

        let mut combined_new: Vec<(Arc<Language>, usize, Vec<Part>)> = Vec::new();
        for found in found {
            let Found {
                language,
                combined,
                part,
            } = found;
            if let Some(pattern) = combined {
                match combined_new.iter_mut().find(|(known, known_pattern, _)| {
                    Arc::ptr_eq(known, &language) && *known_pattern == pattern
                }) {
                    Some((_, _, parts)) => parts.push(part),
                    None => combined_new.push((language, pattern, vec![part])),
                }
                continue;
            }
            let same = |old: &usize| Arc::ptr_eq(&self.old[*old].language, &language);
            // The same text untouched: the old tree as it is; otherwise the old tree of the same
            // language over the same text, to reparse from.
            let unchanged = lenders.iter().position(|old| {
                let layer = &self.old[*old];
                same(old)
                    && !layer.dirty
                    && layer.tree.is_some()
                    && layer.parts.len() == 1
                    && layer.parts[0].ranges == part.ranges
            });
            let layer = Layer {
                language: language.clone(),
                depth,
                parent,
                parts: vec![part],
                combined: None,
                tree: None,
                dirty: false,
            };
            if let Some(position) = unchanged {
                let old = lenders.remove(position);
                let mut layer = layer;
                layer.tree = self.old[old].tree.clone();
                children.push((layer, Some(old), false));
                continue;
            }
            let span = layer.span();
            let lender = lenders.iter().position(|old| {
                let old_span = self.old[*old].span();
                same(old) && old_span.start < span.end && span.start < old_span.end
            });
            let origin = lender.map(|position| lenders.remove(position));
            children.push((layer, origin, true));
        }

        // Combined: the old layer's parts the changes don't reach, with the ones found again.
        for (old, kept) in &combined_old {
            let layer = &self.old[*old];
            let pattern = layer.combined.unwrap_or_default();
            let position = combined_new.iter().position(|(language, known, _)| {
                Arc::ptr_eq(language, &layer.language) && *known == pattern
            });
            let mut parts = kept.clone();
            if let Some(position) = position {
                parts.extend(combined_new.remove(position).2);
            }
            parts.sort_by_key(|part| part.extent.start);
            parts.dedup_by(|a, b| a.ranges == b.ranges);
            if parts.is_empty() {
                continue;
            }
            let unchanged = !layer.dirty && layer.tree.is_some() && parts == layer.parts;
            let new = Layer {
                parts,
                tree: unchanged.then(|| layer.tree.clone()).flatten(),
                dirty: false,
                parent,
                depth,
                ..layer.clone()
            };
            children.push((new, Some(*old), !unchanged));
        }
        for (language, pattern, mut parts) in combined_new {
            parts.sort_by_key(|part| part.extent.start);
            parts.dedup_by(|a, b| a.ranges == b.ranges);
            let layer = Layer {
                language,
                depth,
                parent,
                parts,
                combined: Some(pattern),
                tree: None,
                dirty: false,
            };
            children.push((layer, None, true));
        }
        self.push_all(children, parent);
    }

    /// Adds the layers of a host and queues them, in the order of their text.
    fn push_all(&mut self, mut children: Vec<(Layer, Option<usize>, bool)>, parent: Option<usize>) {
        children.sort_by_key(|(layer, ..)| (layer.span().start, layer.span().end));
        for (mut layer, origin, parse) in children {
            if self.new.len() >= MAX_LAYERS {
                return;
            }
            layer.parent = parent;
            let parse = parse || layer.tree.is_none();
            self.new.push(layer);
            self.state.push(NewState {
                origin,
                parse,
                parsed: false,
            });
            self.queue.push_back(self.new.len() - 1);
        }
    }
}

/// The injections a query finds in `tree`, within `regions` (or everywhere): those of installed
/// languages with text in the host's ranges.
fn find(
    query: &InjectionQuery,
    tree: &Tree,
    text: &Rope,
    host_ranges: Option<&[tree_sitter::Range]>,
    regions: Option<&[Range<usize>]>,
) -> Vec<Found> {
    let windows: Vec<Range<usize>> = match regions {
        None => std::iter::once(0..usize::MAX).collect(),
        Some([]) => Vec::new(),
        Some(regions) if regions.len() > MAX_REGIONS => {
            std::iter::once(regions[0].start..regions[regions.len() - 1].end).collect()
        }
        Some(regions) => regions.to_vec(),
    };
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    let mut found: Vec<Found> = Vec::new();
    for window in windows {
        // A node that ends where the region starts is touched by it too.
        cursor.set_byte_range(window.start.saturating_sub(1)..window.end.saturating_add(1));
        let text_provider = |node: Node| byte_chunks(text, node.byte_range());
        let mut matches = cursor.matches(&query.query, tree.root_node(), text_provider);
        while let Some(m) = matches.next() {
            let settings = &query.patterns[m.pattern_index];
            let name = settings.language.clone().or_else(|| {
                let index = query.language?;
                let node = m.nodes_for_capture_index(index).next()?;
                node_text(text, node)
            });
            let Some(language) = name.as_deref().and_then(injected_language) else {
                continue;
            };
            if language.grammar_failed() {
                continue;
            }
            let content: Vec<Node> = m.nodes_for_capture_index(query.content).collect();
            if content.is_empty() {
                continue;
            }
            let mut ranges = intersect_ranges(host_ranges, &content, settings.include_children);
            // Zero-width children (Markdown's line starts) leave ranges that touch: one is enough.
            ranges.dedup_by(|next, range| {
                let touch = range.end_byte == next.start_byte;
                if touch {
                    range.end_byte = next.end_byte;
                    range.end_point = next.end_point;
                }
                touch
            });
            if ranges
                .iter()
                .all(|range| range.start_byte >= range.end_byte)
            {
                continue;
            }
            let start = m.captures().iter().map(|c| c.node.start_byte()).min();
            let end = m.captures().iter().map(|c| c.node.end_byte()).max();
            let content_start = content.iter().map(Node::start_byte).min();
            let content_end = content.iter().map(Node::end_byte).max();
            let part = Part {
                extent: start.unwrap_or(0)..end.unwrap_or(0),
                content: content_start.unwrap_or(0)..content_end.unwrap_or(0),
                ranges,
            };
            let combined = settings.combined.then_some(m.pattern_index);
            let duplicate = found.iter().any(|known| {
                known.combined == combined
                    && Arc::ptr_eq(&known.language, &language)
                    && known.part.ranges == part.ranges
            });
            if !duplicate {
                found.push(Found {
                    language,
                    combined,
                    part,
                });
            }
        }
    }
    found
}

/// The text of a node naming a language, without spaces and quotes around it (`"ts"` → `ts`);
/// short, so a whole block isn't copied by mistake.
fn node_text(text: &Rope, node: Node) -> Option<String> {
    let range = node.byte_range();
    if range.len() > 64 {
        return None;
    }
    let bytes: Vec<u8> = byte_chunks(text, range).flatten().copied().collect();
    let name = String::from_utf8(bytes).ok()?;
    let name = name.trim();
    let unquoted = ['"', '\'', '`'].iter().find_map(|&quote| {
        name.strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
    });
    Some(unquoted.unwrap_or(name).trim().to_string())
}

/// The ranges of the content nodes, without their named children unless `include_children`,
/// within the host's ranges (`None` — the whole document): an embedded document never takes text
/// its host doesn't have (the `>` of a quote around a code block). As in tree-sitter-highlight.
fn intersect_ranges(
    host_ranges: Option<&[tree_sitter::Range]>,
    nodes: &[Node],
    include_children: bool,
) -> Vec<tree_sitter::Range> {
    const WHOLE: tree_sitter::Range = tree_sitter::Range {
        start_byte: 0,
        end_byte: usize::MAX,
        start_point: Point { row: 0, column: 0 },
        end_point: Point {
            row: usize::MAX,
            column: usize::MAX,
        },
    };
    let host_ranges = host_ranges.unwrap_or(std::slice::from_ref(&WHOLE));
    let mut result = Vec::new();
    let mut hosts = host_ranges.iter();
    let Some(mut host) = hosts.next() else {
        return result;
    };
    for node in nodes {
        let mut cursor = node.walk();
        let mut preceding_end = (node.start_byte(), node.start_position());
        let following = tree_sitter::Range {
            start_byte: node.end_byte(),
            start_point: node.end_position(),
            end_byte: usize::MAX,
            end_point: WHOLE.end_point,
        };
        // Named children only: the punctuation a grammar leaves as anonymous tokens in the
        // content (Markdown's code) is the content's text.
        let children: Vec<tree_sitter::Range> = if include_children {
            Vec::new()
        } else {
            node.named_children(&mut cursor)
                .map(|child| child.range())
                .collect()
        };
        for excluded in children.into_iter().chain(std::iter::once(following)) {
            let mut range = tree_sitter::Range {
                start_byte: preceding_end.0,
                start_point: preceding_end.1,
                end_byte: excluded.start_byte,
                end_point: excluded.start_point,
            };
            preceding_end = (excluded.end_byte, excluded.end_point);
            if range.end_byte < host.start_byte {
                continue;
            }
            while host.start_byte <= range.end_byte {
                if host.end_byte > range.start_byte {
                    if range.start_byte < host.start_byte {
                        range.start_byte = host.start_byte;
                        range.start_point = host.start_point;
                    }
                    if host.end_byte < range.end_byte {
                        if range.start_byte < host.end_byte {
                            result.push(tree_sitter::Range {
                                end_byte: host.end_byte,
                                end_point: host.end_point,
                                ..range
                            });
                        }
                        range.start_byte = host.end_byte;
                        range.start_point = host.end_point;
                    } else {
                        if range.start_byte < range.end_byte {
                            result.push(range);
                        }
                        break;
                    }
                }
                match hosts.next() {
                    Some(next) => host = next,
                    None => return result,
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, old_end: usize, new_end: usize) -> InputEdit {
        let point = |byte: usize| Point {
            row: 0,
            column: byte,
        };
        InputEdit {
            start_byte: start,
            old_end_byte: old_end,
            new_end_byte: new_end,
            start_position: point(start),
            old_end_position: point(old_end),
            new_end_position: point(new_end),
        }
    }

    #[test]
    fn byte_ranges_move_like_included_ranges() {
        // Before, after, around, inside, and an insertion at each end.
        assert_eq!(edit_bytes(&(0..5), &edit(10, 12, 20)), 0..5);
        assert_eq!(edit_bytes(&(20..30), &edit(10, 12, 20)), 28..38);
        assert_eq!(edit_bytes(&(5..30), &edit(10, 12, 20)), 5..38);
        assert_eq!(edit_bytes(&(11..12), &edit(10, 15, 10)), 10..10);
        assert_eq!(edit_bytes(&(10..20), &edit(10, 10, 13)), 13..23);
        assert_eq!(edit_bytes(&(10..20), &edit(20, 20, 23)), 10..23);
    }

    #[test]
    fn ranges_merge() {
        assert_eq!(
            merge_ranges(vec![8..9, 0..2, 1..4, 4..5, 7..7]),
            vec![0..5, 7..7, 8..9]
        );
    }
}
