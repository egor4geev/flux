//! The calls of the plugin API 0.2 that the window does the work of (part 8.2): Git, the problems,
//! closing a tab, the browser and the clipboard here; terminals ([`crate::plugin_terminals`]) and
//! proposals ([`crate::plugin_review`]) in their own modules. The plugin's thread waits for the
//! reply to the calls that return something ([`WindowReply`]) — up to 10 s, so slow work (a diff,
//! reading files) runs in the background and replies from there; the window checks that a
//! terminal or a proposal is the plugin's own. Permissions are checked by `flux-plugin` before a
//! call comes here.
//!
//! - Git: the repositories of the project (root, branch, HEAD, operation), the changed files of all
//!   of them (absolute paths), a file's diff against HEAD (`git diff HEAD`; a file HEAD doesn't
//!   have — all of it as new).
//! - Problems: what Flux knows ([`crate::diagnostics::known_files`], shared with Flux's tools for
//!   Claude): every open document's (its servers' and plugins'), what servers published for files
//!   no editor shows, and plugins' for such files; positions are lines and character columns. A
//!   plugin's own problems are kept by [`crate::plugins::PluginStore`].
//! - A tab closes only without unsaved changes: they are the user's to decide about.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use flux_core::Rope;
use flux_git::{FileStatus, GitError, Repo, RepoState};
use flux_plugin::api::diagnostics::{
    Diagnostic as ApiDiagnostic, FileDiagnostics, Severity as ApiSeverity,
};
use flux_plugin::api::git::{Change as ApiChange, ChangeKind, Repository};
use flux_plugin::api::types::Range as ApiRange;
use flux_plugin::runtime::{WindowCall, WindowReply};
use gpui::{AppContext as _, ClipboardItem, Context, Window};

use crate::diagnostics::{Diagnostic, KnownFile, Severity};
use crate::git::GitStore;
use crate::i18n::{tr, trf};
use crate::plugins::{path_text, resolve, to_offset, to_position};
use crate::workspace::Workspace;

/// A diff larger than this isn't given to a plugin.
const MAX_DIFF_BYTES: usize = 16 << 20;
/// A file whose problems a server published, larger than this, isn't read to turn the server's
/// positions into characters (its positions are passed as they are).
const MAX_READ_BYTES: u64 = 8 << 20;

/// Carries out a plugin's [`WindowCall`] and replies through `reply`.
pub(crate) fn window_call(
    workspace: &mut Workspace,
    plugin: &str,
    call: &WindowCall,
    reply: &Sender<WindowReply>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let send = |answer: WindowReply| {
        reply.send(answer).ok();
    };
    match call {
        WindowCall::OpenTerminal(options) => send(WindowReply::Opened(
            crate::plugin_terminals::open(workspace, plugin, options, window, cx),
        )),
        WindowCall::SendText { terminal, text } => send(WindowReply::Done(
            crate::plugin_terminals::send_text(workspace, plugin, *terminal, text, cx),
        )),
        WindowCall::ShowTerminal { terminal, focus } => send(WindowReply::Done(
            crate::plugin_terminals::show(workspace, plugin, *terminal, *focus, window, cx),
        )),
        WindowCall::CloseTerminal(terminal) => {
            crate::plugin_terminals::close(workspace, plugin, *terminal, window, cx)
        }
        WindowCall::Propose { id, proposal } => send(WindowReply::Done(
            crate::plugin_review::propose(workspace, plugin, *id, proposal, window, cx),
        )),
        WindowCall::Withdraw(id) => crate::plugin_review::withdraw(workspace, plugin, *id, cx),
        WindowCall::CloseEditor(id) => {
            send(WindowReply::Done(close_editor(workspace, *id, window, cx)))
        }
        WindowCall::Repositories => send(WindowReply::Repositories(repositories(
            workspace.git().read(cx),
        ))),
        WindowCall::GitStatus => send(WindowReply::Changes(changes(workspace.git().read(cx)))),
        WindowCall::GitDiff(path) => git_diff(workspace, path, reply.clone(), cx),
        WindowCall::Diagnostics(path) => {
            get_diagnostics(workspace, path.as_deref(), reply.clone(), cx)
        }
        WindowCall::PublishDiagnostics { path, diagnostics } => {
            let path = resolve(workspace.root(), path);
            let diagnostics = diagnostics.clone();
            workspace.plugins.update(cx, |store, cx| {
                store.publish_diagnostics(plugin, path, diagnostics, cx)
            });
        }
        WindowCall::ClearDiagnostics => workspace
            .plugins
            .update(cx, |store, cx| store.clear_diagnostics(plugin, cx)),
        WindowCall::OpenUrl(url) => cx.open_url(url),
        WindowCall::CopyText(text) => cx.write_to_clipboard(ClipboardItem::new_string(text.clone())),
    }
}

// --- Tabs ---

/// `editors.close`: closes the document's tab, unless it has unsaved changes.
fn close_editor(
    workspace: &mut Workspace,
    id: u64,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<(), String> {
    let Some(editor) = workspace
        .editors(cx)
        .into_iter()
        .find(|editor| editor.entity_id().as_u64() == id)
    else {
        return Err(tr("The document is closed").into());
    };
    if editor.read(cx).document.is_modified() {
        return Err(tr("The document has unsaved changes: they are the user's to decide about").into());
    }
    workspace.remove_tab(&editor, window, cx);
    // A working copy a diff opened has no tab of its own: it stays with the diff.
    if workspace.editors(cx).contains(&editor) {
        return Err(tr("The document has no tab of its own: it is shown in a diff").into());
    }
    Ok(())
}

// --- Git ---

/// `git.repositories`: every repository of the project.
fn repositories(git: &GitStore) -> Vec<Repository> {
    git.repos()
        .iter()
        .map(|entry| Repository {
            root: path_text(&entry.repo.work_dir),
            branch: entry.status.branch.head.clone(),
            head: entry.status.branch.oid.clone(),
            operation: operation_name(entry.operation.state).map(Into::into),
        })
        .collect()
}

/// The name of an operation in progress, as the API gives it.
fn operation_name(state: RepoState) -> Option<&'static str> {
    match state {
        RepoState::Normal => None,
        RepoState::Merging => Some("merge"),
        RepoState::Rebasing => Some("rebase"),
        RepoState::CherryPicking => Some("cherry-pick"),
        RepoState::Reverting => Some("revert"),
        RepoState::Bisecting => Some("bisect"),
    }
}

/// `git.status`: the changed files of every repository.
fn changes(git: &GitStore) -> Vec<ApiChange> {
    git.changes()
        .into_iter()
        .map(|change| ApiChange {
            path: path_text(&change.path),
            old_path: change.orig_path.as_deref().map(path_text),
            kind: change_kind(change.status),
        })
        .collect()
}

fn change_kind(status: FileStatus) -> ChangeKind {
    match status {
        FileStatus::Modified | FileStatus::TypeChanged => ChangeKind::Modified,
        FileStatus::Added => ChangeKind::Added,
        FileStatus::Deleted => ChangeKind::Deleted,
        FileStatus::Renamed => ChangeKind::Renamed,
        FileStatus::Untracked => ChangeKind::Untracked,
        FileStatus::Conflicted => ChangeKind::Conflicted,
    }
}

/// What the plugins hear of the repositories, as a number: the roots, the branches, HEADs,
/// operations and changed files. `git-changed` comes when it changes — the store notifies of much
/// more (progress, branches read, the commit's checkboxes).
pub(crate) fn git_state(git: &GitStore) -> u64 {
    let mut hasher = DefaultHasher::new();
    for entry in git.repos() {
        entry.repo.work_dir.hash(&mut hasher);
        entry.status.branch.head.hash(&mut hasher);
        entry.status.branch.oid.hash(&mut hasher);
        operation_name(entry.operation.state).hash(&mut hasher);
        for change in &entry.status.entries {
            change.path.hash(&mut hasher);
            change.orig_path.hash(&mut hasher);
            change.status.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// `git.diff`: the file's diff against HEAD, read in the background; the reply goes from there.
fn git_diff(
    workspace: &Workspace,
    path: &str,
    reply: Sender<WindowReply>,
    cx: &mut Context<Workspace>,
) {
    let path = resolve(workspace.root(), path);
    let git = workspace.git().read(cx);
    let Some(entry) = git.repo_for(&path) else {
        let error = trf(
            "{0} isn't in a Git repository of the project",
            &[&path_text(&path)],
        );
        reply.send(WindowReply::Text(Err(error))).ok();
        return;
    };
    let repo = entry.repo.clone();
    let untracked = git.status_of(&path) == Some(FileStatus::Untracked);
    cx.background_spawn(async move {
        reply
            .send(WindowReply::Text(file_diff(&repo, &path, untracked)))
            .ok();
    })
    .detach();
}

/// A file's changes against HEAD as a unified diff (`git diff HEAD -- <file>`, the file as it is
/// on disk); a file HEAD doesn't have — untracked, or no commit yet — all of it as new.
fn file_diff(repo: &Repo, path: &Path, untracked: bool) -> Result<String, String> {
    let relative = repo
        .relative(path)
        .ok_or_else(|| format!("{} isn't in the repository", path.display()))?;
    if !untracked {
        let diff = repo
            .git()
            .read_only()
            .literal_pathspecs()
            .args(["diff", "--no-ext-diff", "HEAD", "--", relative.as_str()])
            .output();
        match diff {
            Ok(bytes) => return limited(bytes),
            Err(err) if !no_head(&err) => return Err(err.to_string()),
            Err(_) => {}
        }
    }
    if !path.is_file() {
        return Ok(String::new());
    }
    let output = repo
        .git()
        .read_only()
        .args([
            "diff",
            "--no-ext-diff",
            "--no-index",
            "--",
            "/dev/null",
            relative.as_str(),
        ])
        .run_unchecked(None, |_| {})
        .map_err(|err| err.to_string())?;
    // `--no-index` exits with 1 when the files differ: that's the diff.
    if !output.success && output.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    limited(output.stdout)
}

/// The repository has no commit yet: `HEAD` names nothing.
fn no_head(err: &GitError) -> bool {
    let text = err.to_string();
    text.contains("HEAD")
        && ["unknown revision", "bad revision", "ambiguous argument"]
            .iter()
            .any(|known| text.contains(known))
}

fn limited(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.len() > MAX_DIFF_BYTES {
        return Err(format!(
            "The diff is larger than {} MB",
            MAX_DIFF_BYTES >> 20
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// --- Problems ---

/// `diagnostics.get`: the problems Flux knows of — in one file (`path`), or in every file with
/// problems. Open documents are read now; what servers published for other files has the servers'
/// positions (UTF-16), turned into characters with the file's text read in the background.
fn get_diagnostics(
    workspace: &Workspace,
    path: Option<&str>,
    reply: Sender<WindowReply>,
    cx: &mut Context<Workspace>,
) {
    let wanted = path.map(|path| crate::navigation::canonical(&resolve(workspace.root(), path)));
    let mut ready: Vec<FileDiagnostics> = Vec::new();
    let mut published: Vec<(PathBuf, Vec<flux_lsp::lsp_types::Diagnostic>)> = Vec::new();
    let mut open: Vec<PathBuf> = Vec::new();
    for (file, known) in crate::diagnostics::known_files(workspace, cx) {
        let canonical = crate::navigation::canonical(&file);
        if let KnownFile::Open(_) = known {
            open.push(canonical.clone());
        }
        if wanted.as_ref().is_some_and(|wanted| *wanted != canonical) {
            continue;
        }
        match known {
            KnownFile::Open(editor) => {
                let editor = editor.read(cx);
                let text = editor.document.text();
                let diagnostics: Vec<ApiDiagnostic> = editor
                    .diagnostics
                    .iter()
                    .map(|diagnostic| from_editor(text, diagnostic))
                    .collect();
                if !diagnostics.is_empty() {
                    ready.push(FileDiagnostics {
                        path: path_text(&file),
                        diagnostics,
                    });
                }
            }
            KnownFile::Published(diagnostics) => published.push((file, diagnostics)),
        }
    }
    let plugins: Vec<(PathBuf, Vec<ApiDiagnostic>)> = workspace
        .plugins
        .read(cx)
        .published_elsewhere(&open)
        .into_iter()
        .filter(|(file, _)| {
            wanted
                .as_ref()
                .is_none_or(|wanted| *wanted == crate::navigation::canonical(file))
        })
        .collect();
    cx.background_spawn(async move {
        let mut files = ready;
        for (file, diagnostics) in published {
            let text = std::fs::metadata(&file)
                .ok()
                .filter(|meta| meta.len() <= MAX_READ_BYTES)
                .and_then(|_| std::fs::read_to_string(&file).ok())
                .map(|text| Rope::from_str(&text));
            let diagnostics = match &text {
                Some(text) => crate::diagnostics::from_lsp(text, &diagnostics)
                    .iter()
                    .map(|diagnostic| from_editor(text, diagnostic))
                    .collect(),
                None => diagnostics.iter().map(from_lsp_as_sent).collect(),
            };
            merge(&mut files, &file, diagnostics);
        }
        for (file, diagnostics) in plugins {
            merge(&mut files, &file, diagnostics);
        }
        for file in &mut files {
            file.diagnostics.sort_by_key(|diagnostic| {
                (
                    diagnostic.range.start.line,
                    diagnostic.range.start.column,
                    severity_rank(diagnostic.severity),
                )
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        reply.send(WindowReply::Diagnostics(files)).ok();
    })
    .detach();
}

/// Adds a file's problems to the list: to its entry, if it has one.
fn merge(files: &mut Vec<FileDiagnostics>, file: &Path, diagnostics: Vec<ApiDiagnostic>) {
    if diagnostics.is_empty() {
        return;
    }
    let path = path_text(file);
    match files.iter_mut().find(|known| known.path == path) {
        Some(known) => known.diagnostics.extend(diagnostics),
        None => files.push(FileDiagnostics { path, diagnostics }),
    }
}

fn severity_rank(severity: ApiSeverity) -> u8 {
    match severity {
        ApiSeverity::Error => 0,
        ApiSeverity::Warning => 1,
        ApiSeverity::Info => 2,
        ApiSeverity::Hint => 3,
    }
}

fn api_severity(severity: Severity) -> ApiSeverity {
    match severity {
        Severity::Error => ApiSeverity::Error,
        Severity::Warning => ApiSeverity::Warning,
        Severity::Info => ApiSeverity::Info,
        Severity::Hint => ApiSeverity::Hint,
    }
}

fn severity(severity: ApiSeverity) -> Severity {
    match severity {
        ApiSeverity::Error => Severity::Error,
        ApiSeverity::Warning => Severity::Warning,
        ApiSeverity::Info => Severity::Info,
        ApiSeverity::Hint => Severity::Hint,
    }
}

/// A document's problem as the API gives it: positions in lines and characters of `text`.
fn from_editor(text: &Rope, diagnostic: &Diagnostic) -> ApiDiagnostic {
    ApiDiagnostic {
        range: ApiRange {
            start: to_position(text, diagnostic.range.start),
            end: to_position(text, diagnostic.range.end),
        },
        severity: api_severity(diagnostic.severity),
        message: diagnostic.message.clone(),
        source: diagnostic.source.clone(),
        code: diagnostic.code.clone(),
    }
}

/// A server's problem of a file that couldn't be read: its positions as the server sent them.
fn from_lsp_as_sent(diagnostic: &flux_lsp::lsp_types::Diagnostic) -> ApiDiagnostic {
    let position = |position: flux_lsp::lsp_types::Position| flux_plugin::api::types::Position {
        line: position.line,
        column: position.character,
    };
    ApiDiagnostic {
        range: ApiRange {
            start: position(diagnostic.range.start),
            end: position(diagnostic.range.end),
        },
        severity: api_severity(crate::diagnostics::severity_from_lsp(diagnostic)),
        message: diagnostic.message.clone(),
        source: diagnostic.source.clone(),
        code: crate::diagnostics::code_from_lsp(diagnostic),
    }
}

/// A plugin's problems → the document's, positions in `text` (clamped to it; a backwards range
/// is turned around); without a source — `source` (the plugin's name). The owner is set by
/// [`crate::diagnostics::Diagnostics::set`].
pub(crate) fn to_editor_diagnostics(
    text: &Rope,
    published: &[ApiDiagnostic],
    source: &str,
) -> Vec<Diagnostic> {
    published
        .iter()
        .map(|diagnostic| {
            let start = to_offset(text, &diagnostic.range.start);
            let end = to_offset(text, &diagnostic.range.end);
            Diagnostic {
                range: start.min(end)..start.max(end),
                severity: severity(diagnostic.severity),
                message: diagnostic.message.clone(),
                source: Some(
                    diagnostic
                        .source
                        .clone()
                        .filter(|source| !source.trim().is_empty())
                        .unwrap_or_else(|| source.to_string()),
                ),
                code: diagnostic.code.clone(),
                owner: 0,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::api::types::Position;

    fn at(line: u32, column: u32) -> Position {
        Position { line, column }
    }

    fn problem(start: Position, end: Position, source: Option<&str>) -> ApiDiagnostic {
        ApiDiagnostic {
            range: ApiRange { start, end },
            severity: ApiSeverity::Warning,
            message: "unused".into(),
            source: source.map(Into::into),
            code: Some("W1".into()),
        }
    }

    #[test]
    fn statuses_become_change_kinds() {
        assert_eq!(change_kind(FileStatus::Modified), ChangeKind::Modified);
        assert_eq!(change_kind(FileStatus::TypeChanged), ChangeKind::Modified);
        assert_eq!(change_kind(FileStatus::Added), ChangeKind::Added);
        assert_eq!(change_kind(FileStatus::Deleted), ChangeKind::Deleted);
        assert_eq!(change_kind(FileStatus::Renamed), ChangeKind::Renamed);
        assert_eq!(change_kind(FileStatus::Untracked), ChangeKind::Untracked);
        assert_eq!(change_kind(FileStatus::Conflicted), ChangeKind::Conflicted);
        assert_eq!(operation_name(RepoState::Normal), None);
        assert_eq!(operation_name(RepoState::CherryPicking), Some("cherry-pick"));
    }

    #[test]
    fn published_problems_land_on_characters_of_the_text() {
        // "é" and "🦀" are one character each (two and four bytes, the crab is two UTF-16 units).
        let text = Rope::from_str("fn é() {}\nlet 🦀 = 1;\n");
        let items = to_editor_diagnostics(
            &text,
            &[
                problem(at(1, 4), at(1, 5), None),
                // Backwards, and past the end of its line: turned around and clamped.
                problem(at(0, 40), at(0, 3), Some("lint")),
                // Past the last line: the end of the text.
                problem(at(9, 0), at(9, 0), Some("  ")),
            ],
            "Linter",
        );
        assert_eq!(text.slice(items[0].range.clone()).to_string(), "🦀");
        assert_eq!(text.slice(items[1].range.clone()).to_string(), "é() {}");
        assert_eq!(items[0].source.as_deref(), Some("Linter"));
        assert_eq!(items[1].source.as_deref(), Some("lint"));
        assert_eq!(items[2].source.as_deref(), Some("Linter"), "a blank source is none");
        assert_eq!(items[2].range, text.len_chars()..text.len_chars());
        assert_eq!(items[0].severity, Severity::Warning);
        assert_eq!(items[0].code.as_deref(), Some("W1"));
        // And back: the same lines and columns.
        let back = from_editor(&text, &items[0]);
        assert_eq!(back.range.start, at(1, 4));
        assert_eq!(back.range.end, at(1, 5));
        assert_eq!(back.severity, ApiSeverity::Warning);
    }

    #[test]
    fn server_positions_are_kept_when_the_file_cant_be_read() {
        use flux_lsp::lsp_types::{self, DiagnosticSeverity};
        let diagnostic = lsp_types::Diagnostic {
            range: lsp_types::Range::new(
                lsp_types::Position::new(2, 6),
                lsp_types::Position::new(2, 9),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            message: "mismatched types".into(),
            source: Some("rustc".into()),
            code: Some(lsp_types::NumberOrString::String("E0308".into())),
            ..Default::default()
        };
        let item = from_lsp_as_sent(&diagnostic);
        assert_eq!(item.range.start, at(2, 6));
        assert_eq!(item.range.end, at(2, 9));
        assert_eq!(item.severity, ApiSeverity::Error);
        assert_eq!(item.code.as_deref(), Some("E0308"));
        assert_eq!(item.source.as_deref(), Some("rustc"));
    }

    #[test]
    fn files_merge_their_problems() {
        let mut files = Vec::new();
        merge(&mut files, Path::new("/p/a.rs"), vec![problem(at(0, 0), at(0, 1), None)]);
        merge(&mut files, Path::new("/p/b.rs"), Vec::new());
        merge(&mut files, Path::new("/p/a.rs"), vec![problem(at(1, 0), at(1, 1), None)]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].diagnostics.len(), 2);
    }

    #[test]
    fn a_diff_against_head_and_of_a_new_file() {
        let dir = std::env::temp_dir().join(format!("flux-plugin-calls-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(status.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let repo = Repo::discover(&dir).unwrap();
        let file = repo.work_dir.join("a.txt");
        // No commit yet: the whole file is new.
        let diff = file_diff(&repo, &file, false).unwrap();
        assert!(diff.contains("+one"), "{diff}");
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "first"]);
        assert_eq!(file_diff(&repo, &file, false).unwrap(), "");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        let diff = file_diff(&repo, &file, false).unwrap();
        assert!(diff.contains("+two") && diff.contains("a/a.txt"), "{diff}");
        let new = repo.work_dir.join("b.txt");
        std::fs::write(&new, "fresh\n").unwrap();
        let diff = file_diff(&repo, &new, true).unwrap();
        assert!(diff.contains("+fresh") && diff.contains("new file"), "{diff}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
