//! Operations on project files. None of them overwrites an existing file or directory: a taken name
//! is an error (a copy gets a free name), and rename and move are atomic "no replace" operations.
//! They block; call them from the background.
//!
//! Errors are `io::Error` with a short message for the status bar (names are in “ ”).

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// The longest name in bytes (UTF-8): the limit of APFS and most file systems.
const MAX_NAME_BYTES: usize = 255;

/// Checks whether a file or directory can be given this name: not empty and not only spaces, not
/// `.`/`..`, no `/` or NUL, no longer than 255 bytes. `Err` is an explanation for the user. Spaces
/// at the edges are not forbidden; whoever accepted the input must trim them if needed.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Name cannot be empty".into());
    }
    if name == "." || name == ".." {
        return Err(format!("“{name}” is not a valid name"));
    }
    if name.contains('/') {
        return Err("Name cannot contain “/”".into());
    }
    if name.contains('\0') {
        return Err("Name cannot contain NUL".into());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err("Name is too long".into());
    }
    Ok(())
}

/// Creates an empty file `dir/name`. `name` may contain `/` (`src/new/mod.rs`); missing directories
/// are created. Returns the file's path.
pub fn create_file(dir: &Path, name: &str) -> io::Result<PathBuf> {
    if name.ends_with('/') {
        return Err(invalid("A file name cannot end with “/”"));
    }
    let path = nested_path(dir, name)?;
    create_parents(dir, &path)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| describe(err, &path))?;
    Ok(path)
}

/// Creates the directory `dir/name` (`name` may contain `/`). Returns its path.
pub fn create_dir(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let path = nested_path(dir, name)?;
    create_parents(dir, &path)?;
    fs::create_dir(&path).map_err(|err| describe(err, &path))?;
    Ok(path)
}

/// Renames within the same directory; `new_name` is only a name. A change of case alone
/// (`readme.md` → `README.md`) also works on a case-insensitive file system. Returns the new path.
pub fn rename(path: &Path, new_name: &str) -> io::Result<PathBuf> {
    validate_name(new_name).map_err(invalid)?;
    fs::symlink_metadata(path).map_err(|err| describe(err, path))?;
    let target = path.with_file_name(new_name);
    if target == path {
        return Ok(target);
    }
    match rename_no_replace(path, &target) {
        Err(err) if err.kind() == ErrorKind::AlreadyExists && is_case_change(path, &target) => {
            // On a case-insensitive file system, the new name is "taken" by the file itself.
            fs::rename(path, &target).map_err(|err| describe(err, path))?
        }
        result => result.map_err(|err| describe(err, &target))?,
    }
    Ok(target)
}

/// Moves `path` into the directory `dir` under the same name. Moving it into the directory it is
/// already in does nothing. Returns the new path.
pub fn move_into(path: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| invalid("Nothing to move"))?;
    fs::symlink_metadata(path).map_err(|err| describe(err, path))?;
    if path.parent().is_some_and(|parent| same_path(parent, dir)) {
        return Ok(path.to_path_buf());
    }
    if is_inside(dir, path) {
        return Err(invalid(format!(
            "Cannot move “{}” into itself",
            display_name(path)
        )));
    }
    if !fs::metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return Err(not_a_folder(dir));
    }
    let target = dir.join(name);
    rename_no_replace(path, &target).map_err(|err| match err.raw_os_error() {
        Some(code) if code == libc::EXDEV => io::Error::new(
            ErrorKind::CrossesDevices,
            format!("Cannot move “{}” to another volume", display_name(path)),
        ),
        _ => describe(err, &target),
    })?;
    Ok(target)
}

/// Copies `path` (a file, or a whole directory; symlinks are copied as symlinks) into the directory
/// `dir`. If the name is taken, a free one is chosen, as in Finder: "name copy.ext", "name copy
/// 2.ext". A duplicate in the same directory is made the same way. Returns the path of the copy.
pub fn copy_into(path: &Path, dir: &Path) -> io::Result<PathBuf> {
    let meta = fs::symlink_metadata(path).map_err(|err| describe(err, path))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Nothing to copy"))?
        .to_string_lossy()
        .into_owned();
    if meta.is_dir() && is_inside(dir, path) {
        return Err(invalid(format!("Cannot copy “{name}” into itself")));
    }
    if !fs::metadata(dir).is_ok_and(|meta| meta.is_dir()) {
        return Err(not_a_folder(dir));
    }
    let target = dir.join(free_name(dir, &name, meta.is_dir()));
    let copied = copy_entry(path, &target);
    if copied.is_err() {
        // An unfinished copy is ours and fresh, so it can be removed.
        let _ = remove_entry(&target);
    }
    copied.map_err(|err| describe(err, &target))?;
    Ok(target)
}

/// Moves to the Trash. On macOS this uses `NSFileManager trashItemAtURL`: no permission prompt for
/// controlling Finder and no sound (but "Put Back" in Finder may be unavailable; the file can be
/// dragged out of the Trash with the mouse). A symlink itself is trashed, not what it points to.
pub fn trash(paths: &[PathBuf]) -> io::Result<()> {
    let mut context = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        context.set_delete_method(DeleteMethod::NsFileManager);
    }
    for path in paths {
        fs::symlink_metadata(path).map_err(|err| describe(err, path))?;
        context.delete(path).map_err(|err| {
            let reason = match err {
                trash::Error::Unknown { description } => description,
                err => format!("{err:?}"),
            };
            io::Error::other(format!(
                "Cannot move “{}” to Trash: {reason}",
                display_name(path)
            ))
        })?;
    }
    Ok(())
}

/// The new `path` after renaming or moving `from` → `to`: `from` itself or something inside it;
/// otherwise `None`. The comparison is by components: `src2` is not inside `src`.
pub fn remap(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

// --- Paths and names ---

/// `dir/name`, where `name` is one or more components separated by `/` (empty ones, between `//`
/// and at the edges, are skipped); each one is run through [`validate_name`].
fn nested_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let parts: Vec<&str> = name.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return Err(invalid("Name cannot be empty"));
    }
    let mut path = dir.to_path_buf();
    for part in parts {
        validate_name(part).map_err(invalid)?;
        path.push(part);
    }
    Ok(path)
}

/// Creates the missing directories between `dir` and `path`; a file component on the path is an
/// error.
fn create_parents(dir: &Path, path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    for ancestor in parent.ancestors().take_while(|a| a.starts_with(dir)) {
        if fs::metadata(ancestor).is_ok_and(|meta| !meta.is_dir()) {
            return Err(not_a_folder(ancestor));
        }
    }
    fs::create_dir_all(parent).map_err(|err| describe(err, parent))
}

/// A free name for a copy of `name` in `dir`: `name` itself if it is not taken, otherwise "stem
/// copy.ext", "stem copy 2.ext"… A directory has no extension: "dir copy". A copy of a copy does
/// not grow: "x copy.rs" → "x copy 2.rs".
fn free_name(dir: &Path, name: &str, is_dir: bool) -> String {
    let occupied = |candidate: &str| fs::symlink_metadata(dir.join(candidate)).is_ok();
    if !occupied(name) {
        return name.to_string();
    }
    let (stem, extension) = split_extension(name, is_dir);
    let base = strip_copy_suffix(stem);
    (1..)
        .map(|n| {
            let copy = if n == 1 {
                format!("{base} copy")
            } else {
                format!("{base} copy {n}")
            };
            match extension {
                Some(extension) => format!("{copy}.{extension}"),
                None => copy,
            }
        })
        .find(|candidate| !occupied(candidate))
        .expect("an unused name exists")
}

/// The file name and the last extension (`archive.tar.gz` → `archive.tar`, `gz`); a hidden file
/// without a second dot (`.env`) and a directory have no extension.
fn split_extension(name: &str, is_dir: bool) -> (&str, Option<&str>) {
    if is_dir {
        return (name, None);
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => (&name[..dot], Some(&name[dot + 1..])),
        _ => (name, None),
    }
}

/// "x copy" and "x copy 3" → "x".
fn strip_copy_suffix(stem: &str) -> &str {
    if let Some(base) = stem.strip_suffix(" copy") {
        return base;
    }
    match stem.rsplit_once(" copy ") {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => stem,
    }
}

/// `inner` is `outer` or lies inside it (taking symlinks on the path into account).
fn is_inside(inner: &Path, outer: &Path) -> bool {
    if inner.starts_with(outer) {
        return true;
    }
    match (fs::canonicalize(inner), fs::canonicalize(outer)) {
        (Ok(inner), Ok(outer)) => inner.starts_with(outer),
        _ => false,
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The new name differs only in case, and the file system sees the same file under it.
fn is_case_change(from: &Path, to: &Path) -> bool {
    let (Some(a), Some(b)) = (from.file_name(), to.file_name()) else {
        return false;
    };
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase() && same_file(from, to)
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_: &Path, _: &Path) -> bool {
    false
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

// --- Rename without replacing ---

/// A `rename` that does not replace an existing `to` (a plain `rename(2)` would silently replace
/// the file): if it is taken, `AlreadyExists`.
#[cfg(target_os = "macos")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both strings are valid C strings that live until the end of the call.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    // SAFETY: both strings are valid C strings that live until the end of the call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        // The file system does not support `RENAME_NOREPLACE`: a check followed by a plain rename.
        Some(libc::EINVAL | libc::ENOSYS) => rename_checked(from, to),
        _ => Err(err),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    rename_checked(from, to)
}

/// The fallback path: a check, then rename (there is a race window between them).
#[cfg(not(target_os = "macos"))]
fn rename_checked(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::from(ErrorKind::AlreadyExists));
    }
    fs::rename(from, to)
}

#[cfg(unix)]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| invalid("Name cannot contain NUL"))
}

// --- Copying ---

/// Copies `from` to `to` (a free name). A file is copied without overwriting; a directory,
/// recursively; a symlink, as a symlink to the same target; sockets, FIFOs, and devices are skipped
/// (reading a FIFO would hang).
fn copy_entry(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        return copy_symlink(from, to);
    }
    if kind.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_entry(&entry.path(), &to.join(entry.file_name()))?;
        }
        return fs::set_permissions(to, meta.permissions());
    }
    if !kind.is_file() {
        return Ok(());
    }
    // We claim the name ourselves (`create_new`): `fs::copy` would silently overwrite someone
    // else's file.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    fs::copy(from, to).map(drop)
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(from)?, to)
}

#[cfg(not(unix))]
fn copy_symlink(_: &Path, _: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes the unfinished copy.
fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path)? {
        meta if meta.is_dir() => fs::remove_dir_all(path),
        _ => fs::remove_file(path),
    }
}

// --- Errors ---

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message.into())
}

fn not_a_folder(path: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::NotADirectory,
        format!("“{}” is not a folder", display_name(path)),
    )
}

/// An OS error with a clear message for common cases; `path` is what the error is about.
fn describe(err: io::Error, path: &Path) -> io::Error {
    let name = display_name(path);
    let message = match err.kind() {
        ErrorKind::AlreadyExists => format!("“{name}” already exists"),
        ErrorKind::NotFound => format!("“{name}” no longer exists"),
        ErrorKind::PermissionDenied => format!("Permission denied: “{name}”"),
        ErrorKind::NotADirectory => format!("“{name}” is not a folder"),
        _ => return err,
    };
    io::Error::new(err.kind(), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    /// Names in the directory, in byte order.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn message(err: io::Error) -> String {
        err.to_string()
    }

    #[test]
    fn names_are_validated() {
        assert!(validate_name("main.rs").is_ok());
        assert!(validate_name(".env").is_ok());
        assert!(validate_name("заметки.md").is_ok());
        assert!(validate_name(" padded ").is_ok());
        assert_eq!(validate_name("").unwrap_err(), "Name cannot be empty");
        assert_eq!(validate_name("   ").unwrap_err(), "Name cannot be empty");
        assert_eq!(validate_name(".").unwrap_err(), "“.” is not a valid name");
        assert_eq!(validate_name("..").unwrap_err(), "“..” is not a valid name");
        assert_eq!(validate_name("a/b").unwrap_err(), "Name cannot contain “/”");
        assert_eq!(
            validate_name("a\0b").unwrap_err(),
            "Name cannot contain NUL"
        );
        assert!(validate_name(&"ы".repeat(127)).is_ok());
        assert_eq!(
            validate_name(&"ы".repeat(128)).unwrap_err(),
            "Name is too long"
        );
    }

    #[test]
    fn files_and_folders_are_created_with_missing_parents() {
        let dir = temp();
        let root = dir.path();
        let file = create_file(root, "main.rs").unwrap();
        assert_eq!(file, root.join("main.rs"));
        assert!(file.is_file());
        let nested = create_file(root, "src/new/mod.rs").unwrap();
        assert_eq!(nested, root.join("src/new/mod.rs"));
        assert!(nested.is_file());
        let folder = create_dir(root, "docs/img").unwrap();
        assert!(folder.is_dir());
        // Extra `/` do no harm.
        assert_eq!(
            create_dir(root, "/a//b/").unwrap(),
            root.join("a").join("b")
        );
    }

    #[test]
    fn creating_never_overwrites() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("main.rs"), "fn main() {}");
        let err = create_file(root, "main.rs").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(message(err), "“main.rs” already exists");
        assert_eq!(read(&root.join("main.rs")), "fn main() {}");
        assert_eq!(
            message(create_dir(root, "main.rs").unwrap_err()),
            "“main.rs” already exists"
        );
        fs::create_dir(root.join("src")).unwrap();
        assert_eq!(
            message(create_dir(root, "src").unwrap_err()),
            "“src” already exists"
        );
        assert_eq!(
            message(create_file(root, "src").unwrap_err()),
            "“src” already exists"
        );
    }

    #[test]
    fn bad_names_and_file_parents_are_errors() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("notes.txt"), "");
        assert_eq!(
            message(create_file(root, "notes.txt/x.rs").unwrap_err()),
            "“notes.txt” is not a folder"
        );
        assert_eq!(
            message(create_dir(root, "notes.txt/sub").unwrap_err()),
            "“notes.txt” is not a folder"
        );
        assert_eq!(
            message(create_file(root, "../escape.rs").unwrap_err()),
            "“..” is not a valid name"
        );
        assert_eq!(
            message(create_file(root, "").unwrap_err()),
            "Name cannot be empty"
        );
        assert_eq!(
            message(create_file(root, "dir/").unwrap_err()),
            "A file name cannot end with “/”"
        );
        assert_eq!(names(root), ["notes.txt"]);
    }

    #[test]
    fn rename_never_overwrites() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("a.rs"), "a");
        write(&root.join("b.rs"), "b");
        let err = rename(&root.join("a.rs"), "b.rs").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(message(err), "“b.rs” already exists");
        assert_eq!(read(&root.join("a.rs")), "a");
        assert_eq!(read(&root.join("b.rs")), "b");

        let renamed = rename(&root.join("a.rs"), "c.rs").unwrap();
        assert_eq!(renamed, root.join("c.rs"));
        assert_eq!(names(root), ["b.rs", "c.rs"]);
        // The same name: do nothing.
        assert_eq!(rename(&renamed, "c.rs").unwrap(), renamed);
        assert_eq!(
            message(rename(&renamed, "x/y.rs").unwrap_err()),
            "Name cannot contain “/”"
        );
        assert_eq!(
            message(rename(&root.join("gone.rs"), "new.rs").unwrap_err()),
            "“gone.rs” no longer exists"
        );
    }

    #[test]
    fn rename_can_change_only_the_case() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("readme.md"), "text");
        let renamed = rename(&root.join("readme.md"), "README.md").unwrap();
        assert_eq!(renamed, root.join("README.md"));
        assert_eq!(names(root), ["README.md"]);
        assert_eq!(read(&renamed), "text");
        // A directory too.
        fs::create_dir(root.join("src")).unwrap();
        rename(&root.join("src"), "Src").unwrap();
        assert_eq!(names(root), ["README.md", "Src"]);
    }

    #[test]
    fn rename_keeps_a_case_twin_on_a_case_sensitive_fs() {
        // Two different files that differ only in case (possible on a case-sensitive file system):
        // renaming to the "twin" is an error, not a replacement.
        let dir = temp();
        let root = dir.path();
        write(&root.join("a.txt"), "small");
        write(&root.join("B.txt"), "big");
        if root.join("b.txt").exists() {
            // A case-insensitive file system: such a twin cannot be created, so there is nothing to
            // check.
            let err = rename(&root.join("a.txt"), "b.txt").unwrap_err();
            assert_eq!(err.kind(), ErrorKind::AlreadyExists);
            assert_eq!(read(&root.join("B.txt")), "big");
            return;
        }
        write(&root.join("b.txt"), "twin");
        assert!(rename(&root.join("B.txt"), "b.txt").is_err());
        assert_eq!(read(&root.join("b.txt")), "twin");
    }

    #[test]
    fn move_into_a_folder() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("src/lib.rs"), "lib");
        write(&root.join("docs/lib.rs"), "other");
        fs::create_dir(root.join("empty")).unwrap();

        let moved = move_into(&root.join("src/lib.rs"), &root.join("empty")).unwrap();
        assert_eq!(moved, root.join("empty/lib.rs"));
        assert_eq!(read(&moved), "lib");
        assert!(names(&root.join("src")).is_empty());

        // If it is taken, that is an error, and both files are intact.
        let err = move_into(&moved, &root.join("docs")).unwrap_err();
        assert_eq!(message(err), "“lib.rs” already exists");
        assert_eq!(read(&root.join("docs/lib.rs")), "other");
        assert_eq!(read(&moved), "lib");

        // Into the directory it is already in: do nothing.
        assert_eq!(move_into(&moved, &root.join("empty")).unwrap(), moved);

        // A whole directory, with its contents.
        let moved_dir = move_into(&root.join("docs"), &root.join("src")).unwrap();
        assert_eq!(moved_dir, root.join("src/docs"));
        assert_eq!(read(&root.join("src/docs/lib.rs")), "other");
    }

    #[test]
    fn folder_cannot_move_into_itself() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("a/b/file.txt"), "");
        let a = root.join("a");
        assert_eq!(
            message(move_into(&a, &a).unwrap_err()),
            "Cannot move “a” into itself"
        );
        assert_eq!(
            message(move_into(&a, &a.join("b")).unwrap_err()),
            "Cannot move “a” into itself"
        );
        assert!(root.join("a/b/file.txt").exists());
        // The neighboring `a2` is not inside `a`.
        fs::create_dir(root.join("a2")).unwrap();
        assert_eq!(move_into(&a, &root.join("a2")).unwrap(), root.join("a2/a"));
    }

    #[test]
    fn move_into_a_file_is_an_error() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("a.txt"), "a");
        write(&root.join("b.txt"), "b");
        assert_eq!(
            message(move_into(&root.join("a.txt"), &root.join("b.txt")).unwrap_err()),
            "“b.txt” is not a folder"
        );
        assert_eq!(
            message(move_into(&root.join("a.txt"), &root.join("nope")).unwrap_err()),
            "“nope” is not a folder"
        );
        assert_eq!(read(&root.join("a.txt")), "a");
        assert_eq!(read(&root.join("b.txt")), "b");
    }

    #[test]
    fn copies_get_free_names_like_in_finder() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("main.rs"), "fn main() {}");
        write(&root.join(".env"), "KEY=1");
        write(&root.join("Makefile"), "all:");
        write(&root.join("archive.tar.gz"), "");
        fs::create_dir(root.join("v1.2")).unwrap();

        let copy = copy_into(&root.join("main.rs"), root).unwrap();
        assert_eq!(copy, root.join("main copy.rs"));
        assert_eq!(read(&copy), "fn main() {}");
        assert_eq!(
            copy_into(&root.join("main.rs"), root).unwrap(),
            root.join("main copy 2.rs")
        );
        // A copy of a copy does not grow: "main copy 3.rs", not "main copy copy.rs".
        assert_eq!(copy_into(&copy, root).unwrap(), root.join("main copy 3.rs"));
        assert_eq!(
            copy_into(&root.join(".env"), root).unwrap(),
            root.join(".env copy")
        );
        assert_eq!(
            copy_into(&root.join("Makefile"), root).unwrap(),
            root.join("Makefile copy")
        );
        assert_eq!(
            copy_into(&root.join("archive.tar.gz"), root).unwrap(),
            root.join("archive.tar copy.gz")
        );
        assert_eq!(
            copy_into(&root.join("v1.2"), root).unwrap(),
            root.join("v1.2 copy")
        );
    }

    #[test]
    fn copy_to_another_folder_keeps_the_name() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("src/lib.rs"), "lib");
        fs::create_dir(root.join("dst")).unwrap();
        let copy = copy_into(&root.join("src/lib.rs"), &root.join("dst")).unwrap();
        assert_eq!(copy, root.join("dst/lib.rs"));
        assert_eq!(read(&copy), "lib");
        assert_eq!(read(&root.join("src/lib.rs")), "lib");
        assert!(copy_into(&root.join("src/lib.rs"), &root.join("src/lib.rs")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn folders_are_copied_recursively_with_symlinks() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("src/a.rs"), "a");
        write(&root.join("src/deep/b.rs"), "b");
        std::os::unix::fs::symlink("a.rs", root.join("src/link.rs")).unwrap();
        let copy = copy_into(&root.join("src"), root).unwrap();
        assert_eq!(copy, root.join("src copy"));
        assert_eq!(read(&copy.join("a.rs")), "a");
        assert_eq!(read(&copy.join("deep/b.rs")), "b");
        let link = copy.join("link.rs");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("a.rs"));
        assert_eq!(names(&copy), ["a.rs", "deep", "link.rs"]);
    }

    #[test]
    fn folder_cannot_be_copied_into_itself() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("src/deep/b.rs"), "b");
        let src = root.join("src");
        assert_eq!(
            message(copy_into(&src, &src.join("deep")).unwrap_err()),
            "Cannot copy “src” into itself"
        );
        // But copying it "alongside" itself makes a duplicate.
        assert_eq!(copy_into(&src, root).unwrap(), root.join("src copy"));
        assert_eq!(names(&src), ["deep"]);
    }

    #[test]
    fn copy_suffixes_are_recognized() {
        assert_eq!(strip_copy_suffix("main copy"), "main");
        assert_eq!(strip_copy_suffix("main copy 12"), "main");
        assert_eq!(strip_copy_suffix("main copy x"), "main copy x");
        assert_eq!(strip_copy_suffix("main"), "main");
        assert_eq!(split_extension("main.rs", false), ("main", Some("rs")));
        assert_eq!(split_extension(".env", false), (".env", None));
        assert_eq!(split_extension("trailing.", false), ("trailing.", None));
        assert_eq!(split_extension("v1.2", true), ("v1.2", None));
    }

    #[test]
    fn paths_are_remapped_after_moves() {
        let from = Path::new("/p/src");
        let to = Path::new("/p/lib");
        assert_eq!(remap(from, from, to), Some(to.to_path_buf()));
        assert_eq!(
            remap(Path::new("/p/src/a/b.rs"), from, to),
            Some(PathBuf::from("/p/lib/a/b.rs"))
        );
        assert_eq!(remap(Path::new("/p/src2/a.rs"), from, to), None);
        assert_eq!(remap(Path::new("/p/other.rs"), from, to), None);
        assert_eq!(remap(Path::new("/p"), from, to), None);
    }

    #[test]
    fn missing_paths_are_reported_by_name() {
        let dir = temp();
        let root = dir.path();
        let gone = root.join("gone.rs");
        assert_eq!(
            message(trash(std::slice::from_ref(&gone)).unwrap_err()),
            "“gone.rs” no longer exists"
        );
        assert_eq!(
            message(copy_into(&gone, root).unwrap_err()),
            "“gone.rs” no longer exists"
        );
        assert_eq!(
            message(move_into(&gone, &root.join("x")).unwrap_err()),
            "“gone.rs” no longer exists"
        );
    }

    /// The author's real Trash: run manually (`cargo test -p flux-fs -- --ignored`).
    #[test]
    #[ignore]
    fn trash_moves_files_and_folders_to_the_trash() {
        let dir = temp();
        let root = dir.path();
        write(&root.join("flux-trash-test.txt"), "x");
        write(&root.join("flux-trash-dir/inner.txt"), "y");
        trash(&[
            root.join("flux-trash-test.txt"),
            root.join("flux-trash-dir"),
        ])
        .unwrap();
        assert!(names(root).is_empty());
    }
}
