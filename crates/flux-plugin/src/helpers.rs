//! The plugins' own programs (API 0.5, the manifest's `[[helpers]]`): `process.helper(name)` gives
//! the path of the file of this computer's platform. Flux downloads it from the manifest's address
//! (the user approved the manifest: no `network` permission is needed), checks the SHA-256 of the
//! downloaded bytes, unpacks a `.gz` and keeps it apart from the plugin's folder (an update
//! replaces that folder): `helpers/<plugin id>/<sha256>/<name>[.exe]`, executable.
//!
//! A file in place is taken as it is: a marker next to it (`.verified`, the file's size, written
//! after the check) saves rehashing tens of megabytes on every start. The windows' instances and
//! the service may ask at once: one downloads (a lock per file), the others wait and take its
//! file; the file is written beside and renamed into place, so a half-written one is never run.
//! Once a new version is in place the older ones go; all of them go when the plugin is removed.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use crate::manifest::helper_platform;
use crate::registry::PluginEntry;

/// The marker of a checked file: its size.
const MARKER: &str = ".verified";
/// The largest helper Flux downloads.
const MAX_HELPER: u64 = 512 << 20;

/// The folder of a plugin's helpers.
fn plugin_dir(plugin: &str) -> PathBuf {
    crate::paths::helpers_dir().join(plugin)
}

/// The file name of a helper on this system.
fn file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The path of the plugin's helper `name`, downloaded and checked first when it isn't in place.
/// `progress` gets (bytes so far, all bytes) while it downloads. Blocks: the plugin's thread
/// waits.
pub(crate) fn ensure(
    entry: &PluginEntry,
    name: &str,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, String> {
    let spec = entry
        .manifest
        .helper(name)
        .ok_or_else(|| format!("There's no helper \"{name}\" in flux-plugin.toml ([[helpers]])"))?;
    let platform = helper_platform().ok_or("Helpers don't run on this system")?;
    let file = spec.platforms.get(platform).ok_or_else(|| {
        format!("flux-plugin.toml has no file of the helper \"{name}\" for {platform}")
    })?;
    let sha256 = file.sha256.trim().to_ascii_lowercase();
    let dir = plugin_dir(entry.id()).join(&sha256);
    let target = dir.join(file_name(name));
    if ready(&dir, &target) {
        return Ok(target);
    }
    // One download per file in the process; the others wait for it and take its file.
    let lock = lock_of(&target);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if ready(&dir, &target) {
        return Ok(target);
    }
    let url = redirected(&file.url);
    let cancel = AtomicBool::new(false);
    let bytes = crate::catalog::download_bytes(&url, 0, MAX_HELPER, &cancel, progress)?;
    let actual = crate::catalog::sha256_hex(&bytes);
    if actual != sha256 {
        return Err(format!(
            "The helper \"{name}\" from {} doesn't match flux-plugin.toml's checksum (SHA-256 \
             {actual}, expected {sha256}): it wasn't run",
            file.url
        ));
    }
    let program = if file.url.ends_with(".gz") {
        gunzip(&bytes).map_err(|err| format!("The helper \"{name}\": couldn't unpack it: {err}"))?
    } else {
        bytes
    };
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let temp = dir.join(format!(".{}.{}.tmp", file_name(name), std::process::id()));
    write_program(&temp, &program).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("{}: {err}", target.display())
    })?;
    std::fs::rename(&temp, &target).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("{}: {err}", target.display())
    })?;
    let marker = dir.join(MARKER);
    let marker_temp = dir.join(format!("{MARKER}.{}.tmp", std::process::id()));
    std::fs::write(&marker_temp, program.len().to_string())
        .and_then(|()| std::fs::rename(&marker_temp, &marker))
        .map_err(|err| format!("{}: {err}", marker.display()))?;
    remove_old_versions(entry);
    Ok(target)
}

/// The file is in place and was checked: the marker names its size.
fn ready(dir: &Path, target: &Path) -> bool {
    let Ok(marker) = std::fs::read_to_string(dir.join(MARKER)) else {
        return false;
    };
    let size = marker.trim().parse::<u64>().ok();
    std::fs::metadata(target).is_ok_and(|meta| meta.is_file() && Some(meta.len()) == size)
}

/// The lock of one helper's file in this process.
fn lock_of(target: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(target.to_path_buf())
        .or_default()
        .clone()
}

fn write_program(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn gunzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(MAX_HELPER)
        .read_to_end(&mut out)?;
    Ok(out)
}

/// Removes the versions of the plugin's helpers its manifest no longer names (a new one is in
/// place): the folders named by another checksum.
fn remove_old_versions(entry: &PluginEntry) {
    let Some(platform) = helper_platform() else {
        return;
    };
    let current: Vec<String> = entry
        .manifest
        .helpers
        .iter()
        .filter_map(|helper| helper.platforms.get(platform))
        .map(|file| file.sha256.trim().to_ascii_lowercase())
        .collect();
    let Ok(dirs) = std::fs::read_dir(plugin_dir(entry.id())) else {
        return;
    };
    for dir in dirs.flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        let checksum = name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit());
        if checksum && !current.contains(&name) {
            let _ = std::fs::remove_dir_all(dir.path());
        }
    }
}

/// Removes every helper of a plugin: it was removed.
pub fn remove_all(plugin: &str) {
    let dir = plugin_dir(plugin);
    if dir.parent() == Some(crate::paths::helpers_dir().as_path()) && !plugin.is_empty() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The address to download from: the manifest's (tests send it to a local server).
fn redirected(url: &str) -> String {
    #[cfg(test)]
    if let Some(local) = test_redirects().lock().unwrap().get(url) {
        return local.clone();
    }
    url.to_string()
}

/// Addresses of manifests (which must be https) mapped to a local server, for tests.
#[cfg(test)]
pub(crate) fn test_redirects() -> &'static Mutex<HashMap<String, String>> {
    static REDIRECTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    REDIRECTS.get_or_init(Default::default)
}
