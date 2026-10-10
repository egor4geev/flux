//! The manifest, `flux-plugin.toml` in the plugin's folder: who the plugin is, what it may do and
//! what it adds to the window. Flux reads it before running any of the plugin's code: the manager
//! shows it, the install question lists its permissions, the palette its commands.
//!
//! ```toml
//! id = "flux.todo"
//! name = "TODO"
//! version = "0.1.0"
//! api = "0.2"
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
    /// The plugin API version it is built for (`"0.2"`); Flux runs only the one it has.
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
}

/// A tool window in the island on the right, opened by its icon in the launchpad.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ToolWindowSpec {
    pub id: String,
    pub title: String,
    /// A monochrome 16×16 SVG in the plugin's folder; a puzzle piece by default.
    pub icon: Option<String>,
    /// A default shortcut that opens and closes the window.
    pub keys: Option<String>,
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
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')));
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
            if !(folder.path.starts_with("~/") || folder.path == "~" || folder.path.starts_with('/'))
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
            if !self.commands.iter().any(|command| command.id == menu.command) {
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
        })
    }
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
            other => {
                return Err(ManifestError(format!(
                    "setting \"{key}\": unknown type \"{other}\" (bool, string, integer, choice, \
                     string-list)"
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
        assert!(Manifest::parse(TODO).unwrap().permissions.network.is_empty());
    }

    #[test]
    fn menus() {
        let with = |menu: &str| Manifest::parse(&format!("{TODO}\n[[menus]]\n{menu}"));
        let manifest = with("command = \"refresh\"\nlocation = \"tree\"\nwhen = \"folder\"").unwrap();
        assert_eq!(manifest.menus[0].location, MenuLocation::Tree);
        assert_eq!(manifest.menus[0].when, Some(MenuWhen::Folder));
        assert!(with("command = \"nope\"\nlocation = \"editor\"").is_err());
        assert!(with("command = \"refresh\"\nlocation = \"editor\"\nwhen = \"folder\"").is_err());
        assert!(with("command = \"refresh\"\nlocation = \"tab\"\nwhen = \"selection\"").is_err());
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
}
