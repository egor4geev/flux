//! "Add to .gitignore": a path of the working tree goes into the `.gitignore` at the top of the
//! working tree, anchored (`/target/`), so that it doesn't match the same name elsewhere.

use std::fs;
use std::io;
use std::path::Path;

use crate::repo::Repo;

/// Appends `path` (relative, `/`) to `.gitignore`, creating it; a directory gets a trailing `/`. A
/// line that is already there isn't added twice.
pub fn add_to_gitignore(repo: &Repo, path: &str, is_dir: bool) -> io::Result<()> {
    let file = repo.work_dir.join(".gitignore");
    let mut pattern = format!("/{}", escape(path.trim_matches('/')));
    if is_dir {
        pattern.push('/');
    }
    let existing = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    if existing.lines().any(|line| line.trim() == pattern) {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&pattern);
    text.push('\n');
    write_atomically(&file, &text)
}

/// gitignore patterns treat `*`, `?`, `[`, `\`, a leading `#` or `!` and trailing spaces specially:
/// a literal path escapes them.
fn escape(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | '\\' | '#' | '!') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    if escaped.ends_with(' ') {
        let trimmed = escaped.trim_end_matches(' ').len();
        let spaces = escaped.len() - trimmed;
        escaped.truncate(trimmed);
        escaped.push_str(&"\\ ".repeat(spaces));
    }
    escaped
}

fn write_atomically(file: &Path, text: &str) -> io::Result<()> {
    let tmp = file.with_extension("flux-tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::tests::init_repo;

    #[test]
    fn paths_are_appended_once_and_anchored() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let repo = init_repo(&root);
        fs::write(root.join(".gitignore"), "*.log").unwrap();
        add_to_gitignore(&repo, "target", true).unwrap();
        add_to_gitignore(&repo, "notes/draft #1.md", false).unwrap();
        add_to_gitignore(&repo, "target", true).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).unwrap(),
            "*.log\n/target/\n/notes/draft \\#1.md\n"
        );
    }
}
