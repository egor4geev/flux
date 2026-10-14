//! Part 10.2: what the user does with the conversation's messages and turns — copy, edit and send
//! again, rewind the code to before a message, review or roll back a turn's changes (the "Done"
//! card) or one edit, stop a `!` command, open a subagent's conversation, preview a picture. The
//! transcript draws the buttons; the work is here.
//!
//! Rolling back is Flux's own: the model knows every edit of Claude with its file's text before it
//! (the result's `originalFile`) and works out the text after it ([`FileRestore`]); a file that
//! differs from that on disk changed since, and the question says so. Claude's edits only — what
//! its commands changed stays. The CLI has `rewind_files` too, but only for sessions that ran with
//! file checkpointing on from their start, which Flux's sessions until now didn't.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext, ClipboardItem, Context, Image, Window};

use flux_claude::{EntryId, EntryKind, FileRestore};

use crate::claude_chat::{ClaudeChat, ClaudeChatEvent};
use crate::dialog::Dialog;
use crate::diff_view::DiffSide;
use crate::i18n::{tr, trf, trn};
use crate::icons::IconName;
use crate::image_view::ImageSource;
use crate::notifications::Notification;

/// The question lists this many files; the rest are "and N more".
const LISTED_FILES: usize = 12;

impl ClaudeChat {
    /// A message of the user to the clipboard: its text, mentions as typed.
    pub fn copy_message(&mut self, id: EntryId, cx: &mut Context<Self>) {
        if let Some((text, _)) = self.user_message(id, cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// The message back in the field, to change and send again: the conversation continues from
    /// just before it (what came after it stays in the session's record, not in the chat). Where
    /// it goes back to comes from the CLI's record of the session; a message that isn't there
    /// can't be edited — a question says why.
    pub fn edit_message(&mut self, id: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((text, images)) = self.user_message(id, cx) else {
            return;
        };
        let point = self
            .session()
            .update(cx, |session, cx| session.edit_point(id, cx));
        cx.spawn_in(window, async move |chat, cx| {
            match point.await {
                Ok(point) => {
                    chat.update_in(cx, |chat, window, cx| {
                        let images = images
                            .into_iter()
                            .map(|(format, bytes)| Arc::new(Image::from_bytes(format, bytes)))
                            .collect();
                        chat.composer().update(cx, |composer, cx| {
                            composer.start_edit(point, &text, images, window, cx)
                        });
                    })
                    .ok();
                }
                Err(reason) => {
                    Dialog::warning(tr("This message can't be edited"))
                        .message(reason)
                        .primary(tr("OK"))
                        .show_async(cx)
                        .await;
                }
            }
        })
        .detach();
    }

    /// The files Claude changed after this message, back to what they were just before it (asks
    /// first; Claude's edits only — not what its commands did).
    pub fn rewind_code(&mut self, id: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        let files = self.session().read(cx).model().restores_after(id);
        self.restore(
            files,
            tr("Rewind the code to before this message?"),
            window,
            cx,
        );
    }

    /// The "Done" card's Review: the turn's files (or the one in `path`) against their text before
    /// the turn, each in a diff tab.
    pub fn review_turn(
        &mut self,
        turn_end: EntryId,
        path: Option<PathBuf>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files = self.session().read(cx).model().turn_restores(turn_end);
        for file in files
            .into_iter()
            .filter(|file| path.as_ref().is_none_or(|path| *path == file.path))
        {
            cx.emit(ClaudeChatEvent::OpenCompare {
                path: file.path,
                left: DiffSide::Text {
                    label: tr("Before this turn").to_string(),
                    text: file.before.map(Arc::from),
                },
                right: DiffSide::WorkingCopy,
            });
        }
    }

    /// The "Done" card's Rollback: the turn's changes undone — every file, or the one in `path`.
    pub fn rollback_turn(
        &mut self,
        turn_end: EntryId,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files: Vec<FileRestore> = self
            .session()
            .read(cx)
            .model()
            .turn_restores(turn_end)
            .into_iter()
            .filter(|file| path.as_ref().is_none_or(|path| *path == file.path))
            .collect();
        let question = match files.as_slice() {
            [file] => trf("Roll back Claude's changes to {0}?", &[&file_name(&file.path)]),
            _ => tr("Roll back the changes of this turn?").to_string(),
        };
        self.restore(files, question, window, cx);
    }

    /// An edit card's Diff: the file before and after this edit of Claude (the Edit / Write call
    /// `tool`), read-only.
    pub fn diff_edit(&mut self, tool: EntryId, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.session().read(cx).model().edit_restore(tool) else {
            return;
        };
        let right = match file.after {
            Some(after) => DiffSide::Text {
                label: tr("After the edit").to_string(),
                text: Some(Arc::from(after)),
            },
            // The result can't be worked out (a notebook): the file as it is now.
            None => DiffSide::WorkingCopy,
        };
        cx.emit(ClaudeChatEvent::OpenCompare {
            path: file.path,
            left: DiffSide::Text {
                label: tr("Before the edit").to_string(),
                text: file.before.map(Arc::from),
            },
            right,
        });
    }

    /// An edit card's Rollback: this edit undone. A file that changed after it gets the edit's own
    /// text replaced back, when that text is still there once; otherwise a question offers the
    /// diff.
    pub fn rollback_edit(&mut self, tool: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.session().read(cx).model();
        let Some(file) = model.edit_restore(tool) else {
            return;
        };
        let call = model.entries.iter().find_map(|entry| find_tool(entry, tool));
        let disk = std::fs::read_to_string(&file.path).ok();
        if disk == file.before {
            cx.emit(ClaudeChatEvent::Notify(Notification::info(tr(
                "The files are as they were before Claude's edits",
            ))));
            return;
        }
        let restored = if disk.is_some() && disk == file.after {
            Some(file.before.clone())
        } else {
            match (&disk, &call) {
                (Some(text), Some((name, input))) => {
                    flux_claude::edits::reverted_text(name, input, text).ok().map(Some)
                }
                _ => None,
            }
        };
        let name = file_name(&file.path);
        match restored {
            Some(text) => {
                let question = trf("Roll back this edit of {0}?", &[&name]);
                let file = FileRestore {
                    before: text,
                    // Checked above: what's on disk is what the edit left, or the edit is undone
                    // where it is.
                    after: disk,
                    ..file
                };
                self.restore(vec![file], question, window, cx);
            }
            None => {
                let answer = Dialog::warning(trf("Can't roll back this edit of {0}", &[&name]))
                    .message(tr(
                        "The file changed after the edit, and the edited text isn't in it as it was. Compare the edit with the file and undo it by hand.",
                    ))
                    .primary(tr("Show Diff"))
                    .cancel(tr("Cancel"))
                    .show(window, cx);
                cx.spawn_in(window, async move |chat, cx| {
                    if answer.await == Some(0) {
                        chat.update_in(cx, |chat, window, cx| chat.diff_edit(tool, window, cx))
                            .ok();
                    }
                })
                .detach();
            }
        }
    }

    /// Stops a running `!` command (the entry `id`).
    pub fn stop_shell(&mut self, id: EntryId, cx: &mut Context<Self>) {
        self.session()
            .update(cx, |session, cx| session.stop_shell(id, cx));
    }

    /// A subagent's conversation (the Agent call `tool`) on the chat's place, live; Esc — back.
    pub fn open_agent(&mut self, tool: EntryId, window: &mut Window, cx: &mut Context<Self>) {
        self.show_agent(tool, window, cx);
    }

    /// A picture of the conversation in an image tab.
    pub fn preview_image(&mut self, source: ImageSource, cx: &mut Context<Self>) {
        cx.emit(ClaudeChatEvent::PreviewImage(source));
    }

    /// The user's message `id`: its text and pictures.
    #[allow(clippy::type_complexity)]
    fn user_message(
        &self,
        id: EntryId,
        cx: &Context<Self>,
    ) -> Option<(String, Vec<(gpui::ImageFormat, Vec<u8>)>)> {
        self.session()
            .read(cx)
            .model()
            .entries
            .iter()
            .find_map(|entry| match &entry.kind {
                EntryKind::User(user) if entry.id == id => Some((
                    user.text.clone(),
                    user.images
                        .iter()
                        .map(|image| {
                            (
                                crate::claude_transcript::image_format(&image.media_type),
                                image.data.clone(),
                            )
                        })
                        .collect(),
                )),
                _ => None,
            })
    }

    /// Puts files back after a question (`question` — its title) that lists them and warns about
    /// the ones changed since Claude's last edit; files Claude created go to the Trash. Open
    /// documents follow, a notification says how it went.
    fn restore(
        &mut self,
        files: Vec<FileRestore>,
        question: impl Into<gpui::SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A file that is as it was before already (rolled back, a new file gone) has nothing to
        // lose and nothing to put back.
        let files: Vec<FileRestore> = files
            .into_iter()
            .filter(|file| std::fs::read_to_string(&file.path).ok() != file.before)
            .collect();
        if files.is_empty() {
            cx.emit(ClaudeChatEvent::Notify(Notification::info(tr(
                "The files are as they were before Claude's edits",
            ))));
            return;
        }
        let root = self.session().read(cx).model().info.cwd.clone();
        let mut list = String::new();
        for file in files.iter().take(LISTED_FILES) {
            list.push_str(&relative(&file.path, &root));
            if file.before.is_none() {
                list.push_str(&format!(" — {}", tr("to the Trash")));
            }
            list.push('\n');
        }
        if files.len() > LISTED_FILES {
            list.push_str(&trf("and {0} more", &[&(files.len() - LISTED_FILES)]));
        }
        let mut dialog = Dialog::warning(question)
            .message(tr(
                "The files go back as they were before Claude's edits. What Claude's commands changed stays.",
            ))
            .details(list.trim_end().to_string());
        for file in &files {
            let disk = std::fs::read_to_string(&file.path).ok();
            let changed = match (&disk, &file.after) {
                // The file Claude's edit created is gone already: nothing to lose.
                (None, _) => false,
                (Some(disk), Some(after)) => disk != after,
                (Some(_), None) => true,
            };
            if changed {
                dialog = dialog.point(
                    IconName::Warning,
                    trf(
                        "{0} changed after Claude's edit: those changes go too",
                        &[&file_name(&file.path)],
                    ),
                    true,
                );
            }
        }
        let answer = dialog
            .danger(tr("Rollback"))
            .cancel(tr("Cancel"))
            .show(window, cx);
        cx.spawn_in(window, async move |chat, cx| {
            if answer.await != Some(0) {
                return;
            }
            let jobs: Vec<(PathBuf, Option<Arc<str>>)> = files
                .iter()
                .map(|file| (file.path.clone(), file.before.as_deref().map(Arc::from)))
                .collect();
            let errors = cx
                .background_spawn(async move { crate::commit_panel::restore_originals(&jobs) })
                .await;
            chat.update(cx, |chat, cx| {
                let paths: Vec<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
                let notification = if errors.is_empty() {
                    Notification::success(match paths.as_slice() {
                        [path] => trf("Rolled back Claude's changes: {0}", &[&file_name(path)]),
                        _ => trf(
                            "Rolled back Claude's changes in {0}",
                            &[&trn(paths.len(), "{n} file", "{n} files")],
                        ),
                    })
                } else {
                    Notification::error(tr("Couldn't roll back Claude's changes"))
                        .body(errors.join("\n"))
                };
                chat.session()
                    .update(cx, |session, cx| session.files_restored(paths, cx));
                cx.emit(ClaudeChatEvent::Notify(notification));
            })
            .ok();
        })
        .detach();
    }
}

/// The name and input of the tool call `id` under `entry` (subagents' calls too).
fn find_tool(entry: &flux_claude::Entry, id: EntryId) -> Option<(String, serde_json::Value)> {
    let EntryKind::Tool(tool) = &entry.kind else {
        return None;
    };
    if entry.id == id {
        return Some((tool.name.clone(), tool.input.clone()));
    }
    tool.children.iter().find_map(|child| find_tool(child, id))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// A path for the question: from the project's root when it's inside.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// UI scenarios can't hover a message or click a card's buttons: the same actions on the last
/// message, turn and edit of the conversation (`action:claude_turns::EditLastMessage`).
#[cfg(feature = "scenario")]
mod scenario {
    gpui::actions!(
        claude_turns,
        [
            EditLastMessage,
            RewindLastMessage,
            ReviewLastTurn,
            RollbackLastTurn,
            DiffLastEdit,
            RollbackLastEdit
        ]
    );
}

#[cfg(feature = "scenario")]
pub(crate) fn scenario_actions(element: gpui::Div, cx: &mut Context<ClaudeChat>) -> gpui::Div {
    use gpui::InteractiveElement;
    use scenario::*;
    let user = |kind: &EntryKind| matches!(kind, EntryKind::User(_));
    let turn = |kind: &EntryKind| matches!(kind, EntryKind::TurnEnd(_));
    let edit = |kind: &EntryKind| {
        matches!(kind, EntryKind::Tool(tool) if flux_claude::edits::is_edit_tool(&tool.name))
    };
    element
        .on_action(cx.listener(move |chat, _: &EditLastMessage, window, cx| {
            if let Some(id) = chat.last_entry(user, cx) {
                chat.edit_message(id, window, cx)
            }
        }))
        .on_action(cx.listener(move |chat, _: &RewindLastMessage, window, cx| {
            if let Some(id) = chat.last_entry(user, cx) {
                chat.rewind_code(id, window, cx)
            }
        }))
        .on_action(cx.listener(move |chat, _: &ReviewLastTurn, window, cx| {
            if let Some(id) = chat.last_entry(turn, cx) {
                chat.review_turn(id, None, window, cx)
            }
        }))
        .on_action(cx.listener(move |chat, _: &RollbackLastTurn, window, cx| {
            if let Some(id) = chat.last_entry(turn, cx) {
                chat.rollback_turn(id, None, window, cx)
            }
        }))
        .on_action(cx.listener(move |chat, _: &DiffLastEdit, window, cx| {
            if let Some(id) = chat.last_entry(edit, cx) {
                chat.diff_edit(id, window, cx)
            }
        }))
        .on_action(cx.listener(move |chat, _: &RollbackLastEdit, window, cx| {
            if let Some(id) = chat.last_entry(edit, cx) {
                chat.rollback_edit(id, window, cx)
            }
        }))
}

#[cfg(not(feature = "scenario"))]
pub(crate) fn scenario_actions(element: gpui::Div, _cx: &mut Context<ClaudeChat>) -> gpui::Div {
    element
}

#[cfg(feature = "scenario")]
impl ClaudeChat {
    /// The last entry of the conversation that `pick` takes.
    fn last_entry(&self, pick: impl Fn(&EntryKind) -> bool, cx: &Context<Self>) -> Option<EntryId> {
        let model = self.session().read(cx).model();
        model
            .entries
            .iter()
            .rev()
            .find(|entry| pick(&entry.kind))
            .map(|entry| entry.id)
    }
}
