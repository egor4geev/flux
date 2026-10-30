//! How the systems spell paths (part 10.9). macOS and Linux: `/` only, names compared as they are.
//! Windows: `\` (`/` is understood too), drive letters (`C:\`), shares (`\\server\share`), names
//! compared without case — `C:\Proj` and `c:\proj` are one folder — and `canonicalize` answers with
//! the `\\?\` prefix nobody else writes. The rules take the style as a value, so the Windows ones
//! are tested on any system; [`PathStyle::NATIVE`] is the one Flux runs on.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// How a system spells paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    /// macOS and Linux.
    Unix,
    Windows,
}

impl PathStyle {
    /// The style of the system Flux runs on.
    pub const NATIVE: PathStyle = if cfg!(windows) {
        PathStyle::Windows
    } else {
        PathStyle::Unix
    };

    /// The separator the system writes.
    pub fn separator(self) -> char {
        match self {
            PathStyle::Unix => '/',
            PathStyle::Windows => '\\',
        }
    }

    /// A character that separates names: `/`, on Windows `\` too.
    pub fn is_separator(self, c: char) -> bool {
        c == '/' || (self == PathStyle::Windows && c == '\\')
    }

    /// A full path: `/…`; on Windows `C:\…` (or `C:/…`), `\\server\share\…`, `\\?\…`.
    pub fn is_absolute(self, path: &str) -> bool {
        match self {
            PathStyle::Unix => path.starts_with('/'),
            PathStyle::Windows => {
                drive_letter(path).is_some_and(|_| path[2..].starts_with(|c| self.is_separator(c)))
                    || path.starts_with(r"\\")
                    || path.starts_with("//")
            }
        }
    }

    /// A `/`-separated path (git's, a project-relative one) as the system writes it: on Windows
    /// with `\`.
    pub fn native(self, path: &str) -> Cow<'_, str> {
        match self {
            PathStyle::Windows if path.contains('/') => Cow::Owned(path.replace('/', "\\")),
            _ => Cow::Borrowed(path),
        }
    }

    /// The other way: a path with `/` between names (what git and the `@` mentions of the Claude
    /// CLI take); on Unix the path itself.
    pub fn slashed(self, path: &str) -> Cow<'_, str> {
        match self {
            PathStyle::Windows if path.contains('\\') => Cow::Owned(path.replace('\\', "/")),
            _ => Cow::Borrowed(path),
        }
    }

    /// A path without the `\\?\` prefix of Windows' `canonicalize` (`\\?\C:\a` → `C:\a`,
    /// `\\?\UNC\server\share` → `\\server\share`) and with an upper-case drive letter, as Explorer
    /// and the file dialogs write it. Elsewhere the path itself.
    pub fn simplified(self, path: &str) -> Cow<'_, str> {
        if self == PathStyle::Unix {
            return Cow::Borrowed(path);
        }
        let path: Cow<str> = if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
            Cow::Owned(format!(r"\\{share}"))
        } else {
            match path.strip_prefix(r"\\?\") {
                Some(rest) if drive_letter(rest).is_some() => Cow::Borrowed(rest),
                _ => Cow::Borrowed(path),
            }
        };
        match drive_letter(&path) {
            Some(letter) if letter.is_ascii_lowercase() => {
                Cow::Owned(format!("{}{}", letter.to_ascii_uppercase(), &path[1..]))
            }
            _ => path,
        }
    }

    /// What two paths of this system are compared by: on Windows simplified, case-folded, with `\`
    /// only and no separator at the end (`C:\Proj\` and `c:/proj` are one folder); elsewhere the
    /// path itself.
    pub fn key(self, path: &str) -> Cow<'_, str> {
        match self {
            PathStyle::Unix => Cow::Borrowed(path),
            PathStyle::Windows => {
                let path = self.simplified(path).replace('/', "\\").to_lowercase();
                // A root keeps its separator: `C:\` is not `C:` (the current folder of drive C).
                let trimmed = path.trim_end_matches('\\');
                let root = trimmed.len() <= 2 && path.len() > trimmed.len();
                if root || trimmed.is_empty() {
                    Cow::Owned(path)
                } else {
                    Cow::Owned(trimmed.to_string())
                }
            }
        }
    }

    /// The two paths are one file's (or folder's) for this system.
    pub fn same(self, a: &str, b: &str) -> bool {
        a == b || (self == PathStyle::Windows && self.key(a) == self.key(b))
    }

    /// The path a `file://` URL names, from its host and its percent-decoded path; `None` when it
    /// isn't a file's. Windows: `/C:/a/x.rs`, `/c%3A/a/x.rs` (as some servers write it)
    /// and `/c|/a/x.rs` (old style) → `C:\a\x.rs`, the drive letter upper-case like Flux's own paths;
    /// a host or a path of four slashes is a share: `\\server\share\x.rs`. Elsewhere only this
    /// machine's files: no host but `localhost`.
    pub fn file_url_path(self, host: &str, path: &str) -> Option<String> {
        let local = host.is_empty() || host.eq_ignore_ascii_case("localhost");
        if !path.starts_with('/') {
            return None;
        }
        match self {
            PathStyle::Unix => local.then(|| path.to_string()),
            PathStyle::Windows if !local => Some(format!(r"\\{host}{}", path.replace('/', "\\"))),
            PathStyle::Windows => {
                if let Some(share) = path.strip_prefix("//") {
                    return Some(format!(r"\\{}", share.replace('/', "\\")));
                }
                let rest = &path[1..];
                let bytes = rest.as_bytes();
                let drive = bytes.len() >= 2
                    && bytes[0].is_ascii_alphabetic()
                    && matches!(bytes[1], b':' | b'|')
                    && (bytes.len() == 2 || bytes[2] == b'/');
                if !drive {
                    return Some(path.replace('/', "\\"));
                }
                let tail = &rest[2..];
                Some(format!(
                    "{}:{}",
                    char::from(bytes[0].to_ascii_uppercase()),
                    if tail.is_empty() {
                        "\\".to_string()
                    } else {
                        tail.replace('/', "\\")
                    }
                ))
            }
        }
    }

    /// `path` with the home folder shown as `~`: `~/dev/app`, on Windows `~\dev\app` (the home
    /// matched without case). Other paths as they are.
    pub fn tilde(self, path: &str, home: &str) -> String {
        if home.is_empty() {
            return path.to_string();
        }
        match self.strip_dir(path, home) {
            Some("") => "~".to_string(),
            Some(rest) => format!("~{}{}", self.separator(), self.native(rest)),
            None => path.to_string(),
        }
    }

    /// The part of `path` inside `dir` (`""` for the folder itself), names compared as the system
    /// does; `None` outside. The answer keeps `path`'s own spelling.
    pub fn strip_dir<'a>(self, path: &'a str, dir: &str) -> Option<&'a str> {
        let dir = dir.trim_end_matches(|c| self.is_separator(c));
        let head = path.get(..dir.len())?;
        let rest = &path[dir.len()..];
        let same = match self {
            PathStyle::Unix => head == dir,
            PathStyle::Windows => self.key(head) == self.key(dir),
        };
        if !same {
            return None;
        }
        if rest.is_empty() {
            Some("")
        } else if rest.starts_with(|c| self.is_separator(c)) {
            Some(rest.trim_start_matches(|c| self.is_separator(c)))
        } else {
            None
        }
    }
}

/// The drive letter a Windows path starts with (`C:…`).
fn drive_letter(path: &str) -> Option<char> {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| char::from(bytes[0]))
}

/// The two paths are one file's (or folder's) on this system: on Windows `C:\Proj` and `c:\proj`
/// are; on macOS they compare as they always did, exactly.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match PathStyle::NATIVE {
        PathStyle::Unix => a == b,
        style => a == b || style.same(&a.to_string_lossy(), &b.to_string_lossy()),
    }
}

/// `base` joined with a `/`-separated relative path (git's, a project-relative one) in the system's
/// separators: on Windows `C:\p` + `src/a.rs` is `C:\p\src\a.rs`, equal to the file tree's path of
/// the file (`C:\p\src/a.rs` would open the same file but compare as another).
pub fn join_slashed(base: &Path, relative: &str) -> PathBuf {
    base.join(PathStyle::NATIVE.native(relative).as_ref())
}

/// A path as Flux keeps it: on Windows [`PathStyle::simplified`]; elsewhere unchanged.
pub fn simplified(path: PathBuf) -> PathBuf {
    match path.to_str().map(|text| PathStyle::NATIVE.simplified(text)) {
        Some(Cow::Owned(text)) => PathBuf::from(text),
        _ => path,
    }
}

/// `std::fs::canonicalize` without the `\\?\` prefix on Windows: paths Flux keeps are shown to the
/// user and given to git, terminals and the CLIs it starts, which don't take it.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplified)
}

#[cfg(test)]
mod tests {
    use super::PathStyle::{Unix, Windows};

    #[test]
    fn absolute_paths() {
        assert!(Unix.is_absolute("/Users/me"));
        assert!(!Unix.is_absolute("src/main.rs"));
        assert!(!Unix.is_absolute(r"C:\Users"));
        for path in [
            r"C:\Users",
            "c:/Users",
            r"\\server\share\a",
            r"\\?\C:\a",
            "//server/s",
        ] {
            assert!(Windows.is_absolute(path), "{path}");
        }
        for path in [r"src\main.rs", "C:", "C:file.rs", "main.rs", ""] {
            assert!(!Windows.is_absolute(path), "{path}");
        }
    }

    #[test]
    fn separators_as_the_system_writes_them() {
        assert_eq!(Windows.native("src/ui/main.rs"), r"src\ui\main.rs");
        assert_eq!(Unix.native("src/ui/main.rs"), "src/ui/main.rs");
        assert_eq!(Windows.slashed(r"src\ui\main.rs"), "src/ui/main.rs");
        assert_eq!(Unix.slashed(r"odd\name"), r"odd\name");
    }

    #[test]
    fn canonical_prefixes_are_dropped() {
        assert_eq!(Windows.simplified(r"\\?\C:\Users\me"), r"C:\Users\me");
        assert_eq!(Windows.simplified(r"\\?\UNC\srv\share\a"), r"\\srv\share\a");
        assert_eq!(Windows.simplified(r"c:\proj"), r"C:\proj");
        assert_eq!(Windows.simplified(r"\\srv\share"), r"\\srv\share");
        // A verbatim path that isn't a drive's stays as it is.
        assert_eq!(Windows.simplified(r"\\?\Volume{1}\a"), r"\\?\Volume{1}\a");
        assert_eq!(Unix.simplified(r"\\?\C:\x"), r"\\?\C:\x");
    }

    #[test]
    fn windows_paths_compare_without_case() {
        assert!(Windows.same(r"C:\Proj", r"c:\proj"));
        assert!(Windows.same(r"C:\Proj\", "c:/proj"));
        assert!(Windows.same(r"\\?\C:\Proj", r"C:\PROJ"));
        assert!(Windows.same("Ёлка", "ёлка"));
        assert!(!Windows.same(r"C:\Proj", r"C:\Project"));
        assert!(!Windows.same(r"C:\", "C:"));
        assert!(Windows.same(r"C:\", "c:/"));
        // macOS keeps comparing exactly.
        assert!(!Unix.same("/Users/me/Proj", "/users/me/proj"));
        assert!(Unix.same("/Users/me", "/Users/me"));
    }

    #[test]
    fn the_home_folder_is_a_tilde() {
        assert_eq!(Unix.tilde("/Users/me/dev/app", "/Users/me"), "~/dev/app");
        assert_eq!(Unix.tilde("/Users/me", "/Users/me"), "~");
        assert_eq!(
            Unix.tilde("/Users/meme/app", "/Users/me"),
            "/Users/meme/app"
        );
        assert_eq!(
            Windows.tilde(r"c:\users\Me\dev\app", r"C:\Users\me"),
            r"~\dev\app"
        );
        assert_eq!(Windows.tilde(r"D:\app", r"C:\Users\me"), r"D:\app");
        assert_eq!(Windows.tilde(r"C:\app", ""), r"C:\app");
    }

    #[test]
    fn paths_inside_a_folder() {
        assert_eq!(Unix.strip_dir("/p/src/a.rs", "/p"), Some("src/a.rs"));
        assert_eq!(Unix.strip_dir("/p/src/a.rs", "/p/"), Some("src/a.rs"));
        assert_eq!(Unix.strip_dir("/p", "/p"), Some(""));
        assert_eq!(Unix.strip_dir("/pp/a.rs", "/p"), None);
        assert_eq!(Unix.strip_dir("/P/a.rs", "/p"), None);
        assert_eq!(Unix.strip_dir("/a.rs", "/"), Some("a.rs"));
        assert_eq!(Unix.strip_dir("a.rs", "/"), None);
        assert_eq!(
            Windows.strip_dir(r"c:\proj\src\a.rs", r"C:\Proj"),
            Some(r"src\a.rs")
        );
        assert_eq!(Windows.strip_dir("C:/Proj/a.rs", r"C:\Proj\"), Some("a.rs"));
        assert_eq!(Windows.strip_dir(r"C:\Project\a.rs", r"C:\Proj"), None);
        assert_eq!(Windows.strip_dir(r"C:\a.rs", r"C:\"), Some("a.rs"));
        assert_eq!(Windows.strip_dir(r"D:\a.rs", r"C:\"), None);
    }
}
