//! The manifest, `flux-plugin.toml` in the plugin's folder: who the plugin is, what it may do and
//! what it adds to the window. Flux reads it before running any of the plugin's code: the manager
//! shows it, the install question lists its permissions, the palette its commands.
//!
//! ```toml
//! id = "flux.todo"
//! name = "TODO"
//! version = "0.1.0"
//! api = "0.3"
//! authors = ["Egor Ageev"]
//! description = "TODO and FIXME comments of the project in a tool window."
//! icon = "icons/todo.svg"
//! wasm = "todo.wasm"
//!
//! [permissions]
//! project = "read"
//! network = ["api.github.com"]
//! processes = ["git"]
//!
//! [[commands]]
//! id = "refresh"
//! title = "Refresh"
//! keys = "alt-cmd-shift-t"
//!
//! [[tool-windows]]
//! id = "todo"
//! title = "TODO"
//! icon = "icons/todo.svg"
//!
//! [[menus]]
//! command = "refresh"
//! location = "tree"
//! when = "folder"
//!
//! [[settings]]
//! key = "patterns"
//! title = "Patterns"
//! type = "string-list"
//! default = ["\\bTODO\\b", "\\bFIXME\\b"]
//! ```
//!
//! API 0.3 adds settings that aren't values — `type = "secret"` (a token kept encrypted),
//! `"button"` (`command`, `argument`), `"status"` (a line the plugin writes), `"section"` (a
//! heading) — commands only buttons and notifications run (`palette = false`), and the `claude`
//! permission.
//!
//! Declarative contributions (stage 8.3) need no code: a language with its tree-sitter grammar and
//! highlighting query, a language server and how Flux installs it, a color theme, a set of file
//! icons.
//!
//! ```toml
//! [[languages]]
//! id = "rust"
//! name = "Rust"
//! extensions = ["rs"]
//! aliases = ["rs"]
//! grammar = "rust"
//! highlights = "languages/rust/highlights.scm"
//! icon = "icons/rust.svg"
//! icon-color = "orange"
//!
//! [[grammars]]
//! id = "rust"
//! wasm = "grammars/rust.wasm"
//!
//! [[language-servers]]
//! id = "rust-analyzer"
//! command = "rust-analyzer"
//! languages = ["rust"]
//! install = { rustup = "rust-analyzer", fallback = { github = "rust-lang/rust-analyzer", asset = "rust-analyzer-{arch}-apple-darwin.gz", bin = "rust-analyzer" } }
//!
//! [[themes]]
//! file = "themes/flux-night.toml"
//!
//! [[icon-themes]]
//! file = "icon-themes/flux.toml"
//! ```

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Lowercase words of letters and digits joined by dots or dashes: `flux.todo`,
    /// `someone.hello-world`. The key of the plugin everywhere: its folder, its settings, its
    /// notification group.
    pub id: String,
    pub name: String,
    pub version: String,
    /// The plugin API version it is built for (`"0.3"`); Flux runs the ones of
    /// [`crate::SUPPORTED_API_VERSIONS`], and a component must be built for the one it names.
    pub api: String,
    pub authors: Vec<String>,
    pub description: String,
    pub repository: Option<String>,
    /// An SVG in the plugin's folder, for the manager.
    pub icon: Option<String>,
    /// The component, relative to the plugin's folder; none — a plugin without code.
    pub wasm: Option<String>,
    pub permissions: Permissions,
    /// The default display of the plugin's notification group (Settings → Notifications).
    pub notifications: NotificationDisplay,
    pub commands: Vec<CommandSpec>,
    pub tool_windows: Vec<ToolWindowSpec>,
    pub status_items: Vec<StatusItemSpec>,
    pub settings: Vec<SettingSpec>,
    /// Items of the context menus that run the plugin's commands.
    pub menus: Vec<MenuSpec>,
    /// Languages: file types with a grammar and a highlighting query (stage 8.3).
    pub languages: Vec<LanguageSpec>,
    /// The tree-sitter grammars of the languages.
    pub grammars: Vec<GrammarSpec>,
    /// Language servers and how Flux installs them.
    pub language_servers: Vec<LanguageServerSpec>,
    /// Color themes: files in the plugin's folder.
    pub themes: Vec<ThemeSpec>,
    /// Sets of file icons: files in the plugin's folder.
    pub icon_themes: Vec<IconThemeSpec>,
}

/// What a plugin may do beyond its own folder and the API's notifications, questions and windows.
/// The install question lists them; the sandbox and the API enforce them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    /// The project's files through the file system, and what the API tells about them: the
    /// search, Git, the problems, the answers to proposals.
    pub project: ProjectAccess,
    /// The hosts the plugin may send requests to, lowercase: "api.example.com", "*.example.com"
    /// (the domain and its subdomains), "localhost" (with 127.0.0.1 and ::1), "*" (any).
    pub network: Vec<String>,
    /// A server of its own on 127.0.0.1.
    pub server: bool,
    /// The programs the plugin may run, by name ("git"), or "*" — any.
    pub processes: Vec<String>,
    /// Terminal tabs the plugin may open and type into.
    pub terminal: bool,
    /// Folders outside the project, open to the plugin through the file system.
    pub folders: Vec<FolderPermission>,
    /// Claude Code sessions the plugin opens with its own context and requests (API 0.3): they
    /// run with the rights the user gave Claude Code.
    pub claude: bool,
}

impl Permissions {
    /// Nothing beyond what every plugin may do.
    pub fn is_empty(&self) -> bool {
        self.project == ProjectAccess::None
            && self.network.is_empty()
            && !self.server
            && self.processes.is_empty()
            && !self.terminal
            && self.folders.is_empty()
            && !self.claude
    }

    /// Whether a request may go to `host`: a name or an IP address, without the port (IPv6 with
    /// or without brackets).
    pub fn allows_host(&self, host: &str) -> bool {
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        self.network.iter().any(|allowed| match allowed.as_str() {
            "*" => true,
            "localhost" => matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"),
            pattern => match pattern.strip_prefix("*.") {
                Some(domain) => {
                    host == domain
                        || host
                            .strip_suffix(domain)
                            .is_some_and(|sub| sub.ends_with('.'))
                }
                None => host == pattern,
            },
        })
    }

    /// Any host at all.
    pub fn any_host(&self) -> bool {
        self.network.iter().any(|host| host == "*")
    }

    /// Whether the plugin may run `program`: a name looked up in `PATH`, or a path — its file
    /// name is what the manifest lists.
    pub fn allows_program(&self, program: &str) -> bool {
        let name = Path::new(program)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        !name.is_empty()
            && self
                .processes
                .iter()
                .any(|allowed| allowed == "*" || *allowed == name)
    }

    /// Any program at all.
    pub fn any_program(&self) -> bool {
        self.processes.iter().any(|program| program == "*")
    }
}

/// A folder outside the project the plugin may read or write.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderPermission {
    /// As the manifest says it: "~/.config/gh" (in the home folder) or an absolute path.
    pub path: String,
    #[serde(default)]
    pub access: FolderAccess,
}

impl FolderPermission {
    /// The folder's absolute path: "~" is the home folder.
    pub fn resolve(&self, home: &Path) -> PathBuf {
        match self.path.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(&self.path),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderAccess {
    #[default]
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectAccess {
    #[default]
    None,
    Read,
    Write,
}

/// How the plugin's notifications show by default (the user changes it in Settings →
/// Notifications).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationDisplay {
    /// A card that goes away by itself.
    #[default]
    Balloon,
    /// A card that stays until closed.
    Sticky,
    /// The Notifications window only.
    Log,
    Hidden,
}

/// A command: the palette shows «category: title», keys run it, and it comes to the plugin's
/// `run-command`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CommandSpec {
    pub id: String,
    pub title: String,
    /// The palette's chip; the plugin's name by default.
    pub category: Option<String>,
    /// A default shortcut in gpui's notation (`alt-cmd-shift-t`); a key Flux already uses is not
    /// taken.
    pub keys: Option<String>,
    /// Shown in the command palette (API 0.3: `palette = false` for a command only a
    /// notification's action, a settings button or a status bar item runs).
    #[serde(default = "shown")]
    pub palette: bool,
}

fn shown() -> bool {
    true
}

/// A tool window in the island on the right, opened by its icon in the launchpad — or, API 0.3,
/// a tab among the editor's tabs (`location = "editor"`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ToolWindowSpec {
    pub id: String,
    pub title: String,
    /// A monochrome 16×16 SVG in the plugin's folder; a puzzle piece by default.
    pub icon: Option<String>,
    /// A default shortcut that opens and closes the window.
    pub keys: Option<String>,
    /// Where the window opens (API 0.3).
    #[serde(default)]
    pub location: ToolWindowLocation,
}

/// Where a plugin's window opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolWindowLocation {
    /// The island on the right, with an icon in the launchpad.
    #[default]
    Island,
    /// A tab among the editor's tabs, without a launchpad icon — something wide: a board, a
    /// preview. `ui.show` opens it (or brings it forward), `ui.hide` and the user close it; the
    /// palette and the window's keys open it too.
    Editor,
}

/// An item of a context menu: runs one of the plugin's commands on what the menu was opened on
/// (the command's context says what).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MenuSpec {
    /// One of the plugin's `[[commands]]`: its title is the item's label.
    pub command: String,
    pub location: MenuLocation,
    /// Shown only then; always by default.
    pub when: Option<MenuWhen>,
}

/// Which context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MenuLocation {
    /// The editor's (the right button over the text, ⇧F10).
    Editor,
    /// The project tree's.
    Tree,
    /// A tab's.
    Tab,
}

/// When a menu item shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MenuWhen {
    /// The editor has a selection.
    Selection,
    /// The menu is on a file (the tree, a tab).
    File,
    /// The menu is on a folder (the tree).
    Folder,
}

/// An item of the status bar; the plugin sets its text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct StatusItemSpec {
    pub id: String,
}

/// A language (stage 8.3): which files are of it, its grammar and its highlighting query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageSpec {
    /// The key Flux knows the language by: "rust", "typescript", "tsx". Lowercase letters,
    /// digits, `-`, `_`, `+`, `#`. Also a name of code fences.
    pub id: String,
    /// The name people read: "Rust".
    pub name: String,
    /// Extensions without the dot, in lowercase ("rs"); a file matches in any case.
    pub extensions: Vec<String>,
    /// Exact file names ("Cargo.lock", ".bashrc").
    pub file_names: Vec<String>,
    /// More names of code fences: "rs", "py".
    pub aliases: Vec<String>,
    /// The id of one of the plugin's `[[grammars]]`.
    pub grammar: String,
    /// Highlighting query files in the plugin's folder, concatenated in this order.
    pub highlights: Vec<String>,
    pub precedence: QueryPrecedence,
    /// The language id language servers know it by (`textDocument/didOpen`): "typescriptreact";
    /// the id by default.
    pub lsp_id: Option<String>,
    /// The icon of the language's files: a monochrome 16×16 SVG in the plugin's folder, drawn when
    /// the icon theme has none for these files.
    pub icon: Option<String>,
    /// The icon's color: a shade of the theme's palette ("orange", "blue", "text-muted"…) or
    /// "#rrggbb".
    pub icon_color: Option<String>,
}

impl LanguageSpec {
    /// The language id for language servers.
    pub fn lsp_id(&self) -> &str {
        self.lsp_id.as_deref().unwrap_or(&self.id)
    }
}

/// Which pattern wins when patterns of the highlighting query capture the same text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueryPrecedence {
    /// The later one: queries written for tree-sitter-highlight 0.21 and later.
    #[default]
    LastPattern,
    /// The earlier one: older queries, with special cases before generic ones.
    FirstPattern,
}

/// A tree-sitter grammar: compiled to WebAssembly and shipped with the plugin, or compiled into
/// Flux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarSpec {
    /// The id `[[languages]]` name it by.
    pub id: String,
    pub source: GrammarSourceSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrammarSourceSpec {
    /// A `.wasm` in the plugin's folder (`tree-sitter build --wasm`). `symbol` is the grammar's
    /// own name: the module exports `tree_sitter_<symbol>`.
    Wasm { path: String, symbol: String },
    /// A grammar compiled into Flux, by name: only Flux's bundled plugins have them.
    Builtin(String),
}

/// A language server: the program, the files it serves, its settings, and how Flux installs it
/// when it isn't on the machine.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguageServerSpec {
    /// The server's name, also its key: "rust-analyzer" (its folder among installed servers, its
    /// row in Settings → Language Servers).
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    /// The languages whose files it serves (by language id, of any plugin).
    pub languages: Vec<String>,
    /// Instead of the languages' files: these extensions and file names. A server that serves
    /// only some of a language's files (bash-language-server: not `.zsh`).
    pub extensions: Vec<String>,
    pub file_names: Vec<String>,
    /// `initializationOptions` of `initialize`.
    pub initialization_options: Option<Value>,
    /// Answers to `workspace/configuration`, by section.
    pub settings: Option<Value>,
    pub install: Option<InstallSpec>,
}

/// How Flux installs a language server that isn't on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSpec {
    /// npm packages (the server first, then what it needs), with a Node.js of Flux's own;
    /// `bin` — the executable in `node_modules/.bin`. `{ npm = ["pyright"], bin = "pyright-langserver" }`.
    Npm { packages: Vec<String>, bin: String },
    /// A binary from the latest GitHub release of `repo`; `asset` with `{arch}` (`aarch64`,
    /// `x86_64`) and `{tag}`; `bin` — its name inside an archive.
    /// `{ github = "astral-sh/ruff", asset = "ruff-{arch}-apple-darwin.tar.gz", bin = "ruff" }`.
    GitHub {
        repo: String,
        asset: String,
        bin: String,
    },
    /// `go install <package>@latest`. `{ go = "golang.org/x/tools/gopls", bin = "gopls" }`.
    Go { package: String, bin: String },
    /// `rustup component add <component>`, otherwise `fallback`.
    Rustup {
        component: String,
        fallback: Box<InstallSpec>,
    },
}

/// A color theme: a file in the plugin's folder (its format — `docs/themes.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ThemeSpec {
    pub file: String,
}

/// A set of file icons: a file in the plugin's folder (its format — `docs/icon-themes.md`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct IconThemeSpec {
    pub file: String,
}

/// A setting of the plugin: Settings → the plugin shows a form of them.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingSpec {
    pub key: String,
    pub title: String,
    pub description: Option<String>,
    pub kind: SettingKind,
    /// The value until the user changes it, as JSON.
    pub default: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    /// A switch.
    Bool,
    /// A text field.
    String,
    /// A number field.
    Integer { min: Option<i64>, max: Option<i64> },
    /// One of the options.
    Choice(Vec<ChoiceOption>),
    /// A list of strings, a row each.
    StringList,
    /// A token or a password (API 0.3): a masked field whose value Flux keeps encrypted — the
    /// plugin's secret named by the setting's key (`secrets.get`) — never in `settings.json`. No
    /// default.
    Secret,
    /// A button (API 0.3) that runs one of the plugin's commands, with `argument` in the
    /// command's context: "Sign In…", "Check Connection". It has no value.
    Button {
        command: String,
        argument: Option<String>,
    },
    /// A line the plugin writes (API 0.3, `settings.set-status`): "Signed in as …". The default
    /// is its text until the plugin writes one. It has no value.
    Status,
    /// A heading (API 0.3) that starts a group of the settings below it, with the description
    /// under it. It has no value.
    Section,
    /// Several lines of text (API 0.3): a prompt, a template.
    Text,
    /// A list of records with the same fields (API 0.3), a card each, added, removed and moved by
    /// the user: the plugin's own actions (a name, a prompt, a mode). The value is a list of
    /// objects by the fields' keys; a missing key — the field's default.
    Records(Vec<RecordField>),
}

/// A field of a `records` setting.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordField {
    pub key: String,
    pub title: String,
    pub kind: RecordFieldKind,
    /// A new record's value.
    pub default: Value,
}

/// What a field of a record holds.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordFieldKind {
    /// A line of text.
    String,
    /// Several lines.
    Text,
    /// A switch.
    Bool,
    /// One of the options.
    Choice(Vec<ChoiceOption>),
}

impl RecordFieldKind {
    /// Whether `value` is one the field can have.
    pub fn fits(&self, value: &Value) -> bool {
        match self {
            RecordFieldKind::String | RecordFieldKind::Text => value.is_string(),
            RecordFieldKind::Bool => value.is_boolean(),
            RecordFieldKind::Choice(options) => value
                .as_str()
                .is_some_and(|value| options.iter().any(|option| option.value == value)),
        }
    }
}

/// Whether `value` is a list of records of `fields`: objects whose keys are the fields' and whose
/// values fit them.
fn records_fit(fields: &[RecordField], value: &Value) -> bool {
    value.as_array().is_some_and(|records| {
        records.iter().all(|record| {
            record.as_object().is_some_and(|record| {
                record.iter().all(|(key, value)| {
                    fields
                        .iter()
                        .find(|field| field.key == *key)
                        .is_some_and(|field| field.kind.fits(value))
                })
            })
        })
    })
}

impl SettingKind {
    /// The setting has a value in `settings.json` (`settings.get`, `settings.set`): not a secret,
    /// a button, a status or a section.
    pub fn has_value(&self) -> bool {
        matches!(
            self,
            SettingKind::Bool
                | SettingKind::String
                | SettingKind::Integer { .. }
                | SettingKind::Choice(_)
                | SettingKind::StringList
                | SettingKind::Text
                | SettingKind::Records(_)
        )
    }
}

impl SettingSpec {
    /// Whether `value` is one the setting can have, as the Settings page checks what's typed: a
    /// switch takes true or false, a text field a string, a number a whole number within its
    /// range, a choice one of its options' values, a list strings. The error says what it should
    /// be.
    pub fn check_value(&self, value: &Value) -> Result<(), String> {
        let fits = match &self.kind {
            SettingKind::Bool => value.is_boolean(),
            SettingKind::String => value.is_string(),
            SettingKind::Integer { min, max } => value
                .as_i64()
                .is_some_and(|n| min.is_none_or(|min| n >= min) && max.is_none_or(|max| n <= max)),
            SettingKind::Choice(options) => value
                .as_str()
                .is_some_and(|value| options.iter().any(|option| option.value == value)),
            SettingKind::StringList => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            SettingKind::Text => value.is_string(),
            SettingKind::Records(fields) => records_fit(fields, value),
            SettingKind::Secret
            | SettingKind::Button { .. }
            | SettingKind::Status
            | SettingKind::Section => return Err("it has no value".into()),
        };
        if fits {
            return Ok(());
        }
        Err(match &self.kind {
            SettingKind::Bool => "it must be true or false".to_string(),
            SettingKind::String | SettingKind::Text => "it must be a string".to_string(),
            SettingKind::Records(fields) => format!(
                "it must be a list of records with the fields {}",
                fields
                    .iter()
                    .map(|field| format!("\"{}\"", field.key))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            SettingKind::Integer { min, max } => match (min, max) {
                (Some(min), Some(max)) => format!("it must be a whole number from {min} to {max}"),
                (Some(min), None) => format!("it must be a whole number, at least {min}"),
                (None, Some(max)) => format!("it must be a whole number, at most {max}"),
                (None, None) => "it must be a whole number".to_string(),
            },
            SettingKind::Choice(options) => format!(
                "it must be one of {}",
                options
                    .iter()
                    .map(|option| format!("\"{}\"", option.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => "it must be a list of strings".to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub value: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    /// Reads and checks a manifest. An API version Flux doesn't have is not an error here: the
    /// plugin is listed, and the registry says why it can't run.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let raw: RawManifest = toml::from_str(text).map_err(|err| {
            // "unknown field `homepage`, expected one of `id`, `name`, …": the list of every key
            // buries the mistake (docs/plugins.md lists the keys).
            let message = err.message();
            let message = match message.split_once(", expected one of") {
                Some((mistake, _)) if message.starts_with("unknown field") => mistake,
                _ => message,
            };
            ManifestError(message.to_string())
        })?;
        raw.check()
    }

    /// The command by its id.
    pub fn command(&self, id: &str) -> Option<&CommandSpec> {
        self.commands.iter().find(|command| command.id == id)
    }

    pub fn tool_window(&self, id: &str) -> Option<&ToolWindowSpec> {
        self.tool_windows.iter().find(|window| window.id == id)
    }

    pub fn setting(&self, key: &str) -> Option<&SettingSpec> {
        self.settings.iter().find(|setting| setting.key == key)
    }

    pub fn language(&self, id: &str) -> Option<&LanguageSpec> {
        self.languages.iter().find(|language| language.id == id)
    }

    pub fn grammar(&self, id: &str) -> Option<&GrammarSpec> {
        self.grammars.iter().find(|grammar| grammar.id == id)
    }

    /// The plugin adds something without code: languages, servers, themes or icons.
    pub fn has_contributions(&self) -> bool {
        !self.languages.is_empty()
            || !self.language_servers.is_empty()
            || !self.themes.is_empty()
            || !self.icon_themes.is_empty()
    }

    /// The files of the plugin's folder the manifest names (queries, grammars, themes, icons), to
    /// check they are there.
    pub fn named_files(&self) -> Vec<&str> {
        let mut files: Vec<&str> = Vec::new();
        for language in &self.languages {
            files.extend(language.highlights.iter().map(String::as_str));
            files.extend(language.icon.as_deref());
        }
        for grammar in &self.grammars {
            if let GrammarSourceSpec::Wasm { path, .. } = &grammar.source {
                files.push(path);
            }
        }
        files.extend(self.themes.iter().map(|theme| theme.file.as_str()));
        files.extend(self.icon_themes.iter().map(|icons| icons.file.as_str()));
        files
    }
}

/// A language id: lowercase letters, digits, `-`, `_`, `+`, `#` ("c++", "c#", "objective-c").
pub fn is_valid_language_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '+' | '#')
        })
}

/// `flux.todo`, `someone.hello-world`: lowercase letters and digits, words joined by `.` or `-`.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split(['.', '-']).all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawManifest {
    id: String,
    name: String,
    version: String,
    api: String,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    description: String,
    repository: Option<String>,
    icon: Option<String>,
    wasm: Option<String>,
    #[serde(default)]
    permissions: RawPermissions,
    #[serde(default)]
    notifications: RawNotifications,
    #[serde(default)]
    commands: Vec<CommandSpec>,
    #[serde(default)]
    tool_windows: Vec<ToolWindowSpec>,
    #[serde(default)]
    status_items: Vec<StatusItemSpec>,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    menus: Vec<MenuSpec>,
    #[serde(default)]
    languages: Vec<RawLanguage>,
    #[serde(default)]
    grammars: Vec<RawGrammar>,
    #[serde(default)]
    language_servers: Vec<RawLanguageServer>,
    #[serde(default)]
    themes: Vec<ThemeSpec>,
    #[serde(default)]
    icon_themes: Vec<IconThemeSpec>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawLanguage {
    id: String,
    name: String,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    file_names: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    grammar: String,
    highlights: OneOrMany,
    #[serde(default)]
    precedence: QueryPrecedence,
    lsp_id: Option<String>,
    icon: Option<String>,
    icon_color: Option<String>,
}

/// A string or a list of them.
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(one) => vec![one],
            OneOrMany::Many(many) => many,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawGrammar {
    id: String,
    wasm: Option<String>,
    symbol: Option<String>,
    builtin: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawLanguageServer {
    id: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    file_names: Vec<String>,
    initialization_options: Option<toml::Value>,
    settings: Option<toml::Value>,
    install: Option<RawInstall>,
}

/// `{ npm = […], bin = … }`, `{ github = …, asset = …, bin = … }`, `{ go = …, bin = … }`,
/// `{ rustup = …, fallback = {…} }`.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawInstall {
    Npm {
        npm: Vec<String>,
        bin: String,
    },
    GitHub {
        github: String,
        asset: String,
        bin: String,
    },
    Go {
        go: String,
        bin: String,
    },
    Rustup {
        rustup: String,
        fallback: Box<RawInstall>,
    },
}

impl RawInstall {
    fn check(self) -> InstallSpec {
        match self {
            RawInstall::Npm { npm, bin } => InstallSpec::Npm { packages: npm, bin },
            RawInstall::GitHub { github, asset, bin } => InstallSpec::GitHub {
                repo: github,
                asset,
                bin,
            },
            RawInstall::Go { go, bin } => InstallSpec::Go { package: go, bin },
            RawInstall::Rustup { rustup, fallback } => InstallSpec::Rustup {
                component: rustup,
                fallback: Box::new(fallback.check()),
            },
        }
    }
}

/// A path in the plugin's folder: relative, `/`-separated, without `..`.
fn check_relative(what: &str, path: &str) -> Result<(), ManifestError> {
    let fine = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.split('/').all(|part| !part.is_empty() && part != "..");
    if fine {
        Ok(())
    } else {
        Err(ManifestError(format!(
            "{what}: \"{path}\" — a path in the plugin's folder, like \"themes/dark.toml\""
        )))
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawPermissions {
    #[serde(default)]
    project: ProjectAccess,
    #[serde(default)]
    network: Vec<String>,
    #[serde(default)]
    server: bool,
    #[serde(default)]
    processes: Vec<String>,
    #[serde(default)]
    terminal: bool,
    #[serde(default)]
    folders: Vec<FolderPermission>,
    #[serde(default)]
    claude: bool,
}

impl RawPermissions {
    fn check(self) -> Result<Permissions, ManifestError> {
        let error = |message: String| Err(ManifestError(message));
        let mut network = Vec::new();
        for host in self.network {
            let host = host.trim().to_ascii_lowercase();
            let name = host.strip_prefix("*.").unwrap_or(&host);
            let valid = host == "*"
                || (!name.is_empty()
                    && name.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')
                    }));
            if !valid {
                return error(format!(
                    "permissions.network: \"{host}\" — a host, like \"api.example.com\" or \
                     \"*.example.com\", without a scheme, a port or a path"
                ));
            }
            network.push(host);
        }
        for program in &self.processes {
            if program.is_empty() || program.contains('/') || program.contains(char::is_whitespace)
            {
                return error(format!(
                    "permissions.processes: \"{program}\" — a program's name, like \"git\", or \"*\""
                ));
            }
        }
        for folder in &self.folders {
            if !(folder.path.starts_with("~/")
                || folder.path == "~"
                || folder.path.starts_with('/'))
            {
                return error(format!(
                    "permissions.folders: \"{}\" — an absolute path or one in the home folder \
                     (\"~/.config/…\")",
                    folder.path
                ));
            }
        }
        Ok(Permissions {
            project: self.project,
            network,
            server: self.server,
            processes: self.processes,
            terminal: self.terminal,
            folders: self.folders,
            claude: self.claude,
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawNotifications {
    #[serde(default)]
    display: NotificationDisplay,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawSetting {
    key: String,
    title: String,
    description: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    default: Option<toml::Value>,
    min: Option<i64>,
    max: Option<i64>,
    #[serde(default)]
    options: Vec<ChoiceOption>,
    /// A button's command and its argument.
    command: Option<String>,
    argument: Option<String>,
    /// The fields of a `records` setting.
    #[serde(default)]
    fields: Vec<RawRecordField>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawRecordField {
    key: String,
    title: String,
    #[serde(rename = "type")]
    kind: String,
    default: Option<toml::Value>,
    #[serde(default)]
    options: Vec<ChoiceOption>,
}

impl RawRecordField {
    fn check(self, setting: &str) -> Result<RecordField, ManifestError> {
        let key = self.key;
        let error = |message: String| {
            ManifestError(format!("setting \"{setting}\", field \"{key}\": {message}"))
        };
        let kind = match self.kind.as_str() {
            "string" => RecordFieldKind::String,
            "text" => RecordFieldKind::Text,
            "bool" => RecordFieldKind::Bool,
            "choice" if self.options.is_empty() => {
                return Err(error("a choice needs options".into()));
            }
            "choice" => RecordFieldKind::Choice(self.options),
            other => {
                return Err(error(format!(
                    "unknown type \"{other}\" (string, text, bool, choice)"
                )));
            }
        };
        let default = match (self.default.map(toml_to_json), &kind) {
            (Some(value), kind) if kind.fits(&value) => value,
            (Some(_), _) => return Err(error("the default doesn't fit the type".into())),
            (None, RecordFieldKind::Bool) => Value::Bool(false),
            (None, RecordFieldKind::Choice(options)) => Value::String(options[0].value.clone()),
            (None, _) => Value::String(String::new()),
        };
        Ok(RecordField {
            key,
            title: self.title,
            kind,
            default,
        })
    }
}

impl RawManifest {
    fn check(self) -> Result<Manifest, ManifestError> {
        let error = |message: String| Err(ManifestError(message));
        if !is_valid_id(&self.id) {
            return error(format!(
                "id \"{}\": lowercase letters and digits, words joined by \".\" or \"-\"",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return error("name is empty".into());
        }
        if self.version.trim().is_empty() {
            return error("version is empty".into());
        }
        unique("command", self.commands.iter().map(|c| c.id.as_str()))?;
        unique(
            "tool window",
            self.tool_windows.iter().map(|w| w.id.as_str()),
        )?;
        unique(
            "status item",
            self.status_items.iter().map(|s| s.id.as_str()),
        )?;
        unique("setting", self.settings.iter().map(|s| s.key.as_str()))?;
        for menu in &self.menus {
            if !self
                .commands
                .iter()
                .any(|command| command.id == menu.command)
            {
                return error(format!(
                    "menus: the command \"{}\" isn't in [[commands]]",
                    menu.command
                ));
            }
            let fits = match menu.when {
                None => true,
                Some(MenuWhen::Selection) => menu.location == MenuLocation::Editor,
                Some(MenuWhen::File) => menu.location != MenuLocation::Editor,
                Some(MenuWhen::Folder) => menu.location == MenuLocation::Tree,
            };
            if !fits {
                return error(format!(
                    "menus: \"{}\" — `when` doesn't fit the location (selection: editor; file: \
                     tree, tab; folder: tree)",
                    menu.command
                ));
            }
        }
        let permissions = self.permissions.check()?;
        let settings = self
            .settings
            .into_iter()
            .map(RawSetting::check)
            .collect::<Result<Vec<_>, _>>()?;
        for setting in &settings {
            if let SettingKind::Button { command, .. } = &setting.kind
                && !self.commands.iter().any(|known| known.id == *command)
            {
                return error(format!(
                    "setting \"{}\": the button's command \"{command}\" isn't in [[commands]]",
                    setting.key
                ));
            }
        }
        let (languages, grammars, language_servers) =
            check_languages(self.languages, self.grammars, self.language_servers)?;
        for theme in &self.themes {
            check_relative("themes", &theme.file)?;
        }
        for icons in &self.icon_themes {
            check_relative("icon-themes", &icons.file)?;
        }
        Ok(Manifest {
            id: self.id,
            name: self.name,
            version: self.version,
            api: self.api,
            authors: self.authors,
            description: self.description,
            repository: self.repository,
            icon: self.icon,
            wasm: self.wasm,
            permissions,
            notifications: self.notifications.display,
            commands: self.commands,
            tool_windows: self.tool_windows,
            status_items: self.status_items,
            settings,
            menus: self.menus,
            languages,
            grammars,
            language_servers,
            themes: self.themes,
            icon_themes: self.icon_themes,
        })
    }
}

/// The languages, grammars and language servers of a manifest.
type LanguageParts = (Vec<LanguageSpec>, Vec<GrammarSpec>, Vec<LanguageServerSpec>);

/// Checks the languages, grammars and servers of a manifest: ids, references between them, paths.
fn check_languages(
    raw_languages: Vec<RawLanguage>,
    raw_grammars: Vec<RawGrammar>,
    raw_servers: Vec<RawLanguageServer>,
) -> Result<LanguageParts, ManifestError> {
    let error = |message: String| Err(ManifestError(message));
    unique("grammar", raw_grammars.iter().map(|g| g.id.as_str()))?;
    let mut grammars = Vec::new();
    for raw in raw_grammars {
        let source = match (raw.wasm, raw.builtin) {
            (Some(path), None) => {
                check_relative(&format!("grammar \"{}\"", raw.id), &path)?;
                let symbol = raw.symbol.unwrap_or_else(|| raw.id.replace('-', "_"));
                GrammarSourceSpec::Wasm { path, symbol }
            }
            (None, Some(name)) if raw.symbol.is_none() => GrammarSourceSpec::Builtin(name),
            _ => {
                return error(format!(
                    "grammar \"{}\": either `wasm` (with an optional `symbol`) or `builtin`",
                    raw.id
                ));
            }
        };
        grammars.push(GrammarSpec { id: raw.id, source });
    }
    unique("language", raw_languages.iter().map(|l| l.id.as_str()))?;
    let mut languages = Vec::new();
    for raw in raw_languages {
        if !is_valid_language_id(&raw.id) {
            return error(format!(
                "language \"{}\": lowercase letters, digits, \"-\", \"_\", \"+\", \"#\"",
                raw.id
            ));
        }
        if raw.name.trim().is_empty() {
            return error(format!("language \"{}\": the name is empty", raw.id));
        }
        if !grammars.iter().any(|grammar| grammar.id == raw.grammar) {
            return error(format!(
                "language \"{}\": the grammar \"{}\" isn't in [[grammars]]",
                raw.id, raw.grammar
            ));
        }
        let highlights = raw.highlights.into_vec();
        if highlights.is_empty() {
            return error(format!("language \"{}\": no highlights", raw.id));
        }
        for file in &highlights {
            check_relative(&format!("language \"{}\"", raw.id), file)?;
        }
        if let Some(icon) = &raw.icon {
            check_relative(&format!("language \"{}\"", raw.id), icon)?;
        }
        if raw
            .extensions
            .iter()
            .any(|extension| extension.starts_with('.'))
        {
            return error(format!(
                "language \"{}\": extensions go without the dot (\"rs\", not \".rs\")",
                raw.id
            ));
        }
        languages.push(LanguageSpec {
            id: raw.id,
            name: raw.name,
            extensions: raw
                .extensions
                .into_iter()
                .map(|extension| extension.to_ascii_lowercase())
                .collect(),
            file_names: raw.file_names,
            aliases: raw.aliases,
            grammar: raw.grammar,
            highlights,
            precedence: raw.precedence,
            lsp_id: raw.lsp_id,
            icon: raw.icon,
            icon_color: raw.icon_color,
        });
    }
    unique("language server", raw_servers.iter().map(|s| s.id.as_str()))?;
    let mut servers = Vec::new();
    for raw in raw_servers {
        if raw.command.trim().is_empty() {
            return error(format!("language server \"{}\": no command", raw.id));
        }
        if raw.languages.is_empty() && raw.extensions.is_empty() && raw.file_names.is_empty() {
            return error(format!(
                "language server \"{}\": which files — `languages`, `extensions` or `file-names`",
                raw.id
            ));
        }
        servers.push(LanguageServerSpec {
            id: raw.id,
            command: raw.command,
            args: raw.args,
            languages: raw.languages,
            extensions: raw
                .extensions
                .into_iter()
                .map(|extension| extension.to_ascii_lowercase())
                .collect(),
            file_names: raw.file_names,
            initialization_options: raw.initialization_options.map(toml_to_json),
            settings: raw.settings.map(toml_to_json),
            install: raw.install.map(RawInstall::check),
        });
    }
    Ok((languages, grammars, servers))
}

fn unique<'a>(what: &str, ids: impl Iterator<Item = &'a str>) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.is_empty() {
            return Err(ManifestError(format!("a {what} without an id")));
        }
        if !seen.insert(id) {
            return Err(ManifestError(format!("two of {what} \"{id}\"")));
        }
    }
    Ok(())
}

impl RawSetting {
    fn check(self) -> Result<SettingSpec, ManifestError> {
        let key = self.key;
        let wrong = |expected: &str| {
            Err(ManifestError(format!(
                "setting \"{key}\": the default must be {expected}"
            )))
        };
        let default = self.default.map(toml_to_json);
        let (kind, default) = match self.kind.as_str() {
            "bool" => match default {
                None => (SettingKind::Bool, Value::Bool(false)),
                Some(value @ Value::Bool(_)) => (SettingKind::Bool, value),
                Some(_) => return wrong("true or false"),
            },
            "string" => match default {
                None => (SettingKind::String, Value::String(String::new())),
                Some(value @ Value::String(_)) => (SettingKind::String, value),
                Some(_) => return wrong("a string"),
            },
            "integer" => {
                let kind = SettingKind::Integer {
                    min: self.min,
                    max: self.max,
                };
                match default {
                    None => (kind, Value::from(self.min.unwrap_or(0).max(0))),
                    Some(value) if value.is_i64() => (kind, value),
                    Some(_) => return wrong("an integer"),
                }
            }
            "choice" => {
                if self.options.is_empty() {
                    return Err(ManifestError(format!(
                        "setting \"{key}\": a choice needs options"
                    )));
                }
                let first = Value::String(self.options[0].value.clone());
                match default {
                    None => (SettingKind::Choice(self.options), first),
                    Some(Value::String(value))
                        if self.options.iter().any(|option| option.value == value) =>
                    {
                        (SettingKind::Choice(self.options), Value::String(value))
                    }
                    Some(_) => return wrong("one of the options' values"),
                }
            }
            "string-list" => match default {
                None => (SettingKind::StringList, Value::Array(Vec::new())),
                Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                    (SettingKind::StringList, Value::Array(items))
                }
                Some(_) => return wrong("a list of strings"),
            },
            "secret" => match default {
                None => (SettingKind::Secret, Value::Null),
                Some(_) => {
                    return Err(ManifestError(format!(
                        "setting \"{key}\": a secret has no default"
                    )));
                }
            },
            "button" => match self.command {
                Some(command) => (
                    SettingKind::Button {
                        command,
                        argument: self.argument,
                    },
                    Value::Null,
                ),
                None => {
                    return Err(ManifestError(format!(
                        "setting \"{key}\": a button needs a command"
                    )));
                }
            },
            "status" => match default {
                None => (SettingKind::Status, Value::String(String::new())),
                Some(value @ Value::String(_)) => (SettingKind::Status, value),
                Some(_) => return wrong("a string"),
            },
            "section" => (SettingKind::Section, Value::Null),
            "text" => match default {
                None => (SettingKind::Text, Value::String(String::new())),
                Some(value @ Value::String(_)) => (SettingKind::Text, value),
                Some(_) => return wrong("a string"),
            },
            "records" => {
                if self.fields.is_empty() {
                    return Err(ManifestError(format!(
                        "setting \"{key}\": records need fields"
                    )));
                }
                let mut fields = Vec::new();
                for field in self.fields {
                    let field = field.check(&key)?;
                    if fields
                        .iter()
                        .any(|known: &RecordField| known.key == field.key)
                    {
                        return Err(ManifestError(format!(
                            "setting \"{key}\": the field \"{}\" is there twice",
                            field.key
                        )));
                    }
                    fields.push(field);
                }
                let default = default.unwrap_or(Value::Array(Vec::new()));
                if !records_fit(&fields, &default) {
                    return wrong("a list of records of the fields");
                }
                (SettingKind::Records(fields), default)
            }
            other => {
                return Err(ManifestError(format!(
                    "setting \"{key}\": unknown type \"{other}\" (bool, string, integer, choice, \
                     string-list, text, records, secret, button, status, section)"
                )));
            }
        };
        Ok(SettingSpec {
            key,
            title: self.title,
            description: self.description,
            kind,
            default,
        })
    }
}

fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::from(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => Value::Object(
            table
                .into_iter()
                .map(|(key, value)| (key, toml_to_json(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODO: &str = r#"
id = "flux.todo"
name = "TODO"
version = "0.1.0"
api = "0.2"
authors = ["Egor Ageev"]
description = "TODO comments."
wasm = "todo.wasm"

[permissions]
project = "read"

[[commands]]
id = "refresh"
title = "Refresh"

[[tool-windows]]
id = "todo"
title = "TODO"
icon = "icons/todo.svg"

[[settings]]
key = "patterns"
title = "Patterns"
type = "string-list"
default = ["\\bTODO\\b", "\\bFIXME\\b"]

[[settings]]
key = "scope"
title = "Scope"
type = "choice"
options = [{ value = "project", title = "Project" }, { value = "file", title = "Current File" }]
"#;

    #[test]
    fn reads_a_manifest() {
        let manifest = Manifest::parse(TODO).unwrap();
        assert_eq!(manifest.id, "flux.todo");
        assert_eq!(manifest.permissions.project, ProjectAccess::Read);
        assert_eq!(manifest.notifications, NotificationDisplay::Balloon);
        assert_eq!(manifest.command("refresh").unwrap().title, "Refresh");
        assert_eq!(
            manifest.tool_window("todo").unwrap().icon.as_deref(),
            Some("icons/todo.svg")
        );
        assert_eq!(
            manifest.setting("patterns").unwrap().default,
            serde_json::json!(["\\bTODO\\b", "\\bFIXME\\b"])
        );
        assert_eq!(
            manifest.setting("scope").unwrap().default,
            serde_json::json!("project")
        );
    }

    #[test]
    fn rejects_mistakes() {
        let bad_id = TODO.replace("flux.todo", "Flux TODO");
        assert!(Manifest::parse(&bad_id).unwrap_err().0.contains("id"));
        let unknown = format!("{TODO}\nhomepage = \"x\"");
        assert_eq!(
            Manifest::parse(&unknown).unwrap_err().0,
            "unknown field `homepage`"
        );
        let twice = format!("{TODO}\n[[commands]]\nid = \"refresh\"\ntitle = \"Again\"");
        assert!(Manifest::parse(&twice).unwrap_err().0.contains("refresh"));
        let wrong_default = TODO.replace("type = \"string-list\"", "type = \"bool\"");
        assert!(
            Manifest::parse(&wrong_default)
                .unwrap_err()
                .0
                .contains("patterns")
        );
    }

    #[test]
    fn permissions() {
        let text = TODO.replace(
            "project = \"read\"",
            "project = \"read\"\nnetwork = [\"api.github.com\", \"*.Yandex.ru\", \"localhost\"]\n\
             processes = [\"git\"]\nserver = true\nfolders = [{ path = \"~/.config/gh\" }]",
        );
        let permissions = Manifest::parse(&text).unwrap().permissions;
        assert!(permissions.allows_host("api.github.com"));
        assert!(!permissions.allows_host("github.com"));
        assert!(permissions.allows_host("yandex.ru"));
        assert!(permissions.allows_host("api.tracker.YANDEX.ru"));
        assert!(!permissions.allows_host("notyandex.ru"));
        assert!(permissions.allows_host("127.0.0.1"));
        assert!(permissions.allows_host("[::1]"));
        assert!(!permissions.any_host());
        assert!(permissions.allows_program("git"));
        assert!(permissions.allows_program("/usr/bin/git"));
        assert!(!permissions.allows_program("gh"));
        assert!(permissions.server && !permissions.terminal);
        assert_eq!(
            permissions.folders[0].resolve(Path::new("/Users/me")),
            Path::new("/Users/me/.config/gh")
        );
        assert_eq!(permissions.folders[0].access, FolderAccess::Read);
        for wrong in [
            "network = [\"https://api.github.com\"]",
            "network = [\"api.github.com/v3\"]",
            "processes = [\"/bin/sh\"]",
            "folders = [{ path = \"relative\" }]",
        ] {
            let text = TODO.replace("project = \"read\"", wrong);
            assert!(Manifest::parse(&text).is_err(), "{wrong}");
        }
        assert!(
            Manifest::parse(TODO)
                .unwrap()
                .permissions
                .network
                .is_empty()
        );
    }

    #[test]
    fn menus() {
        let with = |menu: &str| Manifest::parse(&format!("{TODO}\n[[menus]]\n{menu}"));
        let manifest =
            with("command = \"refresh\"\nlocation = \"tree\"\nwhen = \"folder\"").unwrap();
        assert_eq!(manifest.menus[0].location, MenuLocation::Tree);
        assert_eq!(manifest.menus[0].when, Some(MenuWhen::Folder));
        assert!(with("command = \"nope\"\nlocation = \"editor\"").is_err());
        assert!(with("command = \"refresh\"\nlocation = \"editor\"\nwhen = \"folder\"").is_err());
        assert!(with("command = \"refresh\"\nlocation = \"tab\"\nwhen = \"selection\"").is_err());
    }

    const RUST: &str = r#"
id = "flux.rust"
name = "Rust"
version = "0.1.0"
api = "0.2"

[[languages]]
id = "rust"
name = "Rust"
extensions = ["RS"]
aliases = ["rs"]
grammar = "rust"
highlights = "languages/rust/highlights.scm"
icon = "icons/rust.svg"
icon-color = "orange"

[[languages]]
id = "typescriptreact"
name = "TSX"
extensions = ["tsx"]
grammar = "tsx"
highlights = ["queries/javascript.scm", "queries/typescript.scm"]
precedence = "first-pattern"
lsp-id = "typescriptreact"

[[grammars]]
id = "rust"
wasm = "grammars/rust.wasm"

[[grammars]]
id = "tsx"
builtin = "tsx"

[[language-servers]]
id = "rust-analyzer"
command = "rust-analyzer"
languages = ["rust"]
initialization-options = { cargo = { features = "all" } }
install = { rustup = "rust-analyzer", fallback = { github = "rust-lang/rust-analyzer", asset = "rust-analyzer-{arch}-apple-darwin.gz", bin = "rust-analyzer" } }

[[language-servers]]
id = "pyright"
command = "pyright-langserver"
args = ["--stdio"]
extensions = ["py"]
install = { npm = ["pyright"], bin = "pyright-langserver" }

[[language-servers]]
id = "gopls"
command = "gopls"
file-names = ["go.mod"]
install = { go = "golang.org/x/tools/gopls", bin = "gopls" }

[[themes]]
file = "themes/night.toml"

[[icon-themes]]
file = "icon-themes/flux.toml"
"#;

    #[test]
    fn reads_declarative_contributions() {
        let manifest = Manifest::parse(RUST).unwrap();
        assert!(manifest.wasm.is_none());
        assert!(manifest.has_contributions());
        let rust = manifest.language("rust").unwrap();
        assert_eq!(rust.extensions, ["rs"]);
        assert_eq!(rust.highlights, ["languages/rust/highlights.scm"]);
        assert_eq!(rust.precedence, QueryPrecedence::LastPattern);
        assert_eq!(rust.lsp_id(), "rust");
        assert_eq!(rust.icon_color.as_deref(), Some("orange"));
        let tsx = manifest.language("typescriptreact").unwrap();
        assert_eq!(tsx.highlights.len(), 2);
        assert_eq!(tsx.precedence, QueryPrecedence::FirstPattern);
        assert_eq!(
            manifest.grammar("rust").unwrap().source,
            GrammarSourceSpec::Wasm {
                path: "grammars/rust.wasm".into(),
                symbol: "rust".into()
            }
        );
        assert_eq!(
            manifest.grammar("tsx").unwrap().source,
            GrammarSourceSpec::Builtin("tsx".into())
        );
        let servers = &manifest.language_servers;
        assert_eq!(
            servers[0].initialization_options,
            Some(serde_json::json!({ "cargo": { "features": "all" } }))
        );
        assert_eq!(
            servers[0].install,
            Some(InstallSpec::Rustup {
                component: "rust-analyzer".into(),
                fallback: Box::new(InstallSpec::GitHub {
                    repo: "rust-lang/rust-analyzer".into(),
                    asset: "rust-analyzer-{arch}-apple-darwin.gz".into(),
                    bin: "rust-analyzer".into(),
                }),
            })
        );
        assert_eq!(
            servers[1].install,
            Some(InstallSpec::Npm {
                packages: vec!["pyright".into()],
                bin: "pyright-langserver".into()
            })
        );
        assert_eq!(
            servers[2].install,
            Some(InstallSpec::Go {
                package: "golang.org/x/tools/gopls".into(),
                bin: "gopls".into()
            })
        );
        assert_eq!(manifest.themes[0].file, "themes/night.toml");
        assert_eq!(manifest.icon_themes[0].file, "icon-themes/flux.toml");
        let files = manifest.named_files();
        for file in [
            "languages/rust/highlights.scm",
            "icons/rust.svg",
            "grammars/rust.wasm",
            "themes/night.toml",
            "icon-themes/flux.toml",
        ] {
            assert!(files.contains(&file), "{file}");
        }
        assert!(!Manifest::parse(TODO).unwrap().has_contributions());
    }

    #[test]
    fn rejects_contribution_mistakes() {
        for (wrong, right) in [
            ("grammar = \"rust\"", "grammar = \"nope\""),
            (
                "id = \"rust\"\nname = \"Rust\"",
                "id = \"Rust\"\nname = \"Rust\"",
            ),
            ("extensions = [\"RS\"]", "extensions = [\".rs\"]"),
            ("highlights = \"languages", "highlights = \"../languages"),
            (
                "wasm = \"grammars/rust.wasm\"",
                "wasm = \"grammars/rust.wasm\"\nbuiltin = \"rust\"",
            ),
            (
                "file = \"themes/night.toml\"",
                "file = \"/themes/night.toml\"",
            ),
            ("extensions = [\"py\"]", "extensions = []"),
            ("install = { go", "install = { goo"),
        ] {
            let text = RUST.replacen(wrong, right, 1);
            assert_ne!(text, RUST, "{wrong}");
            assert!(Manifest::parse(&text).is_err(), "{right}");
        }
        assert!(is_valid_language_id("c++"));
        assert!(is_valid_language_id("c#"));
        assert!(!is_valid_language_id("C"));
        assert!(!is_valid_language_id("a b"));
    }

    #[test]
    fn ids() {
        assert!(is_valid_id("flux.todo"));
        assert!(is_valid_id("someone.hello-world2"));
        assert!(!is_valid_id("Flux.todo"));
        assert!(!is_valid_id("flux..todo"));
        assert!(!is_valid_id("flux todo"));
        assert!(!is_valid_id(""));
    }

    /// API 0.3: settings that aren't values, commands only buttons run, Claude.
    const ACCOUNT: &str = r#"
id = "flux.yandex"
name = "Yandex"
version = "0.1.0"
api = "0.3"

[permissions]
claude = true

[[commands]]
id = "sign-in"
title = "Sign In"
palette = false

[[commands]]
id = "refresh"
title = "Refresh"

[[settings]]
key = "account"
type = "section"
title = "Account"
description = "Who is signed in."

[[settings]]
key = "account-status"
type = "status"
title = "Account"
default = "Not signed in"

[[settings]]
key = "sign-in-button"
type = "button"
title = "Sign In…"
command = "sign-in"
argument = "browser"

[[settings]]
key = "token"
type = "secret"
title = "Token"

[[settings]]
key = "minutes"
type = "integer"
title = "Minutes"
default = 5
min = 0
max = 60
"#;

    #[test]
    fn reads_the_settings_of_api_0_3() {
        let manifest = Manifest::parse(ACCOUNT).unwrap();
        assert!(manifest.permissions.claude);
        assert!(!manifest.permissions.is_empty());
        assert!(!manifest.command("sign-in").unwrap().palette);
        assert!(manifest.command("refresh").unwrap().palette);
        let kinds: Vec<&SettingKind> = manifest.settings.iter().map(|s| &s.kind).collect();
        assert_eq!(
            kinds[..4],
            [
                &SettingKind::Section,
                &SettingKind::Status,
                &SettingKind::Button {
                    command: "sign-in".into(),
                    argument: Some("browser".into()),
                },
                &SettingKind::Secret,
            ]
        );
        assert_eq!(
            manifest.setting("account-status").unwrap().default,
            Value::String("Not signed in".into())
        );
        assert!(!SettingKind::Secret.has_value());
        assert!(manifest.setting("minutes").unwrap().kind.has_value());
    }

    #[test]
    fn rejects_mistakes_in_settings_of_api_0_3() {
        let no_command = ACCOUNT.replace("command = \"sign-in\"\n", "");
        assert!(
            Manifest::parse(&no_command)
                .unwrap_err()
                .0
                .contains("a button needs a command")
        );
        let unknown = ACCOUNT.replace("command = \"sign-in\"", "command = \"log-in\"");
        assert_eq!(
            Manifest::parse(&unknown).unwrap_err().0,
            "setting \"sign-in-button\": the button's command \"log-in\" isn't in [[commands]]"
        );
        let secret_default = ACCOUNT.replace(
            "key = \"token\"\ntype = \"secret\"",
            "key = \"token\"\ntype = \"secret\"\ndefault = \"x\"",
        );
        assert!(
            Manifest::parse(&secret_default)
                .unwrap_err()
                .0
                .contains("a secret has no default")
        );
        let unknown_type = ACCOUNT.replace("type = \"section\"", "type = \"heading\"");
        assert!(
            Manifest::parse(&unknown_type)
                .unwrap_err()
                .0
                .contains("secret, button, status, section")
        );
    }

    #[test]
    fn checks_a_value_the_way_settings_does() {
        let manifest = Manifest::parse(ACCOUNT).unwrap();
        let minutes = manifest.setting("minutes").unwrap();
        assert!(minutes.check_value(&Value::from(60)).is_ok());
        assert_eq!(
            minutes.check_value(&Value::from(61)).unwrap_err(),
            "it must be a whole number from 0 to 60"
        );
        assert!(minutes.check_value(&Value::from("5")).is_err());
        let todo = Manifest::parse(TODO).unwrap();
        let scope = todo.setting("scope").unwrap();
        assert!(scope.check_value(&Value::from("file")).is_ok());
        assert_eq!(
            scope.check_value(&Value::from("folder")).unwrap_err(),
            "it must be one of \"project\", \"file\""
        );
        let patterns = todo.setting("patterns").unwrap();
        assert!(patterns.check_value(&serde_json::json!(["a", "b"])).is_ok());
        assert!(patterns.check_value(&serde_json::json!(["a", 1])).is_err());
        let token = manifest.setting("token").unwrap();
        assert_eq!(
            token.check_value(&Value::from("x")).unwrap_err(),
            "it has no value"
        );
    }

    /// Text and records (API 0.3), in the array-of-tables form a plugin's own actions take.
    const ACTIONS: &str = r#"
id = "test.actions"
name = "Actions"
version = "0.1.0"
api = "0.3"

[[settings]]
key = "template"
type = "text"
title = "Template"
default = """
Line one
Line two"""

[[settings]]
key = "actions"
type = "records"
title = "Actions"

[[settings.fields]]
key = "name"
title = "Name"
type = "string"

[[settings.fields]]
key = "prompt"
title = "Prompt"
type = "text"

[[settings.fields]]
key = "mode"
title = "Mode"
type = "choice"
default = "default"
options = [{ value = "default", title = "Default" }, { value = "plan", title = "Plan" }]

[[settings.fields]]
key = "favorite"
title = "On the board"
type = "bool"

[[settings.default]]
name = "Take to Work"
prompt = """
Take {key} to work."""
favorite = true

[[settings.default]]
name = "Plan"
mode = "plan"

[[settings]]
key = "after"
type = "bool"
title = "After"
"#;

    #[test]
    fn reads_text_and_records() {
        let manifest = Manifest::parse(ACTIONS).unwrap();
        let template = manifest.setting("template").unwrap();
        assert_eq!(template.kind, SettingKind::Text);
        assert_eq!(template.default, Value::from("Line one\nLine two"));
        assert!(template.kind.has_value());
        let actions = manifest.setting("actions").unwrap();
        let SettingKind::Records(fields) = &actions.kind else {
            panic!("not records: {:?}", actions.kind);
        };
        let kinds: Vec<(&str, &RecordFieldKind, &Value)> = fields
            .iter()
            .map(|field| (field.key.as_str(), &field.kind, &field.default))
            .collect();
        assert_eq!(
            kinds[0],
            ("name", &RecordFieldKind::String, &Value::from(""))
        );
        assert_eq!(
            kinds[1],
            ("prompt", &RecordFieldKind::Text, &Value::from(""))
        );
        assert_eq!(kinds[2].0, "mode");
        assert_eq!(kinds[2].2, &Value::from("default"));
        assert_eq!(
            kinds[3],
            ("favorite", &RecordFieldKind::Bool, &Value::Bool(false))
        );
        assert_eq!(
            actions.default,
            serde_json::json!([
                {"name": "Take to Work", "prompt": "Take {key} to work.", "favorite": true},
                {"name": "Plan", "mode": "plan"},
            ])
        );
        assert!(actions.kind.has_value());
        // The table after the records' own tables is a setting of its own.
        assert_eq!(manifest.setting("after").unwrap().kind, SettingKind::Bool);
    }

    #[test]
    fn checks_text_and_records_values() {
        let manifest = Manifest::parse(ACTIONS).unwrap();
        let template = manifest.setting("template").unwrap();
        assert!(template.check_value(&Value::from("a\nb")).is_ok());
        assert_eq!(
            template.check_value(&Value::from(1)).unwrap_err(),
            "it must be a string"
        );
        let actions = manifest.setting("actions").unwrap();
        // Missing keys are fine: the field's default.
        assert!(
            actions
                .check_value(&serde_json::json!([{"name": "Review"}, {}]))
                .is_ok()
        );
        assert!(actions.check_value(&serde_json::json!([])).is_ok());
        let fields = "it must be a list of records with the fields \"name\", \"prompt\", \
                      \"mode\", \"favorite\"";
        for wrong in [
            serde_json::json!({"name": "x"}),
            serde_json::json!([{"title": "x"}]),
            serde_json::json!([{"favorite": "yes"}]),
            serde_json::json!([{"mode": "fast"}]),
            serde_json::json!(["x"]),
        ] {
            assert_eq!(actions.check_value(&wrong).unwrap_err(), fields, "{wrong}");
        }
    }

    #[test]
    fn rejects_mistakes_in_text_and_records() {
        let error = |text: &str| Manifest::parse(text).unwrap_err().0;
        let not_a_string =
            ACTIONS.replace("default = \"\"\"\nLine one\nLine two\"\"\"", "default = 1");
        assert_eq!(
            error(&not_a_string),
            "setting \"template\": the default must be a string"
        );
        let no_fields = r#"
id = "test.actions"
name = "Actions"
version = "0.1.0"
api = "0.3"

[[settings]]
key = "actions"
type = "records"
title = "Actions"
"#;
        assert_eq!(error(no_fields), "setting \"actions\": records need fields");
        let twice = ACTIONS.replacen("key = \"prompt\"", "key = \"name\"", 1);
        assert_eq!(
            error(&twice),
            "setting \"actions\": the field \"name\" is there twice"
        );
        let wrong_default = ACTIONS.replace("favorite = true", "favorite = \"yes\"");
        assert_eq!(
            error(&wrong_default),
            "setting \"actions\": the default must be a list of records of the fields"
        );
        let unknown_type = ACTIONS.replace(
            "type = \"bool\"\n\n[[settings.default]]",
            "type = \"number\"\n\n[[settings.default]]",
        );
        assert_eq!(
            error(&unknown_type),
            "setting \"actions\", field \"favorite\": unknown type \"number\" (string, text, \
             bool, choice)"
        );
        let no_options = ACTIONS.replace(
            "options = [{ value = \"default\", title = \"Default\" }, { value = \"plan\", title = \
             \"Plan\" }]\n",
            "",
        );
        assert_eq!(
            error(&no_options),
            "setting \"actions\", field \"mode\": a choice needs options"
        );
        let field_default = ACTIONS.replace("default = \"default\"", "default = \"fast\"");
        assert_eq!(
            error(&field_default),
            "setting \"actions\", field \"mode\": the default doesn't fit the type"
        );
    }
}
