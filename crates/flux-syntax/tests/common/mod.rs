//! Shared by the integration tests: a deterministic PRNG, random multi-cursor edits, tree
//! comparison.

#![allow(dead_code)]

pub mod samples;

use std::fmt::Write;

use flux_core::transaction::Operation;
use flux_core::{ChangeSet, Rope};
use std::path::Path;
use std::sync::Arc;

use flux_syntax::tree_sitter::{InputEdit, Parser, Point, Tree};
use flux_syntax::{Language, Syntax};

/// The standard languages (registered on first use) by name.
pub fn language_by_name(name: &str) -> Option<Arc<Language>> {
    flux_syntax::standard::register();
    flux_syntax::language_by_name(name)
}

/// The standard languages, registered on first use; other tests' languages are left out.
pub fn languages() -> Vec<Arc<Language>> {
    flux_syntax::standard::register();
    flux_syntax::languages()
        .into_iter()
        .filter(|language| language.owner() == flux_syntax::standard::OWNER)
        .collect()
}

/// The standard language of a file.
pub fn language_for_path(path: &Path) -> Option<Arc<Language>> {
    flux_syntax::standard::register();
    flux_syntax::language_for_path(path)
}

/// SplitMix64: dependency-free and identical on any machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`; `n > 0`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Insertions common to all languages: multibyte characters, newlines, brackets and quotes that
/// break and repair the parse.
pub const COMMON_SNIPPETS: &[&str] = &[
    "x",
    " ",
    "\n",
    "\r\n",
    "\n\n",
    "ы",
    "привет",
    "👍🏽",
    "🦀",
    "\t",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "\"",
    "'",
    "#",
    "//",
    "/*",
    "*/",
    "=",
    ",",
    ":",
    ";",
    "    ",
];

/// A random ChangeSet: one to four ranges (multi-cursor), insertions, deletions (including ones
/// spanning several lines), replacements, and occasionally an insertion at the very end.
pub fn random_changes(rng: &mut Rng, text: &Rope, snippets: &[&str]) -> ChangeSet {
    let len = text.len_chars();
    let snippet = |rng: &mut Rng| -> String {
        let pool = if rng.chance(50) {
            snippets
        } else {
            COMMON_SNIPPETS
        };
        (0..1 + rng.below(2)).map(|_| *rng.pick(pool)).collect()
    };
    let shrink = len > 3000;
    let count = 1 + rng.below(4);
    let mut starts: Vec<usize> = (0..count).map(|_| rng.below(len + 1)).collect();
    starts.sort_unstable();
    let mut changes = Vec::new();
    for from in starts {
        let to = match rng.below(if shrink { 3 } else { 6 }) {
            0 => {
                // A deletion spanning several lines.
                let line = text.char_to_line(from);
                let last = (line + 1 + rng.below(3)).min(text.len_lines() - 1);
                text.line_to_char(last) + rng.below(text.line(last).len_chars() + 1)
            }
            1 => from + rng.below(40),
            2 => from + rng.below(4),
            _ => from,
        };
        let insert = (!shrink || rng.chance(30)).then(|| snippet(rng));
        let insert = if to == from && insert.is_none() {
            Some(snippet(rng))
        } else {
            insert
        };
        changes.push((from, to.min(len), insert));
    }
    if rng.chance(10) {
        changes.push((len, len, Some(snippet(rng))));
    }
    ChangeSet::from_changes(len, changes)
}

pub fn fresh_tree(language: &Language, text: &Rope) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(language.grammar().unwrap()).unwrap();
    parser.parse(text.to_string(), None).unwrap()
}

/// Parses to completion right here, as a background thread would.
pub fn parse_now(syntax: &mut Syntax, text: &Rope) {
    if let Some(job) = syntax.parse_job(text) {
        assert!(syntax.finish(job.run()));
    }
}

/// All nodes of the tree, including anonymous ones, with their bytes and points.
pub fn dump(tree: &Tree) -> String {
    let mut out = String::new();
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        let node = cursor.node();
        let _ = writeln!(
            out,
            "{:indent$}{}{}{} {:?} {}..{} {}-{}",
            "",
            node.kind(),
            if node.is_missing() { " MISSING" } else { "" },
            if node.is_error() { " ERROR" } else { "" },
            cursor.field_name(),
            node.start_byte(),
            node.end_byte(),
            node.start_position(),
            node.end_position(),
            indent = depth * 2,
        );
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
            depth -= 1;
        }
    }
}

/// Points by tree-sitter's definition: lines are split on `\n`, the column is in bytes.
pub struct PointIndex {
    row_starts: Vec<usize>,
}

impl PointIndex {
    pub fn new(text: &str) -> Self {
        let mut row_starts = vec![0];
        row_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { row_starts }
    }

    pub fn point(&self, byte: usize) -> Point {
        let row = self.row_starts.partition_point(|&start| start <= byte) - 1;
        Point::new(row, byte - self.row_starts[row])
    }
}

/// The reference for tree edits: the same edits the crate builds, but with coordinates computed
/// naively on a `String` (character → byte by iteration, points via a line table).
pub struct NaiveTree {
    parser: Parser,
    pub tree: Tree,
}

impl NaiveTree {
    pub fn new(language: &Language, text: &str) -> Self {
        let mut parser = Parser::new();
        parser.set_language(language.grammar().unwrap()).unwrap();
        let tree = parser.parse(text, None).unwrap();
        Self { parser, tree }
    }

    pub fn edit(&mut self, old: &str, changes: &ChangeSet) {
        let index = PointIndex::new(old);
        let byte_of = |char_idx: usize| {
            old.char_indices()
                .nth(char_idx)
                .map_or(old.len(), |(b, _)| b)
        };
        let mut edits = Vec::new();
        let mut pos = 0;
        let mut ops = changes.ops().iter().peekable();
        while let Some(op) = ops.next() {
            let mut inserted = String::new();
            let start = pos;
            match op {
                Operation::Retain(n) => {
                    pos += n;
                    continue;
                }
                Operation::Delete(n) => pos += n,
                Operation::Insert(s) => inserted.push_str(s),
            }
            while let Some(op) = ops.next_if(|op| !matches!(op, Operation::Retain(_))) {
                match op {
                    Operation::Delete(n) => pos += n,
                    Operation::Insert(s) => inserted.push_str(s),
                    Operation::Retain(_) => unreachable!(),
                }
            }
            let start_byte = byte_of(start);
            let old_end_byte = byte_of(pos);
            let mut new_text = old[..start_byte].to_string();
            new_text.push_str(&inserted);
            let new_index = PointIndex::new(&new_text);
            edits.push(InputEdit {
                start_byte,
                old_end_byte,
                new_end_byte: new_text.len(),
                start_position: index.point(start_byte),
                old_end_position: index.point(old_end_byte),
                new_end_position: new_index.point(new_text.len()),
            });
        }
        for edit in edits.iter().rev() {
            self.tree.edit(edit);
        }
    }

    pub fn reparse(&mut self, text: &str) {
        self.tree = self.parser.parse(text, Some(&self.tree)).unwrap();
    }
}

/// Every tree node lies within the text, and its points agree with its bytes.
pub fn check_points(tree: &Tree, text: &str) -> Result<(), String> {
    let index = PointIndex::new(text);
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.end_byte() > text.len() {
            return Err(format!("{node:?} ends after the text ({})", text.len()));
        }
        for (byte, point) in [
            (node.start_byte(), node.start_position()),
            (node.end_byte(), node.end_position()),
        ] {
            if index.point(byte) != point {
                return Err(format!(
                    "{node:?}: byte {byte} is {:?}, tree says {point:?}",
                    index.point(byte)
                ));
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
}
