//! A plugin's work with Git (plugin API 0.3, part 10.3): the branches of a repository, a new branch
//! and a checkout the way the Branches popup does them (local changes come along; Smart Checkout
//! asks when they'd be in the way; Flux's own messages), the end going back as `git-done`; and the
//! plugin's commit message template — it fills an empty message in the commit window (and is
//! withdrawn when unedited, as `MERGE_MSG`), and a message generated with Claude starts with its
//! prefix. Permissions are checked by `flux-plugin`.
//!
//! The repository is the one whose work tree holds `root` (a path of `git.repositories`); the
//! branches are read anew for every call (the store reads its own lazily, for the popup).

use std::path::Path;
use std::sync::mpsc::Sender;

use flux_git::Refs;
use flux_plugin::api::events::Event;
use flux_plugin::api::git::{Branch, CommitTemplate};
use flux_plugin::runtime::WindowReply;
use futures::channel::oneshot;
use gpui::{AppContext as _, Context, Window};

use crate::branch_dialogs::{self, Done};
use crate::plugins::resolve;
use crate::workspace::Workspace;

/// `git.branches`: the branches of the repository whose work tree is `root` → `Branches`.
pub(crate) fn branches(
    workspace: &mut Workspace,
    root: &str,
    reply: Sender<WindowReply>,
    cx: &mut Context<Workspace>,
) {
    let read = match repo_at(workspace, root, cx) {
        Ok(repo) => read_refs(workspace, repo, cx),
        Err(err) => {
            reply.send(WindowReply::Branches(Err(err))).ok();
            return;
        }
    };
    cx.background_spawn(async move {
        let branches = read.await.map(|refs| branch_list(&refs));
        reply.send(WindowReply::Branches(branches)).ok();
    })
    .detach();
}

/// `git.new-branch`: creates `name` from `start` (none — HEAD) and checks it out; `git-done` says
/// how it ended.
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_branch(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    root: &str,
    name: &str,
    start: Option<&str>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let repo = match repo_at(workspace, root, cx) {
        Ok(repo) => repo,
        Err(err) => return git_done(workspace, plugin, id, Err(err), cx),
    };
    if let Err(reason) = flux_git::check_branch_name(name) {
        return git_done(workspace, plugin, id, Err(reason.to_string()), cx);
    }
    let done = waiting(workspace, plugin, id, cx);
    let read = read_refs(workspace, repo, cx);
    let (name, start) = (name.to_string(), start.map(str::to_string));
    cx.spawn_in(window, async move |this, cx| {
        let refs = read.await;
        this.update_in(cx, |this, window, cx| match refs {
            // A branch of that name is there already: the plugin checks it out instead.
            Ok(refs) if refs.local(&name).is_some() => {
                done.finish_now(Err(format!("Branch '{name}' already exists")))
            }
            _ => branch_dialogs::new_branch_for_plugin(
                this,
                repo,
                name,
                start,
                done.into_flow(),
                window,
                cx,
            ),
        })
        .ok();
    })
    .detach();
}

/// `git.checkout`: checks out a branch (a remote one as a new tracking branch); `git-done` says how
/// it ended.
pub(crate) fn checkout(
    workspace: &mut Workspace,
    plugin: &str,
    id: u64,
    root: &str,
    branch: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let repo = match repo_at(workspace, root, cx) {
        Ok(repo) => repo,
        Err(err) => return git_done(workspace, plugin, id, Err(err), cx),
    };
    let done = waiting(workspace, plugin, id, cx);
    let read = read_refs(workspace, repo, cx);
    let branch = branch.to_string();
    cx.spawn_in(window, async move |this, cx| {
        let refs = read.await;
        this.update_in(cx, |this, window, cx| match refs {
            Ok(refs) => branch_dialogs::checkout_for_plugin(
                this,
                repo,
                branch,
                &refs,
                done.into_flow(),
                window,
                cx,
            ),
            Err(err) => done.finish_now(Err(err)),
        })
        .ok();
    })
    .detach();
}

/// `git.set-commit-template`: the plugin's template for the commit window; none removes it.
pub(crate) fn set_commit_template(
    workspace: &mut Workspace,
    plugin: &str,
    template: Option<CommitTemplate>,
    cx: &mut Context<Workspace>,
) {
    let template = template
        .filter(|template| !template.text.trim().is_empty())
        .map(|template| (template.text, template.prefix));
    workspace
        .git()
        .update(cx, |git, cx| git.set_commit_template(plugin, template, cx));
}

/// The repository whose work tree holds `root` (relative — in the project).
fn repo_at(workspace: &Workspace, root: &str, cx: &Context<Workspace>) -> Result<usize, String> {
    let path = resolve(workspace.root(), root);
    workspace
        .git()
        .read(cx)
        .repo_index(Path::new(&path))
        .ok_or_else(|| format!("No Git repository at {root}"))
}

/// Reads a repository's branches in the background.
fn read_refs(
    workspace: &Workspace,
    repo: usize,
    cx: &mut Context<Workspace>,
) -> gpui::Task<Result<Refs, String>> {
    let git_repo = workspace
        .git()
        .read(cx)
        .repos()
        .get(repo)
        .map(|entry| entry.repo.clone());
    cx.background_spawn(async move {
        let git_repo = git_repo.ok_or_else(|| "The repository is gone".to_string())?;
        flux_git::branch::refs(&git_repo).map_err(|err| err.to_string())
    })
}

/// The branches as the API gives them: the local ones, then the remote ones.
fn branch_list(refs: &Refs) -> Vec<Branch> {
    let local = refs.local.iter().map(|branch| Branch {
        name: branch.name.clone(),
        remote: false,
        current: branch.current,
    });
    let remote = refs.remote.iter().map(|branch| Branch {
        name: branch.name.clone(),
        remote: true,
        current: false,
    });
    local.chain(remote).collect()
}

fn git_done(
    workspace: &Workspace,
    plugin: &str,
    id: u64,
    result: Result<(), String>,
    cx: &mut Context<Workspace>,
) {
    workspace
        .plugins
        .read(cx)
        .send_to(plugin, Event::GitDone((id, result)));
}

/// The plugin's wait for a flow: `git-done` goes to it when the flow ends — or, should the flow
/// end without a word, with that said.
struct Waiting {
    done: Done,
}

impl Waiting {
    /// The flow takes over the wait.
    fn into_flow(self) -> Done {
        self.done
    }

    /// The call ends before any flow starts.
    fn finish_now(self, result: Result<(), String>) {
        self.done.finish(result)
    }
}

fn waiting(workspace: &Workspace, plugin: &str, id: u64, cx: &mut Context<Workspace>) -> Waiting {
    let (sender, receiver) = oneshot::channel::<Result<(), String>>();
    let plugins = workspace.plugins.downgrade();
    let plugin = plugin.to_string();
    cx.spawn(async move |_, cx| {
        let result = receiver
            .await
            .unwrap_or_else(|_| Err("The operation ended without a result".into()));
        plugins
            .update(cx, |store, _| {
                store.send_to(&plugin, Event::GitDone((id, result)))
            })
            .ok();
    })
    .detach();
    Waiting {
        done: Done::new(sender),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_git::{Ref, RefKind};

    fn branch(kind: RefKind, name: &str, current: bool) -> Ref {
        Ref {
            kind,
            name: name.into(),
            remote: (kind == RefKind::Remote).then(|| "origin".into()),
            oid: String::new(),
            upstream: None,
            ahead: 0,
            behind: 0,
            upstream_gone: false,
            current,
            time: 0,
            subject: String::new(),
        }
    }

    #[test]
    fn branches_are_local_then_remote() {
        let refs = Refs {
            local: vec![
                branch(RefKind::Local, "feature/x", false),
                branch(RefKind::Local, "main", true),
            ],
            remote: vec![branch(RefKind::Remote, "origin/main", false)],
            ..Refs::default()
        };
        let list = branch_list(&refs);
        let names: Vec<(&str, bool, bool)> = list
            .iter()
            .map(|branch| (branch.name.as_str(), branch.remote, branch.current))
            .collect();
        assert_eq!(
            names,
            [
                ("feature/x", false, false),
                ("main", false, true),
                ("origin/main", true, false),
            ]
        );
    }
}
