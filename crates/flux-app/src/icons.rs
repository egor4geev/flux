//! Значки: однотонные SVG 16×16, вшитые в бинарник. gpui рисует SVG маской — цвет
//! задаёт элемент (`text_color`), поэтому один файл служит в любом цвете. Значок файла и
//! его цвет выбираются по имени ([`file_icon`]), каталога — [`folder_icon`].

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, SharedString, Svg, prelude::*, px, svg};

use crate::theme::UiColors;

/// Перечень значков: имя варианта → файл `assets/icons/<файл>.svg`.
macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum IconName {
            $($name),*
        }

        impl IconName {
            pub const ALL: &[IconName] = &[$(IconName::$name),*];

            /// Путь, по которому gpui просит значок у [`Assets`].
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
    // Интерфейс
    ArrowDown => "arrow-down",
    ArrowUp => "arrow-up",
    Branch => "branch",
    CaseSensitive => "case-sensitive",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    Clock => "clock",
    Close => "close",
    CollapseAll => "collapse-all",
    Command => "command",
    Error => "error",
    File => "file",
    FilePlus => "file-plus",
    FindInFiles => "find-in-files",
    Folder => "folder",
    FolderOpen => "folder-open",
    FolderPlus => "folder-plus",
    Hash => "hash",
    Info => "info",
    Logo => "logo",
    Plus => "plus",
    Project => "project",
    Regex => "regex",
    Replace => "replace",
    ReplaceAll => "replace-all",
    Search => "search",
    Settings => "settings",
    Sidebar => "sidebar",
    Sparkle => "sparkle",
    Terminal => "terminal",
    Warning => "warning",
    WholeWord => "whole-word",
    // Типы файлов
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

/// Размер значка по умолчанию — под строку списка и текст 13 px.
pub const ICON_SIZE: f32 = 16.;

/// Значок размером [`ICON_SIZE`] цветом `color`. Цвет обязателен: gpui красит SVG только
/// собственным цветом элемента, от родителя он не наследуется (без цвета значок не виден).
/// Смена цвета при наведении на родителя — `group_hover` у самого значка.
pub fn icon(name: IconName, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .flex_none()
        .size(px(ICON_SIZE))
        .text_color(color)
}

/// Значок и цвет для файла или каталога.
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

/// Значок файла по имени: сначала точные имена (`Cargo.toml`, `Dockerfile`, `.env.local`),
/// потом расширение. Цвет — оттенок палитры по языку или роли файла; служебное (замки,
/// неизвестное) — приглушённое.
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

/// Значок каталога: открытый или закрытый.
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

/// Логотип flux — цветной SVG (грани и градиенты); рисуется `img(icons::LOGO)`, а не маской.
pub const LOGO: &str = "brand/logo.svg";

/// Цветные ресурсы бренда: путь → содержимое.
const BRAND: &[(&str, &[u8])] = &[(LOGO, include_bytes!("../assets/brand/logo.svg"))];

/// Ресурсы приложения для gpui: значки `icons/<имя>.svg` и бренд `brand/…`.
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

    /// Значки каталогов — заливкой, открытый отличается от закрытого.
    #[test]
    fn folders_open_and_closed() {
        let ui = Theme::flux_night().ui;
        assert_eq!(folder_icon(false, &ui).name, IconName::Folder);
        assert_eq!(folder_icon(true, &ui).name, IconName::FolderOpen);
        assert_eq!(folder_icon(true, &ui).color, ui.folder);
    }
}
