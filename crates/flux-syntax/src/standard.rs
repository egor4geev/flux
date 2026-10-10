//! The eleven languages Flux had before stage 8.3, with their upstream queries and native grammars:
//! the tests' languages. Flux itself gets its languages from plugins (JavaScript and TypeScript
//! from the bundled `flux.javascript`, the rest from the catalog, with WebAssembly grammars).
//!
//! Only with the feature `standard-languages`, which only dev-dependencies turn on (this crate's
//! tests and flux-app's): a product build has none of these grammars.

use crate::language::{self, GrammarSource, LanguageConfig, Precedence};

/// The owner the standard languages are registered under.
pub const OWNER: &str = "flux.standard";

/// JavaScript function parameters: the file is in the grammar but not in its Rust binding.
pub const JAVASCRIPT_PARAMS_QUERY: &str = include_str!("../queries/javascript-params.scm");

/// Registers the standard languages (again: the same configs keep their compiled queries).
pub fn register() {
    register_as(OWNER);
}

/// Registers them under another owner.
pub fn register_as(owner: &str) {
    language::register(owner, configs());
}

/// The standard languages' configs.
pub fn configs() -> Vec<LanguageConfig> {
    let language = |name: &str,
                    display_name: &str,
                    extensions: &[&str],
                    file_names: &[&str],
                    aliases: &[&str],
                    queries: &[&str],
                    precedence: Precedence| LanguageConfig {
        name: name.into(),
        display_name: display_name.into(),
        extensions: extensions.iter().map(|s| s.to_string()).collect(),
        file_names: file_names.iter().map(|s| s.to_string()).collect(),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        grammar: GrammarSource::Builtin(name.into()),
        highlights: queries.join("\n"),
        precedence,
    };
    vec![
        language(
            "rust",
            "Rust",
            &["rs"],
            &[],
            &["rs"],
            &[tree_sitter_rust::HIGHLIGHTS_QUERY],
            Precedence::LastPattern,
        ),
        language(
            "toml",
            "TOML",
            &["toml"],
            &["Cargo.lock", "Pipfile", "poetry.lock", "uv.lock"],
            &[],
            &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
            Precedence::LastPattern,
        ),
        // The JSON query puts the keys `(pair key: …)` before the generic `(string)`.
        language(
            "json",
            "JSON",
            &["json", "jsonc"],
            &["flake.lock"],
            &["jsonc"],
            &[tree_sitter_json::HIGHLIGHTS_QUERY],
            Precedence::FirstPattern,
        ),
        // Block grammar only: inline markup and code in blocks are injections.
        language(
            "markdown",
            "Markdown",
            &["md", "markdown"],
            &[],
            &["md"],
            &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
            Precedence::LastPattern,
        ),
        language(
            "yaml",
            "YAML",
            &["yaml", "yml"],
            &[".clang-format", ".clang-tidy"],
            &["yml"],
            &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
            Precedence::LastPattern,
        ),
        language(
            "bash",
            "Shell",
            &["sh", "bash", "zsh"],
            &[
                ".bashrc",
                ".bash_profile",
                ".bash_aliases",
                ".bash_logout",
                ".profile",
                ".zshrc",
                ".zshenv",
                ".zprofile",
                ".zlogin",
                ".zlogout",
                "PKGBUILD",
            ],
            &["sh", "shell", "zsh", "console"],
            &[tree_sitter_bash::HIGHLIGHT_QUERY],
            Precedence::LastPattern,
        ),
        language(
            "python",
            "Python",
            &["py", "pyi", "pyw"],
            &[],
            &["py", "python3"],
            &[tree_sitter_python::HIGHLIGHTS_QUERY],
            Precedence::LastPattern,
        ),
        // The Go query puts `(identifier) @variable` and `(field_identifier) @property` after
        // function and method calls.
        language(
            "go",
            "Go",
            &["go"],
            &[],
            &["golang"],
            &[tree_sitter_go::HIGHLIGHTS_QUERY],
            Precedence::FirstPattern,
        ),
        // The order of the parts follows the grammar's tree-sitter.json.
        language(
            "javascript",
            "JavaScript",
            &["js", "mjs", "cjs", "jsx"],
            &[],
            &["js", "jsx", "node"],
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                JAVASCRIPT_PARAMS_QUERY,
            ],
            Precedence::LastPattern,
        ),
        // The TypeScript query supplements the JavaScript query. The grammar's tree-sitter.json
        // lists it first, which is the order from the "earlier pattern wins" days. With the current
        // "later pattern wins" rule, the TypeScript parts must come after JavaScript, otherwise the
        // generic `(identifier) @variable` from JS would override them.
        language(
            "typescript",
            "TypeScript",
            &["ts", "mts", "cts"],
            &[],
            &["ts"],
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
            Precedence::LastPattern,
        ),
        language(
            "tsx",
            "TSX",
            &["tsx"],
            &[],
            &[],
            &[
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
            ],
            Precedence::LastPattern,
        ),
    ]
}
