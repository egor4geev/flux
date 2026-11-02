//! Where plugins live on disk. `FLUX_PLUGINS_HOME` puts everything into one folder instead (tests,
//! UI scenarios): `plugins/`, `plugin-data/`, `cache/`, `logs/` and the secrets inside it.

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

/// Flux's support folder (`~/Library/Application Support/flux`, `%LOCALAPPDATA%\Flux`), or
/// `FLUX_PLUGINS_HOME`.
fn home() -> PathBuf {
    if let Some(home) = HOME.get() {
        return home.clone();
    }
    if let Some(home) = std::env::var_os("FLUX_PLUGINS_HOME") {
        return PathBuf::from(home);
    }
    flux_platform::paths::support_dir_in(&user_home())
}

fn user_home() -> PathBuf {
    flux_platform::paths::home().unwrap_or_else(std::env::temp_dir)
}

/// Everything is in a folder of `FLUX_PLUGINS_HOME` or of the process itself, not the user's.
pub(crate) fn overridden() -> bool {
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

/// The plugins' secrets (`crate::secrets`): `secrets.bin`, its key and its lock, in
/// `~/Library/Application Support/flux`.
pub(crate) fn secrets_dir() -> PathBuf {
    // A test never reaches the user's secrets, whichever test comes first.
    #[cfg(test)]
    crate::tests::scratch();
    home()
}

/// Compiled components: `~/Library/Caches/flux/plugins`.
pub fn cache_dir() -> PathBuf {
    if overridden() {
        return home().join("cache");
    }
    flux_platform::paths::caches_dir_in(&user_home()).join("plugins")
}

/// The plugins' logs: `~/Library/Logs/Flux/plugins/<id>.log`.
pub fn logs_dir() -> PathBuf {
    if overridden() {
        return home().join("logs");
    }
    flux_platform::paths::logs_dir_in(&user_home()).join("plugins")
}
