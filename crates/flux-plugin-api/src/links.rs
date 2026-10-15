//! Links on text that names something of the plugin's (API 0.3) — a task's key, `TRACK-142` — in
//! the editor (⌘ + hover underlines it, ⌘-click), in commit messages, in Claude's chat and in the
//! plugins' Markdown. A click comes as `Event::LinkActivated((pattern, text))`; with a tooltip,
//! a hover asks for it with `Event::LinkHovered((request, pattern, text))`, and the plugin answers
//! with [`tooltip`] (or [`no_tooltip`]).
//!
//! ```ignore
//! links::set([links::pattern("task", &links::key_regex(&["TRACK", "WEB"])).tooltip()])?;
//! // Event::LinkHovered((request, _, key)) → links::tooltip(request, &format!("**{key}** …"))
//! // Event::LinkActivated((_, key)) → open the task
//! ```

use crate::host::links::{self as raw, LinkPattern};

/// A pattern being built: [`pattern`], then [`Pattern::tooltip`].
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern(LinkPattern);

/// A pattern named `id` (in the events) matching `regex` (Rust `regex` syntax; the whole match
/// is the link).
pub fn pattern(id: &str, regex: &str) -> Pattern {
    Pattern(LinkPattern {
        id: id.to_string(),
        regex: regex.to_string(),
        tooltip: false,
    })
}

impl Pattern {
    /// The plugin describes a link on hover.
    pub fn tooltip(mut self) -> Self {
        self.0.tooltip = true;
        self
    }
}

/// Replaces the plugin's patterns; the error names a regex that doesn't compile.
pub fn set(patterns: impl IntoIterator<Item = Pattern>) -> Result<(), String> {
    let patterns: Vec<LinkPattern> = patterns.into_iter().map(|pattern| pattern.0).collect();
    raw::set_patterns(&patterns)
}

/// Removes the plugin's patterns.
pub fn clear() {
    let _ = raw::set_patterns(&[]);
}

/// The answer to `Event::LinkHovered`: Markdown for the tooltip.
pub fn tooltip(request: u64, markdown: &str) {
    raw::describe(request, Some(markdown))
}

/// The answer to `Event::LinkHovered`: no tooltip for this one.
pub fn no_tooltip(request: u64) {
    raw::describe(request, None)
}

/// A regex of keys like `TRACK-142` with one of these prefixes (queues, projects):
/// `\b(?:TRACK|WEB)-\d+\b`; no prefixes — any key of capital letters.
pub fn key_regex(prefixes: &[&str]) -> String {
    let prefixes: Vec<String> = prefixes
        .iter()
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| escape(prefix))
        .collect();
    if prefixes.is_empty() {
        return r"\b[A-Z][A-Z0-9_]*-\d+\b".to_string();
    }
    format!(r"\b(?:{})-\d+\b", prefixes.join("|"))
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !c.is_alphanumeric() && c != '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_patterns() {
        assert_eq!(key_regex(&["TRACK", "WEB"]), r"\b(?:TRACK|WEB)-\d+\b");
        assert_eq!(key_regex(&[]), r"\b[A-Z][A-Z0-9_]*-\d+\b");
        assert_eq!(key_regex(&["A.B"]), r"\b(?:A\.B)-\d+\b");
        let task = pattern("task", "x").tooltip();
        assert!(task.0.tooltip);
        assert_eq!(task.0.id, "task");
    }
}
