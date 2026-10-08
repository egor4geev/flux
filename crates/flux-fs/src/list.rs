//! Содержимое одного каталога для дерева файлов.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use crate::rules::{is_skipped, project_walker};

/// Файл или каталог. Симлинк — по тому, на что он указывает; битый — файл.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
}

/// Строка каталога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Имя в каталоге (не путь).
    pub name: String,
    pub kind: EntryKind,
    /// Исключён `.gitignore` / `.ignore` (или лежит в исключённом каталоге): в дереве
    /// приглушён, в поиске файла и по проекту его нет.
    pub ignored: bool,
}

/// Читает каталог `dir` внутри проекта `root`: каталоги первыми, имена — в естественном
/// порядке ([`compare_names`]); служебные каталоги VCS и мусор ([`is_skipped`]) пропущены.
///
/// `ignored` — ровно то, что исключил бы обход проекта (поиск файла и по проекту): тот же
/// обход ([`project_walker`]) на глубину одного каталога, что он не отдал — исключено.
/// `dir_ignored` — сам `dir` исключён: тогда исключено и всё внутри (git не заглядывает
/// в исключённые каталоги, и правила внутри них не действуют), обход не нужен.
///
/// Имена не в UTF-8 пропускаются (на APFS их не бывает). Блокирует — звать из фона.
pub fn list_dir(root: &Path, dir: &Path, dir_ignored: bool) -> io::Result<Vec<DirEntry>> {
    let read = fs::read_dir(dir)?;
    let visible = if dir_ignored {
        HashSet::new()
    } else {
        visible_names(root, dir)
    };
    let mut entries = Vec::new();
    for entry in read {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if is_skipped(&name) {
            continue;
        }
        let ignored = dir_ignored || !visible.contains(&name);
        let Ok(name) = name.into_string() else {
            continue;
        };
        // Тип — из самого каталога (без лишнего `stat`); симлинк — по тому, на что указывает.
        let is_dir = match entry.file_type() {
            Ok(kind) if !kind.is_symlink() => kind.is_dir(),
            _ => fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()),
        };
        let kind = if is_dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        entries.push(DirEntry {
            name,
            kind,
            ignored,
        });
    }
    entries.sort_by(compare_entries);
    Ok(entries)
}

/// Имена в `dir`, которые обход проекта не исключает.
fn visible_names(root: &Path, dir: &Path) -> HashSet<OsString> {
    project_walker(root, dir)
        .max_depth(Some(1))
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .map(|entry| entry.file_name().to_os_string())
        .collect()
}

fn compare_entries(a: &DirEntry, b: &DirEntry) -> Ordering {
    let dirs_first = (b.kind == EntryKind::Dir).cmp(&(a.kind == EntryKind::Dir));
    dirs_first.then_with(|| compare_names(&a.name, &b.name))
}

/// Естественный порядок имён: без учёта регистра (и для кириллицы, «ё» — рядом с «е»),
/// числа — по значению (`file2` раньше `file10`). Имена, равные в этом смысле (`a1` и
/// `a01`, `Readme` и `README`), упорядочены по байтам — порядок полный.
pub fn compare_names(a: &str, b: &str) -> Ordering {
    natural(a, b).then_with(|| a.cmp(b))
}

fn natural(a: &str, b: &str) -> Ordering {
    let (mut i, mut j) = (0, 0);
    loop {
        let (x, y) = match (a[i..].chars().next(), b[j..].chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => (x, y),
        };
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let a_end = i + digits_len(&a[i..]);
            let b_end = j + digits_len(&b[j..]);
            let order = compare_numbers(&a[i..a_end], &b[j..b_end]);
            (i, j) = (a_end, b_end);
            order
        } else {
            (i, j) = (i + x.len_utf8(), j + y.len_utf8());
            fold(x).cmp(fold(y))
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// Символ для сравнения: строчный; «ё» — как «е» (иначе «ёж» встал бы после «яблоко»).
fn fold(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase().map(|c| if c == 'ё' { 'е' } else { c })
}

fn digits_len(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

/// Числа из цифр любой длины — без переполнения: длина без ведущих нулей, затем цифры.
fn compare_numbers(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Дерево во временном каталоге: `(путь, содержимое)`; путь на `/` — пустой каталог.
    pub(crate) fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = dir.path().join(path);
            if path.to_string_lossy().ends_with('/') {
                fs::create_dir_all(&path).unwrap();
                continue;
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        dir
    }

    /// Строки каталога: имя (у каталога — с `/`) и признак «исключён».
    fn list(root: &Path, dir: &str, dir_ignored: bool) -> Vec<(String, bool)> {
        list_dir(root, &root.join(dir), dir_ignored)
            .unwrap()
            .into_iter()
            .map(|entry| {
                let slash = if entry.kind == EntryKind::Dir {
                    "/"
                } else {
                    ""
                };
                (format!("{}{slash}", entry.name), entry.ignored)
            })
            .collect()
    }

    fn names(entries: &[(String, bool)]) -> Vec<&str> {
        entries.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn ignored(entries: &[(String, bool)]) -> Vec<&str> {
        entries
            .iter()
            .filter(|(_, ignored)| *ignored)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    #[test]
    fn directories_first_then_natural_order() {
        let dir = tree(&[
            ("b.txt", ""),
            ("A.txt", ""),
            ("file10.rs", ""),
            ("file2.rs", ""),
            ("zeta/", ""),
            ("Alpha/", ""),
            (".hidden", ""),
        ]);
        let got = list(dir.path(), "", false);
        assert_eq!(
            names(&got),
            [
                "Alpha/",
                "zeta/",
                ".hidden",
                "A.txt",
                "b.txt",
                "file2.rs",
                "file10.rs"
            ]
        );
        // Вне репозитория .gitignore не действует, скрытые файлы — не исключены.
        assert!(ignored(&got).is_empty());
    }

    #[test]
    fn gitignore_marks_entries_as_ignored() {
        let dir = tree(&[
            (".git/HEAD", "ref: refs/heads/main"),
            (".gitignore", "/target\n*.log\nbuild/\n!keep.log\n"),
            ("target/debug/flux", ""),
            ("src/main.rs", ""),
            ("src/build/out.o", ""),
            ("src/trace.log", ""),
            ("keep.log", ""),
            ("debug.log", ""),
            ("notes.txt", ""),
        ]);
        let root = dir.path();
        let top = list(root, "", false);
        // .git нет вовсе, .gitignore виден и не исключён.
        assert_eq!(
            names(&top),
            [
                "src/",
                "target/",
                ".gitignore",
                "debug.log",
                "keep.log",
                "notes.txt"
            ]
        );
        assert_eq!(ignored(&top), ["target/", "debug.log"]);
        // Правила корня действуют и в подкаталогах: `build/` — каталог на любой глубине.
        let src = list(root, "src", false);
        assert_eq!(names(&src), ["build/", "main.rs", "trace.log"]);
        assert_eq!(ignored(&src), ["build/", "trace.log"]);
    }

    #[test]
    fn nested_gitignore_and_negation() {
        let dir = tree(&[
            (".git/HEAD", ""),
            (".gitignore", "*.gen\n"),
            ("crates/a/.gitignore", "!wanted.gen\nlocal.txt\n"),
            ("crates/a/wanted.gen", ""),
            ("crates/a/other.gen", ""),
            ("crates/a/local.txt", ""),
            ("crates/b/local.txt", ""),
        ]);
        let root = dir.path();
        assert_eq!(
            ignored(&list(root, "crates/a", false)),
            ["local.txt", "other.gen"]
        );
        assert!(ignored(&list(root, "crates/b", false)).is_empty());
    }

    #[test]
    fn gitignore_needs_a_repository_but_ignore_files_do_not() {
        let dir = tree(&[
            (".gitignore", "a.txt\n"),
            (".ignore", "b.txt\n"),
            ("a.txt", ""),
            ("b.txt", ""),
        ]);
        assert_eq!(ignored(&list(dir.path(), "", false)), ["b.txt"]);
    }

    #[test]
    fn git_info_exclude_is_respected() {
        let dir = tree(&[
            (".git/HEAD", ""),
            (".git/info/exclude", "secret.txt\n"),
            ("secret.txt", ""),
            ("public.txt", ""),
        ]);
        assert_eq!(ignored(&list(dir.path(), "", false)), ["secret.txt"]);
    }

    #[test]
    fn inside_an_ignored_directory_everything_is_ignored() {
        let dir = tree(&[
            (".git/HEAD", ""),
            (".gitignore", "/target\n"),
            ("target/.gitignore", "!*\n"),
            ("target/debug/flux", ""),
            ("target/notes.txt", ""),
        ]);
        let got = list(dir.path(), "target", true);
        assert_eq!(names(&got), ["debug/", ".gitignore", "notes.txt"]);
        assert_eq!(ignored(&got).len(), 3);
    }

    #[test]
    fn vcs_dirs_and_junk_are_not_listed() {
        let dir = tree(&[
            (".git/HEAD", ""),
            (".hg/store", ""),
            (".jj/repo", ""),
            (".svn/entries", ""),
            (".DS_Store", ""),
            ("sub/.git", "gitdir: ../.git/worktrees/sub"),
            ("sub/.DS_Store", ""),
            ("sub/file.txt", ""),
            ("sub/.file.txt.flux-tmp", ""),
            (".github/ci.yml", ""),
        ]);
        let root = dir.path();
        assert_eq!(names(&list(root, "", false)), [".github/", "sub/"]);
        assert_eq!(names(&list(root, "sub", false)), ["file.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_listed_by_their_target() {
        let dir = tree(&[("real/file.txt", "")]);
        let root = dir.path();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), root.join("broken")).unwrap();
        let entries = list_dir(root, root, false).unwrap();
        let kinds: Vec<(&str, EntryKind)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("link", EntryKind::Dir),
                ("real", EntryKind::Dir),
                ("broken", EntryKind::File),
            ]
        );
        // Симлинк на каталог раскрывается, как каталог, и его содержимое не исключено.
        assert_eq!(list(root, "link", false), [("file.txt".to_string(), false)]);
    }

    #[test]
    fn missing_directory_is_an_error() {
        let dir = tree(&[]);
        assert!(list_dir(dir.path(), &dir.path().join("nope"), false).is_err());
    }

    #[test]
    fn names_compare_naturally() {
        let mut names = vec![
            "file10",
            "file2",
            "File1",
            "file1",
            "a01",
            "a1",
            "b",
            "B",
            "_x",
            ".env",
            "ёж",
            "Яблоко",
            "арбуз",
            "еда",
            "x9y",
            "x10y",
            "x9",
            "x",
        ];
        names.sort_by(|a, b| compare_names(a, b));
        assert_eq!(
            names,
            [
                ".env",
                "_x",
                "a01",
                "a1",
                "B",
                "b",
                "File1",
                "file1",
                "file2",
                "file10",
                "x",
                "x9",
                "x9y",
                "x10y",
                "арбуз",
                "еда",
                "ёж",
                "Яблоко"
            ]
        );
    }

    #[test]
    fn name_order_is_total() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        assert_eq!(compare_names("a", "a"), Equal);
        assert_eq!(compare_names("a01", "a1"), Less);
        assert_eq!(compare_names("a1", "a01"), Greater);
        assert_eq!(compare_names("README", "readme"), Less);
        // Огромные числа не переполняются.
        assert_eq!(
            compare_names("v99999999999999999999999", "v100000000000000000000000"),
            Less
        );
        // Цифры раньше букв, как в Finder.
        assert_eq!(compare_names("1abc", "abc"), Less);
    }
}
