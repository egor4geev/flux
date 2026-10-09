//! Where plugins come from, as in JetBrains IDEs: bundled into Flux (can be turned off, not
//! removed), installed (a copy in the plugins folder) and under development (the author's folder,
//! linked). A plugin under development overrides an installed or bundled one with the same id, an
//! installed one a bundled one: that's how a bundled plugin is developed and updated.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::API_VERSION;
use crate::locales::Locales;
use crate::manifest::Manifest;

/// The manifest's file name in a plugin's folder.
pub const MANIFEST: &str = "flux-plugin.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginSource {
    /// Shipped inside Flux.
    Bundled,
    /// Installed from a file (later also from the catalog).
    Installed,
    /// A folder of the author's: rebuilt and reloaded when it changes.
    Dev,
}

/// A plugin shipped inside Flux: its files, embedded at build time (paths relative to its
/// folder, `/`-separated).
#[derive(Debug, Clone, Copy)]
pub struct Bundled {
    pub files: &'static [(&'static str, &'static [u8])],
}

/// A plugin's files: a folder on disk, or the embedded files of a bundled plugin.
#[derive(Debug, Clone)]
pub enum PluginFiles {
    Disk(PathBuf),
    Embedded(&'static [(&'static str, &'static [u8])]),
}

impl PluginFiles {
    /// A file by its path relative to the plugin's folder (`icons/todo.svg`).
    pub fn read(&self, relative: &str) -> Option<Cow<'static, [u8]>> {
        match self {
            PluginFiles::Disk(dir) => {
                let relative = Path::new(relative);
                // The plugin's files only: no `..`, no absolute paths.
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return None;
                }
                std::fs::read(dir.join(relative)).ok().map(Cow::Owned)
            }
            PluginFiles::Embedded(files) => files
                .iter()
                .find(|(path, _)| *path == relative)
                .map(|(_, bytes)| Cow::Borrowed(*bytes)),
        }
    }

    /// The files right inside a folder of the plugin (`locales/`), as relative paths.
    pub fn list(&self, folder: &str) -> Vec<String> {
        match self {
            PluginFiles::Disk(dir) => std::fs::read_dir(dir.join(folder))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| entry.path().is_file())
                .map(|entry| format!("{folder}{}", entry.file_name().to_string_lossy()))
                .collect(),
            PluginFiles::Embedded(files) => files
                .iter()
                .map(|(path, _)| *path)
                .filter(|path| {
                    path.strip_prefix(folder)
                        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
                })
                .map(str::to_string)
                .collect(),
        }
    }

    /// The folder on disk; none for a bundled plugin.
    pub fn dir(&self) -> Option<&Path> {
        match self {
            PluginFiles::Disk(dir) => Some(dir),
            PluginFiles::Embedded(_) => None,
        }
    }
}

/// A plugin Flux knows about.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub manifest: Manifest,
    pub files: PluginFiles,
    pub source: PluginSource,
    pub locales: Locales,
    /// Why it can't run: an API version Flux doesn't have, a missing component. Such a plugin is
    /// listed, but can't be turned on.
    pub problem: Option<String>,
}

impl PluginEntry {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// The component's bytes; none for a plugin without code or when the file is missing.
    pub fn wasm(&self) -> Option<Cow<'static, [u8]>> {
        self.files.read(self.manifest.wasm.as_deref()?)
    }

    /// A string of the manifest (the name, a command's title) in the interface language.
    pub fn translate<'a>(&'a self, language: &str, text: &'a str) -> &'a str {
        self.locales.translate(language, text)
    }
}

/// What a scan found.
#[derive(Debug, Default)]
pub struct Scan {
    /// One per id, in a stable order: bundled first, then by name.
    pub plugins: Vec<PluginEntry>,
    /// Folders that look like plugins but aren't readable: a broken manifest.
    pub errors: Vec<ScanError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

/// All the plugins: the bundled ones, the installed ones (`paths::plugins_dir()`), and the
/// folders under development.
pub fn scan(bundled: &[Bundled], dev: &[PathBuf]) -> Scan {
    scan_dirs(bundled, &crate::paths::plugins_dir(), dev)
}

/// [`scan`] with the folder of installed plugins given.
pub(crate) fn scan_dirs(bundled: &[Bundled], installed: &Path, dev: &[PathBuf]) -> Scan {
    let mut scan = Scan::default();
    let add = |entry: PluginEntry, scan: &mut Scan| {
        // A later source overrides an earlier one with the same id.
        scan.plugins.retain(|known| known.id() != entry.id());
        scan.plugins.push(entry);
    };
    for plugin in bundled {
        match load_bundled(plugin) {
            Ok(entry) => add(entry, &mut scan),
            Err(message) => scan.errors.push(ScanError {
                path: PathBuf::from("<bundled>"),
                message,
            }),
        }
    }
    let mut folders: Vec<PathBuf> = std::fs::read_dir(installed)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join(MANIFEST).is_file())
        .collect();
    folders.sort();
    for folder in folders {
        match load_dir(&folder, PluginSource::Installed) {
            Ok(entry) => add(entry, &mut scan),
            Err(message) => scan.errors.push(ScanError {
                path: folder,
                message,
            }),
        }
    }
    for folder in dev {
        match load_dir(folder, PluginSource::Dev) {
            Ok(entry) => add(entry, &mut scan),
            Err(message) => scan.errors.push(ScanError {
                path: folder.clone(),
                message,
            }),
        }
    }
    let rank = |entry: &PluginEntry| entry.source != PluginSource::Bundled;
    scan.plugins
        .sort_by_cached_key(|entry| (rank(entry), entry.manifest.name.to_lowercase()));
    scan
}

/// A plugin in a folder: its manifest, translations, and what keeps it from running.
pub fn load_dir(dir: &Path, source: PluginSource) -> Result<PluginEntry, String> {
    let text =
        std::fs::read_to_string(dir.join(MANIFEST)).map_err(|err| format!("{MANIFEST}: {err}"))?;
    let manifest = Manifest::parse(&text).map_err(|err| format!("{MANIFEST}: {err}"))?;
    Ok(entry(
        manifest,
        PluginFiles::Disk(dir.to_path_buf()),
        source,
    ))
}

/// A bundled plugin from its embedded files.
pub fn load_bundled(bundled: &Bundled) -> Result<PluginEntry, String> {
    let files = PluginFiles::Embedded(bundled.files);
    let bytes = files
        .read(MANIFEST)
        .ok_or_else(|| format!("{MANIFEST} is missing"))?;
    let text = std::str::from_utf8(&bytes).map_err(|err| format!("{MANIFEST}: {err}"))?;
    let manifest = Manifest::parse(text).map_err(|err| format!("{MANIFEST}: {err}"))?;
    Ok(entry(manifest, files, PluginSource::Bundled))
}

fn entry(manifest: Manifest, files: PluginFiles, source: PluginSource) -> PluginEntry {
    let locales = Locales::load(&files);
    let problem = if manifest.api != API_VERSION {
        Some(format!(
            "Built for plugin API {}; this Flux has {API_VERSION}",
            manifest.api
        ))
    } else {
        match &manifest.wasm {
            // A plugin under development may not be built yet: Flux builds it.
            Some(wasm) if files.read(wasm).is_none() && source != PluginSource::Dev => {
                Some(format!("The component {wasm} is missing"))
            }
            _ => None,
        }
    };
    PluginEntry {
        manifest,
        files,
        source,
        locales,
        problem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::temp_dir;

    fn manifest(id: &str, name: &str, api: &str) -> String {
        format!(
            "id = \"{id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi = \"{api}\"\nwasm = \
             \"plugin.wasm\"\n"
        )
    }

    /// A plugin folder in `parent`, with or without its component.
    fn folder(parent: &Path, id: &str, name: &str, wasm: bool) -> PathBuf {
        let dir = parent.join(id);
        std::fs::create_dir_all(dir.join("locales")).unwrap();
        std::fs::write(dir.join(MANIFEST), manifest(id, name, API_VERSION)).unwrap();
        if wasm {
            std::fs::write(dir.join("plugin.wasm"), b"\0asm").unwrap();
        }
        std::fs::write(
            dir.join("locales/ru.toml"),
            format!("\"{name}\" = \"Плагин\"\n"),
        )
        .unwrap();
        dir
    }

    const BUNDLED_MANIFEST: &str = "id = \"flux.todo\"\nname = \"TODO\"\nversion = \"0.1.0\"\napi \
                                    = \"0.1\"\nwasm = \"todo.wasm\"\n";
    const BUNDLED: Bundled = Bundled {
        files: &[
            ("flux-plugin.toml", BUNDLED_MANIFEST.as_bytes()),
            ("todo.wasm", b"\0asm"),
            ("locales/ru.toml", "\"TODO\" = \"Задачи\"\n".as_bytes()),
            ("icons/todo.svg", b"<svg/>"),
        ],
    };

    #[test]
    fn finds_bundled_installed_and_dev_plugins() {
        let installed = temp_dir("installed");
        folder(&installed, "someone.zeta", "Zeta", true);
        folder(&installed, "someone.alpha", "Alpha", true);
        folder(&installed, "someone.broken", "Broken", false);
        std::fs::write(installed.join("someone.broken/flux-plugin.toml"), "id = 1").unwrap();
        let dev_parent = temp_dir("dev");
        let dev = folder(&dev_parent, "someone.alpha", "Alpha Dev", false);
        let scan = scan_dirs(&[BUNDLED], &installed, &[dev]);
        let names: Vec<&str> = scan
            .plugins
            .iter()
            .map(|p| p.manifest.name.as_str())
            .collect();
        // Bundled first, then by name; the dev folder overrides the installed copy.
        assert_eq!(names, ["TODO", "Alpha Dev", "Zeta"]);
        let alpha = &scan.plugins[1];
        assert_eq!(alpha.source, PluginSource::Dev);
        // A plugin under development may be unbuilt: Flux builds it.
        assert_eq!(alpha.problem, None);
        assert_eq!(alpha.translate("ru", "Alpha Dev"), "Плагин");
        assert_eq!(scan.errors.len(), 1);
        assert!(scan.errors[0].path.ends_with("someone.broken"));
    }

    #[test]
    fn bundled_files_and_translations() {
        let entry = load_bundled(&BUNDLED).unwrap();
        assert_eq!(entry.source, PluginSource::Bundled);
        assert_eq!(entry.wasm().as_deref(), Some(&b"\0asm"[..]));
        assert_eq!(entry.translate("ru", "TODO"), "Задачи");
        assert_eq!(entry.translate("en", "TODO"), "TODO");
        assert_eq!(entry.files.list("locales/"), ["locales/ru.toml"]);
        assert_eq!(entry.files.list("icons/"), ["icons/todo.svg"]);
        assert!(entry.files.dir().is_none());
    }

    #[test]
    fn what_keeps_a_plugin_from_running() {
        let parent = temp_dir("problems");
        let missing = folder(&parent, "someone.missing", "Missing", false);
        let entry = load_dir(&missing, PluginSource::Installed).unwrap();
        assert!(entry.problem.unwrap().contains("plugin.wasm"));
        let future = folder(&parent, "someone.future", "Future", true);
        std::fs::write(
            future.join(MANIFEST),
            manifest("someone.future", "Future", "2.0"),
        )
        .unwrap();
        let entry = load_dir(&future, PluginSource::Installed).unwrap();
        assert!(entry.problem.unwrap().contains("2.0"));
    }

    #[test]
    fn files_stay_inside_the_plugin() {
        let parent = temp_dir("inside");
        let dir = folder(&parent, "someone.inside", "Inside", true);
        std::fs::write(parent.join("secret.txt"), "secret").unwrap();
        let files = PluginFiles::Disk(dir);
        assert!(files.read("plugin.wasm").is_some());
        assert!(files.read("../secret.txt").is_none());
        assert!(files.read("/etc/hosts").is_none());
    }
}
