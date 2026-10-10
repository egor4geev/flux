//! Installing a plugin from disk and removing it. The window asks the user first: [`inspect`]
//! reads what the plugin is and what it may do without installing anything.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::registry::{MANIFEST, PluginEntry, PluginSource, load_dir};

/// A plugin picked on disk, read but not installed yet.
#[derive(Debug)]
pub struct Candidate {
    pub entry: PluginEntry,
    /// What the user picked.
    pub path: PathBuf,
    pub kind: CandidateKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// A folder with `flux-plugin.toml`: linked as a plugin under development (built with cargo
    /// when it has `Cargo.toml`).
    Folder,
    /// An archive (`.zip`, `.tar.gz`) with the plugin's folder: unpacked into the plugins folder.
    Archive,
}

/// Reads the plugin at `path` (a folder or an archive) without installing it.
pub fn inspect(path: &Path) -> Result<Candidate, String> {
    if path.is_dir() {
        if !path.join(MANIFEST).is_file() {
            return Err(format!("The folder has no {MANIFEST}"));
        }
        let entry = load_dir(path, PluginSource::Dev)?;
        return Ok(Candidate {
            entry,
            path: path.to_path_buf(),
            kind: CandidateKind::Folder,
        });
    }
    let unpacked = unpack(path)?;
    let root = plugin_root(&unpacked).ok_or_else(|| {
        let _ = std::fs::remove_dir_all(&unpacked);
        format!("The archive has no {MANIFEST}")
    })?;
    let entry = load_dir(&root, PluginSource::Installed).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&unpacked);
    })?;
    Ok(Candidate {
        entry,
        path: path.to_path_buf(),
        kind: CandidateKind::Archive,
    })
}

/// Installs the candidate: an archive is unpacked into the plugins folder (replacing an older
/// version of the plugin); a folder stays where it is. Returns the plugin to load.
pub fn install(candidate: Candidate) -> Result<PluginEntry, String> {
    match candidate.kind {
        CandidateKind::Folder => load_dir(&candidate.path, PluginSource::Dev),
        CandidateKind::Archive => {
            let unpacked = candidate
                .entry
                .files
                .dir()
                .map(Path::to_path_buf)
                .ok_or("The archive isn't unpacked")?;
            let result = install_unpacked(&candidate.entry, &unpacked);
            // The temporary folder the archive was unpacked to (the plugin may be in a folder of
            // it).
            let base = unpack_dir();
            if let Some(temp) = unpacked
                .ancestors()
                .find(|dir| dir.parent() == Some(base.as_path()))
            {
                let _ = std::fs::remove_dir_all(temp);
            }
            result
        }
    }
}

fn install_unpacked(entry: &PluginEntry, unpacked: &Path) -> Result<PluginEntry, String> {
    if let Some(problem) = &entry.problem {
        return Err(problem.clone());
    }
    let plugins = crate::paths::plugins_dir();
    std::fs::create_dir_all(&plugins).map_err(|err| format!("{}: {err}", plugins.display()))?;
    let id = entry.id();
    let target = plugins.join(id);
    let incoming = plugins.join(format!(".{id}.installing"));
    let previous = plugins.join(format!(".{id}.previous"));
    let _ = std::fs::remove_dir_all(&incoming);
    let _ = std::fs::remove_dir_all(&previous);
    copy_dir(unpacked, &incoming).map_err(|err| format!("Couldn't copy the plugin: {err}"))?;
    // The old version steps aside, the new one takes its place, then the old one goes.
    if target.exists() {
        std::fs::rename(&target, &previous)
            .map_err(|err| format!("Couldn't replace the old version: {err}"))?;
    }
    if let Err(err) = std::fs::rename(&incoming, &target) {
        let _ = std::fs::rename(&previous, &target);
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!("Couldn't install the plugin: {err}"));
    }
    let _ = std::fs::remove_dir_all(&previous);
    load_dir(&target, PluginSource::Installed)
}

/// Removes an installed plugin's folder and its secrets in the keychain (best effort: the plugin is
/// gone either way); its data (`paths::data_dir`) stays, as JetBrains IDEs keep a removed plugin's
/// settings. A plugin under development is only unlinked by the caller; a bundled one can't be
/// removed.
pub fn uninstall(entry: &PluginEntry) -> Result<(), String> {
    match entry.source {
        PluginSource::Bundled => Err("A bundled plugin can't be removed, only turned off".into()),
        PluginSource::Dev => Err("A plugin under development is unlinked, not removed".into()),
        PluginSource::Installed => {
            let dir = entry
                .files
                .dir()
                .ok_or("The plugin has no folder")?
                .to_path_buf();
            // Only a folder of the plugins folder: nothing else is ever removed.
            if dir.parent() != Some(crate::paths::plugins_dir().as_path()) {
                return Err(format!("{} isn't in the plugins folder", dir.display()));
            }
            std::fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
            let _ = crate::keychain::delete_all(&crate::keychain::service(entry.id()));
            Ok(())
        }
    }
}

/// Where archives are unpacked before installing.
fn unpack_dir() -> PathBuf {
    crate::paths::cache_dir().join("unpacked")
}

/// Unpacks an archive into a new temporary folder with the system `unzip` or `tar`.
fn unpack(archive: &Path) -> Result<PathBuf, String> {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut command = if name.ends_with(".zip") {
        let mut command = Command::new("unzip");
        command.args(["-q", "-o"]).arg(archive).arg("-d");
        command
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let mut command = Command::new("tar");
        command.arg("-xzf").arg(archive).arg("-C");
        command
    } else {
        return Err(
            "Not a plugin: pick a folder with flux-plugin.toml, or a .zip or .tar.gz archive"
                .into(),
        );
    };
    forget_old_unpacked();
    let dir = unpack_dir().join(format!(
        "{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let output = command
        .arg(&dir)
        .output()
        .map_err(|err| format!("Couldn't unpack the archive: {err}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!(
            "Couldn't unpack the archive: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(dir)
}

/// Removes archives unpacked more than an hour ago: installs the user didn't go on with.
fn forget_old_unpacked() {
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in std::fs::read_dir(unpack_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified < hour_ago);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The plugin's folder in an unpacked archive: its root, or its only folder (macOS archivers
/// add `__MACOSX` and dot files next to it).
fn plugin_root(unpacked: &Path) -> Option<PathBuf> {
    if unpacked.join(MANIFEST).is_file() {
        return Some(unpacked.to_path_buf());
    }
    let folders: Vec<PathBuf> = std::fs::read_dir(unpacked)
        .ok()?
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && name != "__MACOSX"
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    match folders.as_slice() {
        [folder] if folder.join(MANIFEST).is_file() => Some(folder.clone()),
        _ => None,
    }
}

/// Copies a folder: its files and folders (links are skipped).
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::temp_dir;

    /// A plugin's folder with a manifest of `version` and a component.
    fn plugin_folder(id: &str, version: &str) -> PathBuf {
        let dir = temp_dir(&format!("source-{id}"));
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "id = \"{id}\"\nname = \"Packed\"\nversion = \"{version}\"\napi = \"0.2\"\nwasm \
                 = \"plugin.wasm\"\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::create_dir_all(dir.join("icons")).unwrap();
        std::fs::write(dir.join("icons/icon.svg"), "<svg/>").unwrap();
        dir
    }

    fn pack(folder: &Path, archive: &Path, nested: bool) {
        let (parent, item) = if nested {
            (
                folder.parent().unwrap(),
                folder.file_name().unwrap().to_owned(),
            )
        } else {
            (folder, ".".into())
        };
        let status = if archive.extension().is_some_and(|ext| ext == "zip") {
            Command::new("zip")
                .arg("-qr")
                .arg(archive)
                .arg(&item)
                .current_dir(parent)
                .status()
        } else {
            Command::new("tar")
                .arg("-czf")
                .arg(archive)
                .arg(&item)
                .current_dir(parent)
                .status()
        };
        assert!(status.unwrap().success());
    }

    #[test]
    fn a_folder_is_linked_where_it_is() {
        let folder = plugin_folder("test.folder", "1.0.0");
        let candidate = inspect(&folder).unwrap();
        assert_eq!(candidate.kind, CandidateKind::Folder);
        let entry = install(candidate).unwrap();
        assert_eq!(entry.source, PluginSource::Dev);
        assert_eq!(entry.files.dir(), Some(folder.as_path()));
        assert!(uninstall(&entry).is_err());
    }

    #[test]
    fn archives_are_unpacked_into_the_plugins_folder() {
        for (archive, nested) in [("zipped.zip", true), ("tarred.tar.gz", false)] {
            let id = format!("test.{}", archive.split('.').next().unwrap());
            let archive = temp_dir("archives").join(archive);
            pack(&plugin_folder(&id, "1.0.0"), &archive, nested);
            let candidate = inspect(&archive).unwrap();
            assert_eq!(candidate.kind, CandidateKind::Archive);
            assert_eq!(candidate.entry.manifest.version, "1.0.0");
            let entry = install(candidate).unwrap();
            let installed = crate::paths::plugins_dir().join(&id);
            assert_eq!(entry.files.dir(), Some(installed.as_path()));
            assert_eq!(entry.source, PluginSource::Installed);
            assert!(installed.join("icons/icon.svg").is_file());
            // A newer version replaces the old one.
            let newer = temp_dir("archives").join(format!("{id}-2.tar.gz"));
            pack(&plugin_folder(&id, "2.0.0"), &newer, false);
            let entry = install(inspect(&newer).unwrap()).unwrap();
            assert_eq!(entry.manifest.version, "2.0.0");
            uninstall(&entry).unwrap();
            assert!(!installed.exists());
        }
    }

    #[test]
    fn refuses_what_isnt_a_plugin() {
        let empty = temp_dir("not-a-plugin");
        assert!(inspect(&empty).unwrap_err().contains(MANIFEST));
        let text = empty.join("notes.txt");
        std::fs::write(&text, "hi").unwrap();
        assert!(inspect(&text).unwrap_err().starts_with("Not a plugin"));
        let broken = empty.join("broken.zip");
        std::fs::write(&broken, "not a zip").unwrap();
        assert!(inspect(&broken).unwrap_err().starts_with("Couldn't unpack"));
    }

    #[test]
    fn an_archive_for_another_api_is_not_installed() {
        let folder = plugin_folder("test.future", "1.0.0");
        let manifest = std::fs::read_to_string(folder.join(MANIFEST))
            .unwrap()
            .replace("api = \"0.2\"", "api = \"9.0\"");
        std::fs::write(folder.join(MANIFEST), manifest).unwrap();
        let archive = temp_dir("archives").join("future.tar.gz");
        pack(&folder, &archive, false);
        let candidate = inspect(&archive).unwrap();
        assert!(candidate.entry.problem.is_some());
        assert!(install(candidate).unwrap_err().contains("9.0"));
    }
}
