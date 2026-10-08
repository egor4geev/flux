//! Какие файлы принадлежат проекту — одни правила для дерева, поиска файла и поиска
//! по проекту.

use std::ffi::OsStr;
use std::path::Path;

use ignore::WalkBuilder;

/// Служебные каталоги систем контроля версий: в дереве и обходе их нет; они же — признак
/// корня проекта (`find_vcs_root` в flux-search).
pub const VCS_DIRS: [&str; 4] = [".git", ".hg", ".jj", ".svn"];

/// Мусор, который не нужен ни в дереве, ни в поиске.
pub const JUNK_FILES: [&str; 1] = [".DS_Store"];

/// Временный файл атомарного сохранения — `.имя.flux-tmp` (`flux_core::Document::save`):
/// живёт миллисекунды до переименования поверх настоящего.
const SAVE_TMP_SUFFIX: &str = ".flux-tmp";

/// Имя, которого нет ни в дереве, ни в обходе: служебный каталог VCS, мусор или временный
/// файл сохранения.
pub fn is_skipped(name: &OsStr) -> bool {
    VCS_DIRS.iter().chain(&JUNK_FILES).any(|skip| name == *skip)
        || name
            .to_str()
            .is_some_and(|name| name.starts_with('.') && name.ends_with(SAVE_TMP_SUFFIX))
}

/// Обход каталога `start` внутри проекта `root` по общим правилам: `.gitignore` (внутри
/// репозитория git), `.ignore`, глобальный gitignore и `.git/info/exclude`, в том числе из
/// каталогов выше `start`; скрытые файлы видны, симлинки не разворачиваются,
/// [`is_skipped`] — пропускаются. Весь проект — `project_walker(root, root)`.
pub fn project_walker(root: &Path, start: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(start);
    builder
        .hidden(false)
        .follow_links(false)
        // Глобальный gitignore применяется относительно корня проекта, а не каталога,
        // из которого запущен редактор (из Finder это `/`).
        .current_dir(root)
        .filter_entry(|entry| !is_skipped(entry.file_name()));
    builder
}
