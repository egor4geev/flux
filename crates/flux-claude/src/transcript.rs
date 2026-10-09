//! Saved sessions of a project: `~/.claude/projects/<slug>/<session id>.jsonl`, where the slug is
//! the absolute project path with every character other than a letter or a digit replaced by `-`
//! (part 9.2: the session history and Resume).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A saved session.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedSession {
    pub id: String,
    /// The CLI's title (`ai-title`), otherwise the first prompt.
    pub title: String,
    pub modified: SystemTime,
    pub path: PathBuf,
}

/// The folder of a project's transcripts under `projects` (`claude auth status` →
/// `projectsDirectory`, by default `~/.claude/projects`).
pub fn project_dir(projects: &Path, root: &Path) -> PathBuf {
    let slug: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    projects.join(slug)
}

/// The project's saved sessions, the most recent first (part 9.2).
pub fn list(_projects: &Path, _root: &Path) -> Vec<SavedSession> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slug_replaces_everything_but_letters_and_digits() {
        assert_eq!(
            project_dir(
                Path::new("/h/.claude/projects"),
                Path::new("/Users/me/dev/my_app")
            ),
            PathBuf::from("/h/.claude/projects/-Users-me-dev-my-app")
        );
    }
}
