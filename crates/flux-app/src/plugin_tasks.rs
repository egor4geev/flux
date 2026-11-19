//! A task's action run by another plugin (`tasks.run`, plugin API 0.6 — the bot's part 3): the
//! project's window for it. The action is the window's work — Git, the tabs, Claude's chat — done
//! by the source plugin's instance there, so `flux-plugin` asks for the window
//! ([`HostCall::ProjectWindow`](flux_plugin::runtime::HostCall)) before it sends the query: the one
//! that has the project, or one opened in the background (as a remote session's,
//! [`crate::claude_hub`]) — and only once it is ready for the action: its repositories found
//! (`git.repositories` is empty before) and Claude's check done (`claude.new-session` fails while
//! it runs). The reply is the window's id among the plugins' instances. The wait follows the
//! stores' changes; nothing polls.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use futures::channel::oneshot;
use gpui::{App, Entity};

use crate::claude::{ClaudeStore, CliState};
use crate::git::GitStore;
use crate::workspace::Workspace;

/// How long a window may take to be ready; then the action runs in it as it is.
const READY_WAIT: Duration = Duration::from_secs(15);

/// Replies the id of the window of `project` (its folder) — opened in the background when none
/// has it — once it is ready for a task's action, or why there is none.
pub(crate) fn project_window(project: &str, reply: Sender<Result<u64, String>>, cx: &mut App) {
    // As the windows name their projects (`/tmp` is `/private/tmp` on macOS).
    let root = flux_platform::path_style::canonicalize(Path::new(project))
        .unwrap_or_else(|_| PathBuf::from(project));
    if !root.is_dir() {
        let _ = reply.send(Err(format!("No project folder {project}")));
        return;
    }
    let window = crate::workspace::project_window(&root, cx)
        .or_else(|| crate::workspace::open_project_window_in_background(root, cx));
    let parts = window.and_then(|window| {
        window.read(cx).ok().map(|workspace: &Workspace| {
            (
                workspace.plugins.read(cx).window_id(),
                workspace.git().clone(),
                workspace.claude.clone(),
            )
        })
    });
    let Some((id, git, claude)) = parts else {
        let _ = reply.send(Err(format!("No window could open {project}")));
        return;
    };
    if ready(&git, &claude, cx) {
        let _ = reply.send(Ok(id));
        return;
    }
    // The stores tell when they change: the reply goes once both are ready, or after the wait.
    let (sender, receiver) = oneshot::channel::<()>();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let check = {
        let (git, claude) = (git.clone(), claude.clone());
        move |cx: &mut App| {
            if ready(&git, &claude, cx)
                && let Some(sender) = sender.borrow_mut().take()
            {
                sender.send(()).ok();
            }
        }
    };
    let subscriptions = [
        cx.observe(&git, {
            let check = check.clone();
            move |_, cx| check(cx)
        }),
        cx.observe(&claude, move |_, cx| check(cx)),
    ];
    cx.spawn(async move |cx| {
        let timer = cx.background_executor().timer(READY_WAIT);
        futures::future::select(receiver, timer).await;
        drop(subscriptions);
        let _ = reply.send(Ok(id));
    })
    .detach();
}

/// The window is ready for a task's action: its repositories found, Claude's check done.
fn ready(git: &Entity<GitStore>, claude: &Entity<ClaudeStore>, cx: &App) -> bool {
    !git.read(cx).is_discovering() && !matches!(claude.read(cx).cli(), CliState::Checking)
}
