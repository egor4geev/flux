//! Plugins' terminals (part 8.2, `terminal` interface): a tab with a command (or the user's shell)
//! in the terminal panel or among the editor's tabs, text typed into it, `terminal-exited` when the
//! command ends (its tab stays, with the exit code, as the Run window of JetBrains IDEs) and
//! `terminal-closed` when the tab closes. A plugin reaches only the terminals it opened.
//!
//! A plugin's terminal is a pane ([`TerminalView`]) whose `plugin` names its owner; its id is the
//! pane's entity id. The user may split its tab or move it between the panel and the editor area:
//! the pane is still the plugin's. When the plugin stops, its panes become the user's (their tabs
//! stay).
//!
//! A command runs through the user's login shell (`$SHELL -l -c 'exec "$0" "$@"' program args…`):
//! the profile gives it the `PATH` an app started from the Dock lacks, and `exec` keeps the
//! program's exit code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use flux_plugin::api::events::Event;
use flux_plugin::api::terminal::{TerminalLocation, TerminalOptions};
use gpui::{App, Context, Entity, Focusable, SharedString, Window};

use crate::terminal_group::TerminalGroup;
use crate::terminal_view::{SpawnSpec, TerminalView, TerminalViewEvent};
use crate::workspace::Workspace;

/// `terminal.open`: opens the tab and returns the terminal's id.
pub(crate) fn open(
    workspace: &mut Workspace,
    plugin: &str,
    options: &TerminalOptions,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<u64, String> {
    let root = workspace.root().map(Path::to_path_buf);
    let cwd = working_dir(root.as_deref(), options.cwd.as_deref())?;
    let (command, default_title) = match options.command.as_deref() {
        None | Some([]) => (None, None),
        Some([program, args @ ..]) => {
            if program.trim().is_empty() {
                return Err("The command's program is empty".into());
            }
            let (shell, _) = flux_term::login_shell();
            let name = Path::new(program)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            (Some(through_login_shell(&shell, program, args)), name)
        }
    };
    let title = options
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .or(default_title)
        .map(SharedString::from);
    let spec = SpawnSpec {
        keep_on_exit: command.is_some(),
        command,
        cwd,
        env: options.env.clone(),
        title,
        plugin: Some(Arc::from(plugin)),
    };
    let group = TerminalGroup::spawn_with(spec, root, window, cx)
        .map_err(|err| format!("Couldn't start the terminal: {err}"))?;
    let view = group.read(cx).active_view();
    let id = view.entity_id().as_u64();
    watch(&view, cx);
    let in_editor = options.location == TerminalLocation::Editor;
    workspace.add_plugin_terminal(group, in_editor, options.focus, window, cx);
    Ok(id)
}

/// `terminal.send-text`: types the text as if the user did.
pub(crate) fn send_text(
    workspace: &mut Workspace,
    plugin: &str,
    terminal: u64,
    text: &str,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let (_, view) = find(workspace, plugin, terminal, cx).ok_or_else(|| unknown(terminal))?;
    if view.update(cx, |view, cx| view.send_text(text, cx)) {
        Ok(())
    } else {
        Err(format!("The command of terminal {terminal} has ended"))
    }
}

/// `terminal.show`: brings the tab forward; `focus` — it takes the keyboard.
pub(crate) fn show(
    workspace: &mut Workspace,
    plugin: &str,
    terminal: u64,
    focus: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let (group, view) = find(workspace, plugin, terminal, cx).ok_or_else(|| unknown(terminal))?;
    if !workspace.show_terminal_group(&group, focus, window, cx) {
        return Err(unknown(terminal));
    }
    // The tab may be split: the keyboard goes to the plugin's pane, not the tab's last one.
    if focus {
        window.focus(&view.focus_handle(cx));
    }
    Ok(())
}

/// The plugin stopped: its terminals are no longer its (their tabs stay, the user's).
pub(crate) fn plugin_stopped(workspace: &mut Workspace, plugin: &str, cx: &mut Context<Workspace>) {
    for view in plugin_views(workspace, plugin, cx) {
        view.update(cx, |view, _| view.plugin = None);
    }
}

/// `terminal.close`: closes the tab; what runs in it ends.
pub(crate) fn close(
    workspace: &mut Workspace,
    plugin: &str,
    terminal: u64,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some((group, view)) = find(workspace, plugin, terminal, cx) {
        group.update(cx, |group, cx| group.close_view(&view, window, cx));
    }
}

/// The plugin hears when its terminal's command ends and when the pane goes away.
fn watch(view: &Entity<TerminalView>, cx: &mut Context<Workspace>) {
    let id = view.entity_id().as_u64();
    cx.subscribe(view, move |workspace, view, event: &TerminalViewEvent, cx| {
        if let TerminalViewEvent::ProcessEnded(code) = event
            && let Some(plugin) = view.read(cx).plugin.clone()
        {
            workspace
                .plugins
                .read(cx)
                .send_to(&plugin, Event::TerminalExited((id, *code)));
        }
    })
    .detach();
    cx.observe_release(view, move |workspace, view: &mut TerminalView, cx| {
        if let Some(plugin) = view.plugin.take() {
            workspace
                .plugins
                .read(cx)
                .send_to(&plugin, Event::TerminalClosed(id));
        }
    })
    .detach();
}

/// The plugin's terminal `id`: its tab and its pane. None — no such terminal, or not the plugin's.
fn find(
    workspace: &Workspace,
    plugin: &str,
    id: u64,
    cx: &App,
) -> Option<(Entity<TerminalGroup>, Entity<TerminalView>)> {
    workspace.terminal_groups(cx).into_iter().find_map(|group| {
        let view = group
            .read(cx)
            .views()
            .into_iter()
            .find(|view| view.entity_id().as_u64() == id)?;
        (view.read(cx).plugin.as_deref() == Some(plugin)).then_some((group, view))
    })
}

/// The panes the plugin opened.
fn plugin_views(workspace: &Workspace, plugin: &str, cx: &App) -> Vec<Entity<TerminalView>> {
    workspace
        .terminal_groups(cx)
        .into_iter()
        .flat_map(|group| group.read(cx).views())
        .filter(|view| view.read(cx).plugin.as_deref() == Some(plugin))
        .collect()
}

fn unknown(terminal: u64) -> String {
    format!("No terminal {terminal} of this plugin")
}

/// Where the terminal starts: the folder given (relative to the project root), or the root.
fn working_dir(root: Option<&Path>, given: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(given) = given.map(str::trim).filter(|given| !given.is_empty()) else {
        return Ok(root.map(Path::to_path_buf));
    };
    let path = Path::new(given);
    let path = match root {
        Some(root) if path.is_relative() => root.join(path),
        None if path.is_relative() => {
            return Err(format!(
                "The window has no project: give an absolute folder, not {given}"
            ));
        }
        _ => path.to_path_buf(),
    };
    if !path.is_dir() {
        return Err(format!("No such folder: {}", path.display()));
    }
    Ok(Some(path))
}

/// The command line that runs `program` with `args` through the user's login shell: its profile
/// gives the `PATH` an app started from the Dock lacks, and `exec` keeps the program's exit code.
/// A shell that doesn't speak `sh` (other than fish) is replaced by zsh, the macOS default.
fn through_login_shell(shell: &str, program: &str, args: &[String]) -> (String, Vec<String>) {
    let name = Path::new(shell)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (shell, script) = match name.as_str() {
        "fish" => (shell, "exec $argv"),
        "zsh" | "bash" | "sh" | "dash" | "ksh" | "mksh" | "yash" => (shell, "exec \"$0\" \"$@\""),
        _ => ("/bin/zsh", "exec \"$0\" \"$@\""),
    };
    let mut argv = vec![
        "-l".to_string(),
        "-c".to_string(),
        script.to_string(),
        program.to_string(),
    ];
    argv.extend(args.iter().cloned());
    (shell.to_string(), argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_run_through_the_login_shell() {
        let args = ["test".to_string(), "--all".to_string()];
        let (shell, argv) = through_login_shell("/bin/zsh", "cargo", &args);
        assert_eq!(shell, "/bin/zsh");
        assert_eq!(argv, ["-l", "-c", "exec \"$0\" \"$@\"", "cargo", "test", "--all"]);
        let (shell, argv) = through_login_shell("/opt/homebrew/bin/fish", "npm", &[]);
        assert_eq!(shell, "/opt/homebrew/bin/fish");
        assert_eq!(argv, ["-l", "-c", "exec $argv", "npm"]);
        // nushell doesn't take `-c 'exec "$0" "$@"'`: zsh runs the command instead.
        let (shell, _) = through_login_shell("/opt/homebrew/bin/nu", "make", &[]);
        assert_eq!(shell, "/bin/zsh");
    }

    #[test]
    fn the_exit_code_survives_the_login_shell() {
        let (shell, argv) = through_login_shell("/bin/sh", "sh", &["-c".into(), "exit 3".into()]);
        let status = std::process::Command::new(shell).args(argv).status().unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn working_dirs_are_in_the_project() {
        let root = std::env::temp_dir().join(format!("flux-term-dir-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        assert_eq!(working_dir(Some(&root), None), Ok(Some(root.clone())));
        assert_eq!(working_dir(Some(&root), Some("  ")), Ok(Some(root.clone())));
        assert_eq!(working_dir(Some(&root), Some("sub")), Ok(Some(root.join("sub"))));
        assert!(working_dir(Some(&root), Some("missing")).is_err());
        assert!(working_dir(None, Some("sub")).is_err());
        assert_eq!(working_dir(None, None), Ok(None));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
