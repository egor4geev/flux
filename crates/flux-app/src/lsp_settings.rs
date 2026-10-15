//! The user's settings of language servers (part 10.5): `lsp.<server>` in `settings.json` —
//! `settings` (answers to `workspace/configuration`), `initialization_options`, the command, its
//! arguments and environment, `enabled` — over what the language plugin brings
//! ([`flux_lsp::config::with_user_settings`]). The window's servers take them as their configs
//! ([`configs`]): a change of `settings` reaches a running server without a restart
//! (`workspace/didChangeConfiguration`), anything else starts it again. "Configure…" in Settings →
//! Language Servers and in the servers' popup opens `settings.json` in a tab at the server's key
//! ([`configure`]); saving the file applies it ([`crate::settings_file`]).

use std::path::Path;

use flux_lsp::config::{self, ServerConfig};
use gpui::{Action, App, Context, Window};
use serde_json::{Map, Value};

use crate::settings;
use crate::workspace::Workspace;

/// "Configure…" of a server from a notification about it: `settings.json` at its key.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = language_server, no_json)]
pub struct ConfigureServer {
    pub name: String,
}

pub fn init(cx: &mut App) {
    cx.on_action(|action: &ConfigureServer, cx| {
        let Some(window) = crate::settings_file::workspace_window(cx) else {
            return;
        };
        let name = action.name.clone();
        // The action arrives while its window is being updated: the window is updated after.
        cx.defer(move |cx| {
            window
                .update(cx, |workspace, window, cx| configure(workspace, &name, window, cx))
                .ok();
        });
    });
    // Scenarios can't click "Configure…": `FLUX_SCENARIO_CONFIGURE=<server>` does it a moment
    // after the window opens.
    #[cfg(feature = "scenario")]
    if let Ok(name) = std::env::var("FLUX_SCENARIO_CONFIGURE") {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1500))
                .await;
            cx.update(|cx| cx.dispatch_action(&ConfigureServer { name }))
                .ok();
        })
        .detach();
    }
}

/// The language servers of the turned-on plugins with the user's settings over them, in the
/// plugins' order; those the user turned off are left out.
pub fn configs(cx: &App) -> Vec<ServerConfig> {
    let user = settings::lsp(cx);
    crate::contributions::servers(cx)
        .into_iter()
        .filter_map(|config| match user.get(&config.name) {
            Some(own) => config::with_user_settings(config, own),
            None => Some(config),
        })
        .collect()
}

/// The plugin's config of `name` as the user set it up; `None` — turned off.
pub fn effective(config: &ServerConfig, cx: &App) -> Option<ServerConfig> {
    match settings::lsp_server(&config.name, cx) {
        Some(own) => config::with_user_settings(config.clone(), &own),
        None => Some(config.clone()),
    }
}

/// The user's own command for the server (`lsp.<server>.command`): Flux doesn't install the
/// plugin's in its place, and a command that isn't there is an error worth telling.
pub fn own_command(name: &str, cx: &App) -> Option<String> {
    settings::lsp_server(name, cx)?
        .get("command")?
        .as_str()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
}

/// Whether the server is on (`"enabled": false` turns it off).
pub fn is_enabled(name: &str, cx: &App) -> bool {
    settings::lsp_server(name, cx).is_none_or(|own| own.get("enabled") != Some(&Value::Bool(false)))
}

/// Turns the server on or off (the switch in Settings → Language Servers): `"enabled": false`
/// under its key, gone again when it is turned on (with the key, if nothing else is there).
pub fn set_enabled(name: &str, on: bool, cx: &mut App) {
    if is_enabled(name, cx) == on {
        return;
    }
    settings::update_lsp(cx, |lsp| {
        if on {
            if let Some(Value::Object(own)) = lsp.get_mut(name) {
                own.remove("enabled");
                if own.is_empty() {
                    lsp.remove(name);
                }
            }
        } else {
            let own = lsp
                .entry(name.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if !own.is_object() {
                *own = Value::Object(Map::new());
            }
            own["enabled"] = Value::Bool(false);
        }
    });
}

/// The section a server asks its settings by (`workspace/configuration`): "Configure…" puts it
/// under `settings`, for the user to fill in. Unknown servers get an empty `settings`.
fn settings_section(server: &str) -> Option<&'static str> {
    Some(match server {
        "rust-analyzer" => "rust-analyzer",
        "pyright" => "python",
        "gopls" => "gopls",
        "typescript-language-server" => "typescript",
        "yaml-language-server" => "yaml",
        "vscode-json-language-server" => "json",
        "vscode-css-language-server" => "css",
        "vscode-html-language-server" => "html",
        "taplo" => "evenBetterToml",
        "bash-language-server" => "bashIde",
        _ => return None,
    })
}

/// What "Configure…" adds for a server the user hasn't set up yet.
fn stub(server: &str) -> Value {
    let mut settings = Map::new();
    if let Some(section) = settings_section(server) {
        settings.insert(section.to_string(), Value::Object(Map::new()));
    }
    let mut own = Map::new();
    own.insert("settings".to_string(), Value::Object(settings));
    Value::Object(own)
}

/// "Configure…" of the server `server` (Settings → Language Servers, the servers' popup, a
/// notification about a project the server couldn't load): `settings.json` in a tab of the
/// window, the server's key selected (the key is added when missing). An open tab of the file
/// with unsaved edits keeps them: the key is looked for in them.
pub fn configure(
    workspace: &mut Workspace,
    server: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(path) = settings::file_path(cx) else {
        return crate::settings_file::open(workspace, None, window, cx);
    };
    let tab = workspace.editors(cx).into_iter().find(|editor| {
        editor
            .read(cx)
            .document
            .path()
            .is_some_and(|open| same_file(open, &path))
    });
    let edited = tab
        .as_ref()
        .is_some_and(|editor| editor.read(cx).document.is_modified());
    if !edited
        && settings::file_error(cx).is_none()
        && settings::lsp_server(server, cx).is_none()
    {
        settings::update_lsp(cx, |lsp| {
            lsp.insert(server.to_string(), stub(server));
        });
        // The tab shows the key at once (the file watcher would bring it a moment later).
        if let (Some(editor), Ok(text)) = (&tab, std::fs::read_to_string(&path)) {
            editor.update(cx, |editor, cx| editor.reload(&text, cx));
        }
    }
    let text = match &tab {
        Some(editor) => editor.read(cx).document.text().to_string(),
        None => std::fs::read_to_string(&path).unwrap_or_default(),
    };
    let place = find_key(&text, &["lsp", server]).or_else(|| find_key(&text, &["lsp"]));
    crate::settings_file::open(workspace, place, window, cx);
}

/// Whether two paths name the same file (one may come through a symbolic link, `/tmp`).
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
}

/// Where the key at `path` (`["lsp", "rust-analyzer"]`) is in the JSON `text`: its line and the
/// columns of its name without the quotes, 0-based, in characters. Formatting doesn't matter;
/// broken JSON may give `None`.
pub fn find_key(text: &str, path: &[&str]) -> Option<(usize, usize, usize)> {
    // Every object or array open at this point; an object knows the key of the value being read.
    struct Frame {
        object: bool,
        key: Option<String>,
    }
    let chars: Vec<char> = text.chars().collect();
    let mut stack: Vec<Frame> = Vec::new();
    let (mut line, mut column) = (0, 0);
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' | '[' => stack.push(Frame {
                object: chars[i] == '{',
                key: None,
            }),
            '}' | ']' => {
                stack.pop();
            }
            '\n' => {
                line += 1;
                column = 0;
                i += 1;
                continue;
            }
            '"' => {
                let start = column + 1;
                let mut end = i + 1;
                let mut name = String::new();
                while end < chars.len() && chars[end] != '"' && chars[end] != '\n' {
                    if chars[end] == '\\' && end + 1 < chars.len() {
                        end += 1;
                    }
                    name.push(chars[end]);
                    end += 1;
                }
                let next = chars[(end + 1).min(chars.len())..]
                    .iter()
                    .find(|c| !c.is_whitespace());
                let in_object = stack.last().is_some_and(|frame| frame.object);
                if next == Some(&':') && in_object {
                    let depth = stack.len();
                    let enclosing_match = depth <= path.len()
                        && stack[..depth - 1]
                            .iter()
                            .zip(path)
                            .all(|(frame, key)| frame.key.as_deref() == Some(*key));
                    if depth == path.len() && enclosing_match && name == path[depth - 1] {
                        return Some((line, start, start + (end - i - 1)));
                    }
                    stack.last_mut().expect("in an object").key = Some(name);
                }
                column += end - i;
                i = end;
            }
            _ => {}
        }
        column += 1;
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_found_by_their_path_whatever_the_formatting() {
        let text = "{\n  \"lsp\": {\n    \"gopls\": { \"enabled\": false },\n    \"rust-analyzer\": {\n      \"settings\": {}\n    }\n  },\n  \"markdown\": { \"layout\": \"split\" }\n}\n";
        assert_eq!(find_key(text, &["lsp"]), Some((1, 3, 6)));
        assert_eq!(find_key(text, &["lsp", "gopls"]), Some((2, 5, 10)));
        assert_eq!(find_key(text, &["lsp", "rust-analyzer"]), Some((3, 5, 18)));
        assert_eq!(
            find_key(text, &["lsp", "rust-analyzer", "settings"]),
            Some((4, 7, 15))
        );
        // A key of the same name elsewhere is not it.
        assert_eq!(find_key(text, &["layout"]), None);
        assert_eq!(find_key(text, &["markdown", "layout"]), Some((7, 17, 23)));
        assert_eq!(find_key(text, &["lsp", "pyright"]), None);
        // On one line, with escapes and values that look like keys.
        let text = r#"{"a": "lsp", "b\"": [{"lsp": 1}], "lsp": {"x": {}}}"#;
        assert_eq!(find_key(text, &["lsp"]), Some((0, 35, 38)));
        assert_eq!(find_key(text, &["lsp", "x"]), Some((0, 43, 44)));
        assert_eq!(find_key("not json", &["lsp"]), None);
    }

    #[test]
    fn configure_adds_the_section_the_server_asks_for() {
        assert_eq!(stub("rust-analyzer"), json!({ "settings": { "rust-analyzer": {} } }));
        assert_eq!(stub("pyright"), json!({ "settings": { "python": {} } }));
        assert_eq!(stub("some-server"), json!({ "settings": {} }));
        // Empty, it changes nothing in the server's config.
        let config = ServerConfig {
            name: "rust-analyzer".into(),
            command: "rust-analyzer".into(),
            args: vec![],
            extensions: vec!["rs".into()],
            file_names: vec![],
            initialization_options: None,
            settings: None,
            install: None,
            env: Default::default(),
        };
        assert_eq!(
            config::with_user_settings(config.clone(), &stub("rust-analyzer")),
            Some(config)
        );
    }
}
