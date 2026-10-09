//! Icons: monochrome 16×16 SVGs embedded in the binary. gpui draws an SVG as a mask, so the element
//! sets the color (`text_color`) and one file works in any color. A file's icon and its color are
//! chosen by name ([`file_icon`]), a directory's by [`folder_icon`].

use std::borrow::Cow;

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
    Branch => "branch",
    Check => "check",
    CheckAll => "check-all",
    CheckCircle => "check-circle",
    CaseSensitive => "case-sensitive",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    Clock => "clock",
    Close => "close",
    CollapseAll => "collapse-all",
    Command => "command",
    Commit => "commit",
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
    Hash => "hash",
    History => "history",
    Info => "info",
    Logo => "logo",
    Merge => "merge",
    Minus => "minus",
    More => "more",
    Pencil => "pencil",
    Plus => "plus",
    Project => "project",
    Pull => "pull",
    Push => "push",
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

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
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
