//! Файлы проекта: корень по системе контроля версий и обход с учётом `.gitignore`.
//! Те же правила обхода использует поиск по проекту ([`crate::grep`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ignore::{DirEntry, WalkBuilder, WalkState};

/// Больше файлов обход не отдаёт (`truncated`): поиск файла по всей домашней папке
/// не должен съесть память и время.
pub const MAX_FILES: usize = 100_000;

/// Служебные каталоги систем контроля версий: признак корня проекта; в обход не попадают.
const VCS_DIRS: [&str; 4] = [".git", ".hg", ".jj", ".svn"];

/// Мусор, который не нужен ни в поиске файла, ни в поиске по проекту.
const JUNK_FILES: [&str; 1] = [".DS_Store"];

/// Итог обхода.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkSummary {
    /// Сколько файлов отдано.
    pub files: usize,
    /// Остановились на [`MAX_FILES`].
    pub truncated: bool,
    /// Остановились по флагу отмены.
    pub cancelled: bool,
}

/// Корень проекта для каталога `start`: ближайший предок (включая сам `start`), где есть
/// `.git` (каталог или файл — у git worktree и подмодулей это файл), `.hg`, `.jj` или `.svn`.
pub fn find_vcs_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| VCS_DIRS.iter().any(|vcs| dir.join(vcs).exists()))
        .map(Path::to_path_buf)
}

/// Обходит файлы проекта параллельно. Учитываются `.gitignore` (внутри репозитория git),
/// `.ignore`, глобальный gitignore и `.git/info/exclude`, в том числе из каталогов выше
/// `root`. Скрытые файлы показываются; каталоги `.git`, `.hg`, `.jj`, `.svn` и файлы
/// `.DS_Store` — нет. Отдаются только обычные файлы: симлинки пропускаются (и не
/// разворачиваются). `f` вызывается из рабочих потоков с путём относительно `root`.
/// Обход останавливается по `cancel` или после [`MAX_FILES`] файлов.
pub fn walk_files(root: &Path, cancel: &AtomicBool, f: impl Fn(&Path) + Sync) -> WalkSummary {
    walk_files_limited(root, cancel, MAX_FILES, f)
}

pub(crate) fn walk_files_limited(
    root: &Path,
    cancel: &AtomicBool,
    limit: usize,
    f: impl Fn(&Path) + Sync,
) -> WalkSummary {
    let files = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);
    let cancelled = AtomicBool::new(false);
    walker(root, None).build_parallel().run(|| {
        let (f, files, truncated, cancelled) = (&f, &files, &truncated, &cancelled);
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) {
                cancelled.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }
            let Some(entry) = file_entry(entry) else {
                return WalkState::Continue;
            };
            if files.fetch_add(1, Ordering::Relaxed) >= limit {
                truncated.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }
            f(relative(root, entry.path()));
            WalkState::Continue
        })
    });
    WalkSummary {
        files: files.into_inner().min(limit),
        truncated: truncated.into_inner(),
        cancelled: cancelled.into_inner(),
    }
}

/// Общие правила обхода для поиска файла и поиска по проекту. `max_file_bytes` —
/// файлы крупнее пропускаются.
pub(crate) fn walker(root: &Path, max_file_bytes: Option<u64>) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        // Глобальный gitignore применяется относительно корня проекта, а не каталога,
        // из которого запущен редактор (из Finder это `/`).
        .current_dir(root)
        .max_filesize(max_file_bytes)
        .filter_entry(|entry| {
            let name = entry.file_name();
            !VCS_DIRS.iter().chain(&JUNK_FILES).any(|skip| name == *skip)
        });
    builder
}

/// Обычный файл из обхода; ошибки чтения каталогов и всё, кроме файлов, — `None`.
pub(crate) fn file_entry(entry: Result<DirEntry, ignore::Error>) -> Option<DirEntry> {
    let entry = entry.ok()?;
    entry
        .file_type()
        .is_some_and(|kind| kind.is_file())
        .then_some(entry)
}

pub(crate) fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// Дерево файлов во временном каталоге: `(путь, содержимое)`.
    pub(crate) fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        dir
    }

    fn walk(root: &Path) -> Vec<String> {
        let found = Mutex::new(Vec::new());
        let summary = walk_files(root, &AtomicBool::new(false), |path| {
            found
                .lock()
                .unwrap()
                .push(path.to_string_lossy().into_owned());
        });
        let mut found = found.into_inner().unwrap();
        found.sort();
        assert_eq!(summary.files, found.len());
        assert!(!summary.truncated && !summary.cancelled);
        found
    }

    #[test]
    fn respects_gitignore_and_shows_hidden_files() {
        let dir = tree(&[
            (".git/HEAD", "ref: refs/heads/main"),
            (".git/config", ""),
            (".gitignore", "/target\n*.log\n"),
            (".github/workflows/ci.yml", ""),
            (".env", ""),
            (".DS_Store", ""),
            ("src/main.rs", ""),
            ("src/.DS_Store", ""),
            ("src/debug.log", ""),
            ("target/debug/flux", ""),
            ("crates/a/.gitignore", "generated.rs\n"),
            ("crates/a/generated.rs", ""),
            ("crates/a/lib.rs", ""),
        ]);
        assert_eq!(
            walk(dir.path()),
            [
                ".env",
                ".github/workflows/ci.yml",
                ".gitignore",
                "crates/a/.gitignore",
                "crates/a/lib.rs",
                "src/main.rs",
            ]
        );
    }

    #[test]
    fn gitignore_needs_a_repository() {
        // Как у git и ripgrep: вне репозитория .gitignore не действует, а .ignore — да.
        let dir = tree(&[
            (".gitignore", "a.txt\n"),
            (".ignore", "b.txt\n"),
            ("a.txt", ""),
            ("b.txt", ""),
        ]);
        assert_eq!(walk(dir.path()), [".gitignore", ".ignore", "a.txt"]);
    }

    #[test]
    fn vcs_dirs_and_files_are_skipped() {
        let dir = tree(&[
            (".hg/store", ""),
            (".jj/repo", ""),
            (".svn/entries", ""),
            ("sub/.git", "gitdir: ../.git/worktrees/sub"),
            ("sub/file.txt", ""),
        ]);
        assert_eq!(walk(dir.path()), ["sub/file.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped() {
        let dir = tree(&[("real/file.txt", "")]);
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("real/file.txt"),
            dir.path().join("file-link.txt"),
        )
        .unwrap();
        assert_eq!(walk(dir.path()), ["real/file.txt"]);
    }

    #[test]
    fn walk_stops_at_the_limit_and_on_cancel() {
        let files: Vec<(String, &str)> = (0..50).map(|i| (format!("f{i}.txt"), "")).collect();
        let files: Vec<(&str, &str)> = files.iter().map(|(p, c)| (p.as_str(), *c)).collect();
        let dir = tree(&files);
        let count = AtomicUsize::new(0);
        let summary = walk_files_limited(dir.path(), &AtomicBool::new(false), 10, |_| {
            count.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(count.into_inner(), 10);
        assert_eq!(summary.files, 10);
        assert!(summary.truncated);

        let summary = walk_files(dir.path(), &AtomicBool::new(true), |_| {
            panic!("cancelled walk must not report files")
        });
        assert!(summary.cancelled);
        assert_eq!(summary.files, 0);
    }

    #[test]
    fn vcs_root_is_the_nearest_marked_ancestor() {
        let dir = tree(&[
            ("repo/.git/HEAD", ""),
            ("repo/src/deep/file.rs", ""),
            ("repo/worktree/.git", "gitdir: ../.git/worktrees/w"),
            ("repo/worktree/src/lib.rs", ""),
            ("plain/sub/file.txt", ""),
        ]);
        let root = dir.path();
        let repo = root.join("repo");
        assert_eq!(find_vcs_root(&repo.join("src/deep")), Some(repo.clone()));
        assert_eq!(find_vcs_root(&repo), Some(repo.clone()));
        assert_eq!(
            find_vcs_root(&repo.join("worktree/src")),
            Some(repo.join("worktree"))
        );
        // Временный каталог может лежать внутри чужого репозитория — сравниваем с тем,
        // что найдётся от самого временного каталога.
        assert_eq!(find_vcs_root(&root.join("plain/sub")), find_vcs_root(root));
    }
}
