//! Where plugins live on disk. `FLUX_PLUGINS_HOME` puts everything into one folder instead (tests,
//! UI scenarios): `plugins/`, `plugin-data/`, `cache/`, `logs/` inside it.

use std::path::PathBuf;
use std::sync::OnceLock;

/// A folder for everything set by the process itself (tests): wins over `FLUX_PLUGINS_HOME`.
static HOME: OnceLock<PathBuf> = OnceLock::new();

/// Puts everything into `dir` for the rest of the process, as `FLUX_PLUGINS_HOME` does, without
/// touching the environment; the first call wins. For tests.
#[doc(hidden)]
pub fn override_home(dir: PathBuf) {
    let _ = HOME.set(dir);
}

/// `~/Library/Application Support/flux`, or `FLUX_PLUGINS_HOME`.
fn home() -> PathBuf {
    if let Some(home) = HOME.get() {
        return home.clone();
    }
    if let Some(home) = std::env::var_os("FLUX_PLUGINS_HOME") {
        return PathBuf::from(home);
    }
    user_home().join("Library/Application Support/flux")
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn overridden() -> bool {
    HOME.get().is_some() || std::env::var_os("FLUX_PLUGINS_HOME").is_some()
}

/// Installed plugins, a folder each: `plugins/<id>/`.
pub fn plugins_dir() -> PathBuf {
    home().join("plugins")
}

/// The plugin's own folder (storage, its files): `plugin-data/<id>/`.
pub fn data_dir(id: &str) -> PathBuf {
    home().join("plugin-data").join(id)
}

/// Compiled components: `~/Library/Caches/flux/plugins`.
pub fn cache_dir() -> PathBuf {
    if overridden() {
        return home().join("cache");
    }
    user_home().join("Library/Caches/flux/plugins")
}

/// The plugins' logs: `~/Library/Logs/Flux/plugins/<id>.log`.
pub fn logs_dir() -> PathBuf {
    if overridden() {
        return home().join("logs");
    }
    user_home().join("Library/Logs/Flux/plugins")
}
