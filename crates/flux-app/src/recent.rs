//! Recent projects: directories, most recent first. Stored between launches in the file
//! `~/Library/Application Support/flux/recent-projects` (one path per line).
//!
//! The file path can be overridden with `FLUX_RECENT_FILE`. In verification scenarios
//! (`FLUX_SCENARIO`), without it the list is neither read nor written: agent runs don't end up in
//! the author's list. I/O errors don't interfere with operation: the list simply won't be saved (a
//! message goes to stderr).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How many projects to remember.
pub const MAX_RECENT: usize = 10;

/// Recent projects, most recent first; directories that have disappeared are skipped.
pub fn load() -> Vec<PathBuf> {
    store_path().map_or_else(Vec::new, |file| load_from(&file))
}

/// Remembers the project as the first in the list; returns the new list (directories that have
/// disappeared are skipped in it but remain in the file: the volume may simply not be connected).
pub fn record(root: &Path) -> Vec<PathBuf> {
    match store_path() {
        Some(file) => record_in(&file, root),
        None => vec![root.to_path_buf()],
    }
}

fn store_path() -> Option<PathBuf> {
    store_path_for(
        std::env::var_os("FLUX_RECENT_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    )
}

/// Where the list is stored: an explicit file, otherwise (outside a scenario) Application Support.
fn store_path_for(
    recent_file: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = recent_file.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    let home = PathBuf::from(home?);
    Some(home.join("Library/Application Support/flux/recent-projects"))
}

fn load_from(file: &Path) -> Vec<PathBuf> {
    read(file).into_iter().filter(|dir| dir.is_dir()).collect()
}

fn record_in(file: &Path, root: &Path) -> Vec<PathBuf> {
    let list = push_front(read(file), root);
    if let Err(error) = write(file, &list) {
        eprintln!("flux: {}: {error}", file.display());
    }
    list.into_iter()
        .filter(|dir| dir == root || dir.is_dir())
        .collect()
}

fn read(file: &Path) -> Vec<PathBuf> {
    match fs::read_to_string(file) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            eprintln!("flux: {}: {error}", file.display());
            Vec::new()
        }
    }
}

/// File → list: one path per line; empty lines, relative paths, and duplicates are skipped, and
/// anything beyond [`MAX_RECENT`] is dropped.
fn parse(text: &str) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let path = Path::new(line.trim_end_matches('\r'));
        if path.is_absolute() && !list.iter().any(|known| known == path) {
            list.push(path.to_path_buf());
        }
    }
    list.truncate(MAX_RECENT);
    list
}

/// `root` goes first; its previous place is freed; the length is at most [`MAX_RECENT`].
fn push_front(mut list: Vec<PathBuf>, root: &Path) -> Vec<PathBuf> {
    list.retain(|known| known != root);
    list.insert(0, root.to_path_buf());
    list.truncate(MAX_RECENT);
    list
}

/// List → file text. Paths that can't be written as a line (non-UTF-8, containing a line break) are
/// skipped; otherwise reading would produce a different path.
fn serialize(list: &[PathBuf]) -> String {
    list.iter()
        .filter_map(|path| path.to_str())
        .filter(|path| !path.contains(['\n', '\r']))
        .map(|path| format!("{path}\n"))
        .collect()
}

/// Writes through a temporary file next to the target and a `rename`: the file is never left
/// half-written. The temporary file's name includes the process ID: two flux windows don't write to
/// the same temporary file.
fn write(file: &Path, list: &[PathBuf]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = file.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    fs::write(&temp, serialize(list))?;
    fs::rename(&temp, file).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    /// Each test has its own temporary directory: tests run in parallel.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("flux-recent-test-{}-{name}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_skips_blanks_relative_paths_and_repeats() {
        let text = "/a/one\n\nrelative/two\n/a/one\r\n/b/three\n";
        assert_eq!(parse(text), paths(&["/a/one", "/b/three"]));
        let many: String = (0..MAX_RECENT + 5).map(|n| format!("/p/{n}\n")).collect();
        assert_eq!(parse(&many).len(), MAX_RECENT);
    }

    #[test]
    fn recorded_project_moves_to_the_front() {
        let list = paths(&["/a", "/b", "/c"]);
        assert_eq!(
            push_front(list.clone(), Path::new("/c")),
            paths(&["/c", "/a", "/b"])
        );
        assert_eq!(
            push_front(list, Path::new("/new")),
            paths(&["/new", "/a", "/b", "/c"])
        );
        let full: Vec<PathBuf> = (0..MAX_RECENT).map(|n| format!("/p/{n}").into()).collect();
        let list = push_front(full, Path::new("/newest"));
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], Path::new("/newest"));
        assert_eq!(
            list.last().unwrap(),
            Path::new(&format!("/p/{}", MAX_RECENT - 2))
        );
    }

    #[test]
    fn unwritable_paths_are_left_out() {
        let list = paths(&["/a", "/with\nnewline", "/b"]);
        assert_eq!(serialize(&list), "/a\n/b\n");
    }

    #[test]
    fn store_path_honours_override_and_scenarios() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            store_path_for(None, false, home.clone()),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/recent-projects"
            ))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(Some("/tmp/r".into()), true, home.clone()),
            Some(PathBuf::from("/tmp/r"))
        );
        assert_eq!(
            store_path_for(Some(OsString::new()), false, home),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/recent-projects"
            ))
        );
        assert_eq!(store_path_for(None, false, None), None);
    }

    #[test]
    fn record_writes_the_file_and_load_reads_existing_dirs() {
        let dir = temp_dir("roundtrip");
        let (one, two) = (dir.join("one"), dir.join("two"));
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        // The storage directory is created on the first write.
        let file = dir.join("store/recent-projects");

        assert_eq!(record_in(&file, &one), vec![one.clone()]);
        assert_eq!(record_in(&file, &two), vec![two.clone(), one.clone()]);
        assert_eq!(record_in(&file, &one), vec![one.clone(), two.clone()]);
        assert_eq!(load_from(&file), vec![one.clone(), two.clone()]);

        // A directory that has disappeared is not shown but stays in the file.
        fs::remove_dir_all(&two).unwrap();
        assert_eq!(load_from(&file), vec![one.clone()]);
        assert_eq!(read(&file), vec![one.clone(), two.clone()]);
        // No temporary files are left.
        let leftovers = fs::read_dir(dir.join("store")).unwrap().count();
        assert_eq!(leftovers, 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_empty_list() {
        let dir = temp_dir("missing");
        assert!(load_from(&dir.join("nothing")).is_empty());
        fs::remove_dir_all(&dir).ok();
    }
}
