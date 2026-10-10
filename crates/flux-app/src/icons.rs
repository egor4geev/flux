//! Icons: monochrome 16×16 SVGs embedded in the binary. gpui draws an SVG as a mask, so the element
//! sets the color (`text_color`) and one file works in any color. A file's icon and its color are
//! chosen by name ([`file_icon`]), a directory's by [`folder_icon`].
//!
//! Plugins (stage 8) name their icons in views and manifests ([`plugin_icon`]): a built-in icon by
//! its file name, a file type by a file name, or an SVG of their own folder, served by [`Assets`]
//! under `plugins/<id>/…` once the plugin's files are registered ([`register_plugin_files`]).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use flux_plugin::registry::PluginFiles;
use gpui::{AssetSource, Hsla, SharedString, Svg, prelude::*, px, svg};

use crate::theme::UiColors;

/// List of icons: variant name → file `assets/icons/<file>.svg`.
macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum IconName {
            $($name),*
        }

        impl IconName {
            pub const ALL: &[IconName] = &[$(IconName::$name),*];

            /// The path gpui uses to request an icon from [`Assets`].
            pub fn path(self) -> &'static str {
                match self {
                    $(IconName::$name => concat!("icons/", $file, ".svg")),*
                }
            }

            /// The icon whose file is `icons/<name>.svg` ("refresh", "file-rust").
            pub fn from_file_name(name: &str) -> Option<IconName> {
                match name {
                    $($file => Some(IconName::$name),)*
                    _ => None,
                }
            }

            fn source(self) -> &'static [u8] {
                match self {
                    $(IconName::$name => include_bytes!(concat!("../assets/icons/", $file, ".svg"))),*
                }
            }
        }
    };
}

icons! {
    // Interface
    ArrowDown => "arrow-down",
    ArrowLeft => "arrow-left",
    ArrowRight => "arrow-right",
    ArrowUp => "arrow-up",
    Bell => "bell",
    Bulb => "bulb",
    Branch => "branch",
    At => "at",
    Agent => "agent",
    Check => "check",
    CheckAll => "check-all",
    CheckCircle => "check-circle",
    Checklist => "checklist",
    Claude => "claude",
    CaseSensitive => "case-sensitive",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    Clock => "clock",
    Close => "close",
    CollapseAll => "collapse-all",
    Command => "command",
    Commit => "commit",
    Copy => "copy",
    Diff => "diff",
    Error => "error",
    ExpandAll => "expand-all",
    File => "file",
    FilePlus => "file-plus",
    FindInFiles => "find-in-files",
    Folder => "folder",
    FolderOpen => "folder-open",
    FolderPlus => "folder-plus",
    GitLog => "git-log",
    Globe => "globe",
    Hash => "hash",
    History => "history",
    Info => "info",
    Logo => "logo",
    Merge => "merge",
    Minus => "minus",
    More => "more",
    Paperclip => "paperclip",
    Pencil => "pencil",
    Plan => "plan",
    Plug => "plug",
    Plus => "plus",
    Project => "project",
    Pull => "pull",
    Push => "push",
    Puzzle => "puzzle",
    Question => "question",
    Refresh => "refresh",
    Regex => "regex",
    Replace => "replace",
    ReplaceAll => "replace-all",
    Rollback => "rollback",
    Search => "search",
    Settings => "settings",
    Sidebar => "sidebar",
    Sparkle => "sparkle",
    SplitDown => "split-down",
    SplitRight => "split-right",
    Star => "star",
    StarFilled => "star-filled",
    Stash => "stash",
    Stop => "stop",
    Tag => "tag",
    Terminal => "terminal",
    Trash => "trash",
    Unified => "unified",
    Update => "update",
    Warning => "warning",
    WholeWord => "whole-word",
    // File types
    FileArchive => "file-archive",
    FileC => "file-c",
    FileCode => "file-code",
    FileConfig => "file-config",
    FileCss => "file-css",
    FileDocker => "file-docker",
    FileGit => "file-git",
    FileGo => "file-go",
    FileHtml => "file-html",
    FileImage => "file-image",
    FileJavaScript => "file-javascript",
    FileJson => "file-json",
    FileLicense => "file-license",
    FileLock => "file-lock",
    FileMarkdown => "file-markdown",
    FilePackage => "file-package",
    FilePython => "file-python",
    FileReact => "file-react",
    FileReadme => "file-readme",
    FileRust => "file-rust",
    FileShell => "file-shell",
    FileSwift => "file-swift",
    FileText => "file-text",
    FileToml => "file-toml",
    FileTypeScript => "file-typescript",
    FileYaml => "file-yaml",
}

/// Default icon size, to fit a list row and 13 px text.
pub const ICON_SIZE: f32 = 16.;

/// An icon of size [`ICON_SIZE`] in color `color`. The color is required: gpui paints an SVG only
/// with the element's own color, which is not inherited from the parent (without a color the icon
/// is invisible). To change the color when the parent is hovered, use `group_hover` on the icon
/// itself.
pub fn icon(name: IconName, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .flex_none()
        .size(px(ICON_SIZE))
        .text_color(color)
}

/// Icon and color for a file or directory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileIcon {
    pub name: IconName,
    pub color: Hsla,
}

impl FileIcon {
    pub fn render(self) -> Svg {
        icon(self.name, self.color)
    }
}

/// A file's icon by name: exact names first (`Cargo.toml`, `Dockerfile`, `.env.local`), then the
/// extension. The color is a palette shade by the file's language or role; utility files (lock
/// files, unknown types) are muted.
pub fn file_icon(file_name: &str, ui: &UiColors) -> FileIcon {
    let lower = file_name.to_ascii_lowercase();
    let (name, color) = match lower.as_str() {
        "cargo.toml" => (IconName::FilePackage, ui.orange),
        "package.json" => (IconName::FilePackage, ui.red),
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb"
        | "poetry.lock" | "gemfile.lock" | "composer.lock" | "flake.lock" => {
            (IconName::FileLock, ui.dim)
        }
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => (IconName::FileGit, ui.red),
        "dockerfile"
        | ".dockerignore"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | "compose.yml"
        | "compose.yaml" => (IconName::FileDocker, ui.blue),
        "readme" | "readme.md" | "readme.markdown" | "readme.txt" => {
            (IconName::FileReadme, ui.teal)
        }
        "makefile" | "gnumakefile" | "justfile" => (IconName::FileShell, ui.green),
        "go.mod" | "go.sum" => (IconName::FileGo, ui.cyan),
        "rust-toolchain" | "rust-toolchain.toml" => (IconName::FileRust, ui.orange),
        "tsconfig.json" => (IconName::FileTypeScript, ui.blue),
        "jsconfig.json" => (IconName::FileJavaScript, ui.amber),
        _ if lower.starts_with("license") || lower.starts_with("licence") || lower == "copying" => {
            (IconName::FileLicense, ui.amber)
        }
        _ if lower == ".env" || lower.starts_with(".env.") => (IconName::FileConfig, ui.lime),
        _ => match lower.rsplit_once('.').map_or("", |(_, ext)| ext) {
            "rs" => (IconName::FileRust, ui.orange),
            "ts" | "mts" | "cts" => (IconName::FileTypeScript, ui.blue),
            "js" | "mjs" | "cjs" => (IconName::FileJavaScript, ui.amber),
            "tsx" | "jsx" => (IconName::FileReact, ui.cyan),
            "json" | "jsonc" | "json5" => (IconName::FileJson, ui.amber),
            "md" | "markdown" | "mdx" => (IconName::FileMarkdown, ui.indigo),
            "toml" => (IconName::FileToml, ui.text_muted),
            "yaml" | "yml" => (IconName::FileYaml, ui.pink),
            "py" | "pyi" | "pyw" => (IconName::FilePython, ui.blue),
            "go" => (IconName::FileGo, ui.cyan),
            "sh" | "bash" | "zsh" | "fish" | "ksh" => (IconName::FileShell, ui.green),
            "swift" => (IconName::FileSwift, ui.orange),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" => {
                (IconName::FileC, ui.indigo)
            }
            "html" | "htm" | "xhtml" => (IconName::FileHtml, ui.orange),
            "css" | "scss" | "sass" | "less" => (IconName::FileCss, ui.violet),
            "lock" => (IconName::FileLock, ui.dim),
            "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "tiff" | "avif"
            | "icns" => (IconName::FileImage, ui.violet),
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "dmg" => {
                (IconName::FileArchive, ui.amber)
            }
            "txt" | "log" | "text" | "rst" => (IconName::FileText, ui.text_muted),
            "env" | "ini" | "cfg" | "conf" | "editorconfig" | "properties" | "plist" => {
                (IconName::FileConfig, ui.lime)
            }
            "rb" => (IconName::FileCode, ui.red),
            "java" | "xml" | "svelte" => (IconName::FileCode, ui.orange),
            "kt" | "kts" | "wasm" | "wat" => (IconName::FileCode, ui.violet),
            "php" => (IconName::FileCode, ui.indigo),
            "lua" | "dart" => (IconName::FileCode, ui.blue),
            "sql" => (IconName::FileCode, ui.amber),
            "vue" | "zig" => (IconName::FileCode, ui.green),
            _ => (IconName::File, ui.text_muted),
        },
    };
    FileIcon { name, color }
}

/// Directory icon: open or closed.
pub fn folder_icon(expanded: bool, ui: &UiColors) -> FileIcon {
    FileIcon {
        name: if expanded {
            IconName::FolderOpen
        } else {
            IconName::Folder
        },
        color: ui.folder,
    }
}

/// The flux logo is a color SVG (facets and gradients); it is drawn with `img(icons::LOGO)`, not as
/// a mask.
pub const LOGO: &str = "brand/logo.svg";

/// Color brand assets: path → contents.
const BRAND: &[(&str, &[u8])] = &[(LOGO, include_bytes!("../assets/brand/logo.svg"))];

/// App assets for gpui: icons `icons/<name>.svg` and branding `brand/…`.
pub struct Assets;

/// The prefix of the asset paths of plugins' files.
const PLUGIN_ASSETS: &str = "plugins/";

/// The files of the plugins that may be drawn, by plugin id. gpui asks [`Assets`] for them from
/// any thread.
static PLUGIN_FILES: LazyLock<Mutex<HashMap<String, PluginFiles>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The asset path of a file of a plugin (`icons/todo.svg`): `svg().path(…)` draws it once the
/// plugin's files are registered ([`register_plugin_files`]).
pub fn plugin_asset(plugin: &str, relative: &str) -> SharedString {
    format!("{PLUGIN_ASSETS}{plugin}/{relative}").into()
}

/// Makes a plugin's files available to [`Assets`] under [`plugin_asset`] paths.
pub fn register_plugin_files(plugin: &str, files: PluginFiles) {
    PLUGIN_FILES
        .lock()
        .unwrap()
        .insert(plugin.to_string(), files);
}

/// Forgets a plugin's files: it was turned off or removed.
pub fn unregister_plugin_files(plugin: &str) {
    PLUGIN_FILES.lock().unwrap().remove(plugin);
}

/// A file of a registered plugin by its asset path (`plugins/<id>/icons/todo.svg`).
fn plugin_file(path: &str) -> Option<Cow<'static, [u8]>> {
    let (plugin, relative) = path.strip_prefix(PLUGIN_ASSETS)?.split_once('/')?;
    let files = PLUGIN_FILES.lock().unwrap().get(plugin)?.clone();
    files.read(relative)
}

/// An icon a plugin names (the `ui` interface, a tool window in the manifest).
#[derive(Clone, Debug, PartialEq)]
pub enum PluginIcon {
    /// A built-in icon by its file name: "refresh", "expand-all".
    Builtin(IconName),
    /// The icon of a file type ("file:main.rs") or a folder ("folder", "folder-open"), in its own
    /// color.
    File(FileIcon),
    /// An SVG of the plugin's folder ("icons/todo.svg"), as an asset path.
    Asset(SharedString),
}

impl PluginIcon {
    /// The icon of size [`ICON_SIZE`]: in `color`, a file type in its own.
    pub fn render(&self, color: Hsla) -> Svg {
        match self {
            PluginIcon::Builtin(name) => icon(*name, color),
            PluginIcon::File(file) => file.render(),
            PluginIcon::Asset(path) => icon_at(path.clone(), color),
        }
    }

    /// The asset path gpui draws.
    pub fn path(&self) -> SharedString {
        match self {
            PluginIcon::Builtin(name) => name.path().into(),
            PluginIcon::File(file) => file.name.path().into(),
            PluginIcon::Asset(path) => path.clone(),
        }
    }
}

/// The icon `name` of the plugin `plugin`: "file:<name>" — a file type, a path with `/` or an
/// `.svg` — the plugin's file, otherwise a built-in icon by its file name; `None` if there is no
/// such built-in icon.
pub fn plugin_icon(plugin: &str, name: &str, ui: &UiColors) -> Option<PluginIcon> {
    if let Some(file) = name.strip_prefix("file:") {
        return Some(PluginIcon::File(file_icon(file, ui)));
    }
    if name.contains('/') || name.ends_with(".svg") {
        return Some(PluginIcon::Asset(plugin_asset(plugin, name)));
    }
    match name {
        "folder" => Some(PluginIcon::File(folder_icon(false, ui))),
        "folder-open" => Some(PluginIcon::File(folder_icon(true, ui))),
        _ => IconName::from_file_name(name).map(PluginIcon::Builtin),
    }
}

/// An icon by its asset path (a plugin's SVG), like [`icon`].
pub fn icon_at(path: impl Into<SharedString>, color: Hsla) -> Svg {
    svg()
        .path(path.into())
        .flex_none()
        .size(px(ICON_SIZE))
        .text_color(color)
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(PLUGIN_ASSETS) {
            return Ok(plugin_file(path));
        }
        let brand = BRAND.iter().find(|(name, _)| *name == path);
        Ok(IconName::ALL
            .iter()
            .find(|icon| icon.path() == path)
            .map(|icon| icon.source())
            .or(brand.map(|(_, source)| *source))
            .map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(IconName::ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|icon| icon.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn every_icon_is_a_valid_svg() {
        for icon in IconName::ALL {
            let source = std::str::from_utf8(icon.source()).unwrap();
            assert!(source.trim_start().starts_with("<svg"), "{icon:?}");
            assert!(source.contains("viewBox=\"0 0 16 16\""), "{icon:?}");
        }
    }

    #[test]
    fn icons_by_file_name() {
        assert_eq!(IconName::from_file_name("refresh"), Some(IconName::Refresh));
        assert_eq!(
            IconName::from_file_name("file-rust"),
            Some(IconName::FileRust)
        );
        assert_eq!(IconName::from_file_name("puzzle"), Some(IconName::Puzzle));
        assert_eq!(IconName::from_file_name("nothing"), None);
        for icon in IconName::ALL {
            let name = icon
                .path()
                .strip_prefix("icons/")
                .unwrap()
                .strip_suffix(".svg")
                .unwrap();
            assert_eq!(IconName::from_file_name(name), Some(*icon));
        }
    }

    #[test]
    fn plugin_icons_by_name() {
        let ui = Theme::flux_night().ui;
        let resolve = |name: &str| plugin_icon("flux.todo", name, &ui);
        assert_eq!(
            resolve("refresh"),
            Some(PluginIcon::Builtin(IconName::Refresh))
        );
        assert_eq!(
            resolve("file:main.rs"),
            Some(PluginIcon::File(file_icon("main.rs", &ui)))
        );
        assert_eq!(
            resolve("folder"),
            Some(PluginIcon::File(folder_icon(false, &ui)))
        );
        assert_eq!(
            resolve("icons/todo.svg"),
            Some(PluginIcon::Asset("plugins/flux.todo/icons/todo.svg".into()))
        );
        assert_eq!(
            resolve("todo.svg"),
            Some(PluginIcon::Asset("plugins/flux.todo/todo.svg".into()))
        );
        assert_eq!(resolve("no-such-icon"), None);
    }

    #[test]
    fn plugin_files_are_served_while_registered() {
        static FILES: &[(&str, &[u8])] = &[("icons/demo.svg", b"<svg/>")];
        let path = plugin_asset("test.assets", "icons/demo.svg");
        assert!(Assets.load(&path).unwrap().is_none());
        register_plugin_files("test.assets", PluginFiles::Embedded(FILES));
        assert_eq!(Assets.load(&path).unwrap().as_deref(), Some(&b"<svg/>"[..]));
        assert!(
            Assets
                .load(&plugin_asset("test.assets", "icons/missing.svg"))
                .unwrap()
                .is_none()
        );
        unregister_plugin_files("test.assets");
        assert!(Assets.load(&path).unwrap().is_none());
    }

    #[test]
    fn assets_are_found_by_path() {
        let source = Assets.load(IconName::Search.path()).unwrap();
        assert!(source.is_some());
        let logo = Assets.load(LOGO).unwrap().unwrap();
        assert!(std::str::from_utf8(&logo).unwrap().contains("<svg"));
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }

    #[test]
    fn file_icons_by_name_then_extension() {
        let ui = Theme::flux_night().ui;
        let name = |file: &str| file_icon(file, &ui).name;
        assert_eq!(name("main.rs"), IconName::FileRust);
        assert_eq!(name("Cargo.toml"), IconName::FilePackage);
        assert_eq!(name("config.toml"), IconName::FileToml);
        assert_eq!(name("App.TSX"), IconName::FileReact);
        assert_eq!(name(".gitignore"), IconName::FileGit);
        assert_eq!(name("README.md"), IconName::FileReadme);
        assert_eq!(name("notes.md"), IconName::FileMarkdown);
        assert_eq!(name("Makefile"), IconName::FileShell);
        assert_eq!(name("archive.tar.gz"), IconName::FileArchive);
        assert_eq!(name("LICENSE-MIT"), IconName::FileLicense);
        assert_eq!(name(".env.local"), IconName::FileConfig);
        assert_eq!(name("Cargo.lock"), IconName::FileLock);
        assert_eq!(name("types.d.ts"), IconName::FileTypeScript);
        assert_eq!(name("Без расширения"), IconName::File);
        assert_eq!(name("trailing."), IconName::File);
    }

    /// Directory icons are filled; the open one differs from the closed one.
    #[test]
    fn folders_open_and_closed() {
        let ui = Theme::flux_night().ui;
        assert_eq!(folder_icon(false, &ui).name, IconName::Folder);
        assert_eq!(folder_icon(true, &ui).name, IconName::FolderOpen);
        assert_eq!(folder_icon(true, &ui).color, ui.folder);
    }
}
