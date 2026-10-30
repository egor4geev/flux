//! Where Flux keeps its files.
//!
//! |                  | macOS                                | Windows                     | Linux                    |
//! |------------------|--------------------------------------|-----------------------------|--------------------------|
//! | [`home`]         | `$HOME`                              | `%USERPROFILE%`             | `$HOME`                  |
//! | [`support_dir`]  | `~/Library/Application Support/flux` | `%LOCALAPPDATA%\Flux`       | `~/.local/share/flux`    |
//! | [`caches_dir`]   | `~/Library/Caches/flux`              | `%LOCALAPPDATA%\Flux\cache` | `~/.cache/flux`          |
//! | [`logs_dir`]     | `~/Library/Logs/Flux`                | `%LOCALAPPDATA%\Flux\logs`  | `~/.local/state/flux`    |
//!
//! The support folder holds settings, state, language servers and plugins. On Windows everything
//! is under Local, not Roaming AppData: servers and compiled plugins are programs for this machine,
//! and the plugins' secrets are sealed for it (DPAPI). Each folder is derived from the home folder
//! (`AppData\Local` is where Windows keeps `%LOCALAPPDATA%`), so code that takes the home folder as
//! an argument for its tests (`…_in`) and the real one agree.

use std::path::{Path, PathBuf};

/// The user's home folder.
pub fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Flux's own folder for settings, state, servers and plugins.
pub fn support_dir() -> Option<PathBuf> {
    home().map(|home| support_dir_in(&home))
}

/// The support folder of a user whose home is `home`.
pub fn support_dir_in(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/flux")
    } else if cfg!(windows) {
        home.join("AppData").join("Local").join("Flux")
    } else {
        home.join(".local/share/flux")
    }
}

/// Flux's caches: whatever it can download or build again.
pub fn caches_dir() -> Option<PathBuf> {
    home().map(|home| caches_dir_in(&home))
}

/// The caches folder of a user whose home is `home`.
pub fn caches_dir_in(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/flux")
    } else if cfg!(windows) {
        support_dir_in(home).join("cache")
    } else {
        home.join(".cache/flux")
    }
}

/// Flux's logs.
pub fn logs_dir() -> Option<PathBuf> {
    home().map(|home| logs_dir_in(&home))
}

/// The logs folder of a user whose home is `home`.
pub fn logs_dir_in(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Logs/Flux")
    } else if cfg!(windows) {
        support_dir_in(home).join("logs")
    } else {
        home.join(".local/state/flux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_inside_the_home_folder() {
        let home = Path::new("/home/me");
        for dir in [support_dir_in(home), caches_dir_in(home), logs_dir_in(home)] {
            assert!(dir.starts_with(home), "{}", dir.display());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_folders_are_the_ones_flux_always_used() {
        let home = Path::new("/Users/me");
        assert_eq!(
            support_dir_in(home),
            Path::new("/Users/me/Library/Application Support/flux")
        );
        assert_eq!(
            caches_dir_in(home),
            Path::new("/Users/me/Library/Caches/flux")
        );
        assert_eq!(logs_dir_in(home), Path::new("/Users/me/Library/Logs/Flux"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_folders_are_in_local_app_data() {
        let home = Path::new(r"C:\Users\me");
        assert_eq!(
            support_dir_in(home),
            Path::new(r"C:\Users\me\AppData\Local\Flux")
        );
        assert_eq!(
            caches_dir_in(home),
            Path::new(r"C:\Users\me\AppData\Local\Flux\cache")
        );
        assert_eq!(
            logs_dir_in(home),
            Path::new(r"C:\Users\me\AppData\Local\Flux\logs")
        );
    }
}
