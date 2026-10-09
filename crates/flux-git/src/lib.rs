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
//! - [`branch`] — branches and tags: listing, checkout, new, rename, delete, merge, rebase, compare;
//! - [`sync`] — fetch, pull, Update Project, fast-forward of a branch that isn't checked out;
//! - [`stash`] — stashes: list, files, create, apply / pop, unstash into a branch, drop, clear;
//! - [`ops`] — the operation in progress (merge, rebase…), continue / skip / abort, local changes
//!   in the way;
//! - [`conflict`] — conflicted files: their three versions, taking a side, marking resolved;
//! - [`merge3`] — three-way merge of texts (the merge tool);
//! - [`ignore`] — adding paths to `.gitignore`;
//! - [`watch`] — changes in the working tree and in the git directory (HEAD, index, refs).
//!
//! Everything here blocks: call it from the background (gpui's `background_spawn`).

pub mod blob;
pub mod branch;
pub mod cli;
pub mod commit;
pub mod conflict;
pub mod diff;
pub mod ignore;
pub mod merge3;
pub mod ops;
pub mod push;
pub mod repo;
pub mod rollback;
pub mod stash;
pub mod status;
pub mod sync;
#[cfg(test)]
mod testing;
pub mod watch;

pub use blob::{BlobReader, is_binary};
pub use branch::{
    DeletedBranch, Ref, RefKind, Refs, check_branch_name, compare_commits, diff_changes,
    diff_files, is_not_fully_merged, unmerged_commits,
};
pub use cli::{Cancel, GitError};
pub use commit::{
    CommitContent, CommitFile, CommitRequest, CommitResult, head_message, recent_messages,
};
pub use conflict::{ConflictSide, ConflictVersions, conflict_versions};
pub use diff::{Hunk, HunkKind, apply_hunks, diff_lines, diff_words};
pub use ignore::add_to_gitignore;
pub use merge3::{Chunk, ChunkKind, initial_result, merge3, resolve_simple};
pub use ops::{Blocked, Operation, Outcome, blocked_by, operation};
pub use push::{
    CommitInfo, PushProgress, PushRequest, PushResult, Remote, commit_files, outgoing, remotes,
};
pub use repo::{Repo, find_repos, repo_for};
pub use rollback::{RollbackFile, rollback};
pub use stash::{Stash, StashRequest, stash_changes, stash_files, stashes};
pub use status::{
    BranchState, ConflictKind, FileChange, FileStatus, RepoState, RepoStatus, StatusEntry,
    ignored_paths, status, untracked_dirs,
};
pub use sync::{FetchResult, PullMode, UpdateMethod, UpdateResult};
pub use watch::{RepoEvent, RepoWatcher};
