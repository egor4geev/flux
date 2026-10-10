//! Plugins' proposed edits (part 8.2, `review` interface): a diff tab like Claude's — the file as it
//! is on the left, the proposal on the right (editable, hunks rejected one by one), Accept (⌘↵) and
//! Reject on a banner ([`crate::diff_view::DiffView`]'s proposal mode). The answer goes back to the
//! plugin as `proposal-answered`: accepted (the text as the user left it), rejected, or closed (the
//! tab closed without an answer, the plugin withdrew it, or an accepted text couldn't be written).
//! With `apply`, Flux writes the accepted text itself: into the open document as one undo step,
//! saved, or atomically into the file. Ids are the plugin's own.
//!
//! The file must be in the project, or in a folder of the plugin's `folders` permission (with
//! write access when Flux writes it); its left side is the open document's text, or the file on
//! disk (none — a new file).

use std::fs;
use std::io::{self, Write as _};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use flux_plugin::api::events::Event;
use flux_plugin::api::review::{Proposal, ProposalOutcome};
use flux_plugin::manifest::{FolderAccess, FolderPermission};
use gpui::{App, AppContext as _, Context, Entity, SharedString, Window};

use crate::diff_view::{DiffView, PluginProposal, PluginProposalAnswer};
use crate::editor::Editor;
use crate::i18n::trf;
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::workspace::Workspace;

/// Larger files aren't proposed (as the diff's limit).
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// How many leading bytes are checked for NUL to call a file binary.
const BINARY_PROBE: usize = 8000;

/// `review.propose`: opens the proposal's tab (the id is the plugin's); an error says why it
/// can't (a folder, a binary file, a path outside the project and the allowed folders).
pub(crate) fn propose(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    proposal: &Proposal,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let root = workspace.root().map(Path::to_path_buf);
    let path = resolve(root.as_deref(), &proposal.path)?;
    let (name, folders) = {
        let store = workspace.plugins.read(cx);
        let state = store
            .plugin(plugin)
            .ok_or_else(|| "The plugin isn't loaded".to_string())?;
        (
            SharedString::from(state.name().to_string()),
            state.entry.manifest.permissions.folders.clone(),
        )
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    allowed(&path, root.as_deref(), &folders, &home, proposal.apply)?;
    let original = match open_editor(workspace, &path, cx) {
        Some(editor) => Some(editor.read(cx).document.text().to_string()),
        None => read_file(&path)?,
    };
    let owner = PluginProposal {
        plugin: Arc::from(plugin),
        name,
        id,
        path,
        apply: proposal.apply,
        title: proposal
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .map(SharedString::from),
    };
    let git = workspace.git().clone();
    let text = proposal.text.clone();
    let view = cx.new(|cx| DiffView::plugin_proposal(original, text, owner, git, window, cx));
    watch(&view, cx);
    workspace.add_diff_view_tab(view, proposal.focus, window, cx);
    Ok(())
}

/// The plugin stopped: its proposals' tabs close (nobody waits for the answer).
pub(crate) fn plugin_stopped(
    workspace: &mut Workspace,
    plugin: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let _ = window;
    for view in plugin_views(workspace, plugin, None, cx) {
        view.update(cx, |view, cx| view.drop_proposal(cx));
    }
}

/// `review.withdraw`: closes the proposal's tab without an answer (the plugin hears `closed`).
pub(crate) fn withdraw(workspace: &mut Workspace, plugin: &str, id: u64, cx: &mut Context<Workspace>) {
    for view in plugin_views(workspace, plugin, Some(id), cx) {
        view.update(cx, |view, cx| view.withdraw_proposal(cx));
    }
}

/// The proposals' tabs of the plugin (one of them: `id`).
fn plugin_views(
    workspace: &Workspace,
    plugin: &str,
    id: Option<u64>,
    cx: &App,
) -> Vec<Entity<DiffView>> {
    workspace
        .diff_views()
        .into_iter()
        .filter(|view| {
            view.read(cx).proposed_by_plugin().is_some_and(|proposal| {
                &*proposal.plugin == plugin && id.is_none_or(|id| proposal.id == id)
            })
        })
        .collect()
}

/// The plugin hears the answer, or `closed` when the tab goes without one.
fn watch(view: &Entity<DiffView>, cx: &mut Context<Workspace>) {
    cx.subscribe(view, |workspace, _, answer: &PluginProposalAnswer, cx| {
        answered(workspace, answer, cx)
    })
    .detach();
    cx.observe_release(view, |workspace, view: &mut DiffView, cx| {
        if let Some(proposal) = view.unanswered_plugin_proposal() {
            workspace.plugins.read(cx).send_to(
                &proposal.plugin,
                Event::ProposalAnswered((proposal.id, ProposalOutcome::Closed)),
            );
        }
    })
    .detach();
}

/// Accept or Reject: an accepted text is written if the plugin asked for it, then the plugin
/// hears the answer. A text that can't be written is an error for the user and `closed` for the
/// plugin: nothing changed.
fn answered(workspace: &mut Workspace, answer: &PluginProposalAnswer, cx: &mut Context<Workspace>) {
    let proposal = &answer.proposal;
    let outcome = match &answer.accepted {
        None => ProposalOutcome::Rejected,
        Some(text) if !proposal.apply => ProposalOutcome::Accepted(text.clone()),
        Some(text) => match apply(workspace, &proposal.path, text, cx) {
            Ok(()) => ProposalOutcome::Accepted(text.clone()),
            Err(err) => {
                let name = proposal
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let notification = Notification::error(trf("Couldn't write {0}", &[&name]))
                    .body(err)
                    .group(NotificationGroup::Files);
                workspace.notify(notification, cx);
                ProposalOutcome::Closed
            }
        },
    };
    workspace
        .plugins
        .read(cx)
        .send_to(&proposal.plugin, Event::ProposalAnswered((proposal.id, outcome)));
}

/// Writes an accepted text: into the open document (only what changed, one undo step, then
/// saved), or atomically into the file (made with its folders if it is new). The text takes the
/// file's line endings.
fn apply(
    workspace: &mut Workspace,
    path: &Path,
    text: &str,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let Some(editor) = open_editor(workspace, path, cx) else {
        return write_file(path, text).map_err(|err| err.to_string());
    };
    editor.update(cx, |editor, cx| {
        if editor.read_only || editor.message.is_some() {
            return Err(format!("{} is read-only", path.display()));
        }
        let text = with_line_ending(text, editor.document.line_ending());
        let current = editor.document.text().to_string();
        if let Some((range, replacement)) = minimal_edit(&current, &text) {
            editor.replace_ranges(vec![(range, replacement)], cx);
        }
        // A failed save tells the user itself (the editor's "Couldn't save").
        editor.save(cx).detach();
        Ok(())
    })
}

/// The open document of `path`, if any.
fn open_editor(workspace: &Workspace, path: &Path, cx: &App) -> Option<Entity<Editor>> {
    workspace.editors(cx).into_iter().find(|editor| {
        editor
            .read(cx)
            .document
            .path()
            .is_some_and(|open| same_file(open, path))
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The file's text for the left side: none — it doesn't exist (a new file).
fn read_file(path: &Path) -> Result<Option<String>, String> {
    let shown = path.display();
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("Can't read {shown}: {err}")),
    };
    if metadata.is_dir() {
        return Err(format!("{shown} is a folder"));
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!("{shown} is too large to review (over 4 MB)"));
    }
    let bytes = fs::read(path).map_err(|err| format!("Can't read {shown}: {err}"))?;
    if bytes.iter().take(BINARY_PROBE).any(|byte| *byte == 0) {
        return Err(format!("{shown} is a binary file"));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| format!("{shown} isn't UTF-8 text"))
}

/// The proposal's file: relative to the project root, or absolute; `..` and `.` resolved.
fn resolve(root: Option<&Path>, given: &str) -> Result<PathBuf, String> {
    let given = given.trim();
    if given.is_empty() {
        return Err("The path is empty".into());
    }
    let path = Path::new(given);
    let path = match root {
        _ if path.is_absolute() => path.to_path_buf(),
        Some(root) => root.join(path),
        None => {
            return Err(format!(
                "The window has no project: give an absolute path, not {given}"
            ));
        }
    };
    Ok(normalize(&path))
}

/// `..` and `.` resolved without the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The path with its symlinks resolved as far as it exists (a new file's folder may not).
fn real_path(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(existing) {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn inside(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir) || real_path(path).starts_with(real_path(dir))
}

/// Whether the plugin may propose `path`: in the project, or in one of its `folders` (writable
/// when Flux writes the answer).
fn allowed(
    path: &Path,
    root: Option<&Path>,
    folders: &[FolderPermission],
    home: &Path,
    apply: bool,
) -> Result<(), String> {
    if root.is_some_and(|root| inside(path, &normalize(root))) {
        return Ok(());
    }
    let folder = folders
        .iter()
        .find(|folder| inside(path, &normalize(&folder.resolve(home))));
    match folder {
        Some(folder) if apply && folder.access != FolderAccess::Write => Err(format!(
            "{} is in {}, which the plugin may only read: Flux can't write it (`access = \
             \"write\"` in the permissions of flux-plugin.toml)",
            path.display(),
            folder.path
        )),
        Some(_) => Ok(()),
        None => Err(format!(
            "{} is outside the project and the folders the plugin may use (`folders` in the \
             permissions of flux-plugin.toml)",
            path.display()
        )),
    }
}

/// The text with the line endings of a document or a file: `"\r\n"` or `"\n"`.
fn with_line_ending(text: &str, ending: &str) -> String {
    let text = text.replace("\r\n", "\n");
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text
    }
}

/// The one span (in characters) that turns `old` into `new`, and its new text: what lies between
/// the common start and the common end. None — no change.
fn minimal_edit(old: &str, new: &str) -> Option<(Range<usize>, String)> {
    if old == new {
        return None;
    }
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let replacement = new[prefix..new.len() - suffix].iter().collect();
    Some((prefix..old.len() - suffix, replacement))
}

/// Writes a file atomically: a temporary file next to it, then a rename (the old file stays whole
/// if the write fails). A new file gets its folders; an existing one keeps its line endings, its
/// permissions, and its symlink.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let existing = fs::read(&target).ok();
    let text = match &existing {
        Some(bytes) if bytes.windows(2).any(|pair| pair == b"\r\n") => {
            with_line_ending(text, "\r\n")
        }
        Some(_) => with_line_ending(text, "\n"),
        None => text.to_string(),
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    let tmp = target.with_file_name(format!(
        ".{}.flux-tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&tmp, permissions)?;
        }
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("flux-review-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_resolve_against_the_project() {
        let root = Path::new("/work/app");
        assert_eq!(resolve(Some(root), "src/main.rs"), Ok(root.join("src/main.rs")));
        assert_eq!(resolve(Some(root), "./a/../b.rs"), Ok(root.join("b.rs")));
        assert_eq!(resolve(Some(root), "/etc/hosts"), Ok(PathBuf::from("/etc/hosts")));
        assert!(resolve(Some(root), "  ").is_err());
        assert!(resolve(None, "src/main.rs").is_err());
        assert_eq!(resolve(None, "/tmp/x"), Ok(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn proposals_stay_in_the_project_and_the_allowed_folders() {
        let root = Path::new("/work/app");
        let home = Path::new("/Users/me");
        let folders = [
            FolderPermission {
                path: "~/.config/tool".into(),
                access: FolderAccess::Read,
            },
            FolderPermission {
                path: "/opt/shared".into(),
                access: FolderAccess::Write,
            },
        ];
        let ok = |path: &str, apply| allowed(Path::new(path), Some(root), &folders, home, apply);
        assert!(ok("/work/app/src/main.rs", true).is_ok());
        // `..` is resolved before the check: the file is outside.
        let outside = resolve(Some(root), "../other/x.rs").unwrap();
        assert!(allowed(&outside, Some(root), &folders, home, false).is_err());
        assert!(ok("/work/other/x.rs", false).is_err());
        assert!(ok("/Users/me/.config/tool/config.toml", false).is_ok());
        // Read access: proposing is fine, writing isn't.
        assert!(ok("/Users/me/.config/tool/config.toml", true).is_err());
        assert!(ok("/opt/shared/notes.md", true).is_ok());
        assert!(allowed(Path::new("/work/app/a.rs"), None, &[], home, false).is_err());
    }

    #[test]
    fn line_endings_follow_the_file() {
        assert_eq!(with_line_ending("a\nb\n", "\r\n"), "a\r\nb\r\n");
        assert_eq!(with_line_ending("a\r\nb\n", "\r\n"), "a\r\nb\r\n");
        assert_eq!(with_line_ending("a\r\nb\r\n", "\n"), "a\nb\n");
    }

    #[test]
    fn edits_cover_only_what_changed() {
        assert_eq!(minimal_edit("same", "same"), None);
        assert_eq!(
            minimal_edit("fn main() {}\n", "fn main() { run(); }\n"),
            Some((11..11, " run(); ".to_string()))
        );
        assert_eq!(minimal_edit("abc", ""), Some((0..3, String::new())));
        assert_eq!(minimal_edit("", "new"), Some((0..0, "new".to_string())));
        // Characters, not bytes: the rope counts characters.
        assert_eq!(
            minimal_edit("привет мир", "привет, мир"),
            Some((6..6, ",".to_string()))
        );
        // A repeated letter at the seam is counted once.
        assert_eq!(minimal_edit("aaa", "aaaa"), Some((3..3, "a".to_string())));
    }

    #[test]
    fn files_are_read_for_the_left_side() {
        let dir = temp("read");
        assert_eq!(read_file(&dir.join("new.rs")), Ok(None));
        fs::write(dir.join("text.rs"), "fn main() {}\n").unwrap();
        assert_eq!(read_file(&dir.join("text.rs")), Ok(Some("fn main() {}\n".into())));
        fs::write(dir.join("bin"), [0u8, 1, 2]).unwrap();
        assert!(read_file(&dir.join("bin")).is_err());
        assert!(read_file(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_are_written_whole_with_their_line_endings() {
        let dir = temp("write");
        let new = dir.join("deep/er/new.txt");
        write_file(&new, "one\ntwo\n").unwrap();
        assert_eq!(fs::read_to_string(&new).unwrap(), "one\ntwo\n");
        let crlf = dir.join("crlf.txt");
        fs::write(&crlf, "a\r\nb\r\n").unwrap();
        write_file(&crlf, "a\nb\nc\n").unwrap();
        assert_eq!(fs::read_to_string(&crlf).unwrap(), "a\r\nb\r\nc\r\n");
        assert!(!dir.join(".crlf.txt.flux-tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
