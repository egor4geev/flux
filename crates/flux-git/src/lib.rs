//! Git without UI, through the git command line (as JetBrains IDEs do): SSH, hooks, credential
//! helpers, signing and the user's config work as they do in a terminal.
//!
//! - [`cli`] — running git: the binary, the environment, errors, cancellation;
//! - [`repo`] — repositories of a project: the one containing the root and those nested in it;
//! - [`status`] — `git status --porcelain=v2`: changed files, the branch, its upstream;
//! - [`blob`] — file contents at a revision (HEAD) through one long-lived `git cat-file --batch`;
//! - [`diff`] — line and word diffs of two texts (no git involved);
//! - [`commit`] — commits of chosen files, whole or partly, without touching the user's staging;
//! - [`rollback`] — returning files to their HEAD state;
//! - [`push`] — remotes, outgoing commits, push with progress;
//! - [`ignore`] — adding paths to `.gitignore`;
//! - [`watch`] — changes in the working tree and in the git directory (HEAD, index, refs).
//!
//! Everything here blocks: call it from the background (gpui's `background_spawn`).

pub mod blob;
pub mod cli;
pub mod commit;
pub mod diff;
pub mod ignore;
pub mod push;
pub mod repo;
pub mod rollback;
pub mod status;
pub mod watch;

pub use blob::{BlobReader, is_binary};
pub use cli::{Cancel, GitError};
pub use commit::{
    CommitContent, CommitFile, CommitRequest, CommitResult, head_message, recent_messages,
};
pub use diff::{Hunk, HunkKind, apply_hunks, diff_lines, diff_words};
pub use ignore::add_to_gitignore;
pub use push::{
    CommitInfo, PushProgress, PushRequest, PushResult, Remote, commit_files, outgoing, remotes,
};
pub use repo::{Repo, find_repos, repo_for};
pub use rollback::{RollbackFile, rollback};
pub use status::{
    BranchState, FileStatus, RepoState, RepoStatus, StatusEntry, ignored_paths, status,
    untracked_dirs,
};
pub use watch::{RepoEvent, RepoWatcher};
