//! Наблюдение за файлами проекта: на macOS — FSEvents (через `notify`), рекурсивно по
//! всему корню одним потоком событий, без отдельного наблюдателя на каждый каталог.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::rules::is_skipped;

/// Что изменилось на диске.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsChange {
    /// Изменились (созданы, удалены, переименованы — старое и новое имя, записаны) эти
    /// пути — абсолютные, с тем же префиксом, что и корень, данный [`Watcher::new`].
    /// Событий внутри служебных каталогов VCS (`.git/…`) и про мусор (`.DS_Store`) здесь нет;
    /// `.gitignore` и `.ignore` — есть (по ним пересчитываются исключённые).
    Paths(Vec<PathBuf>),
    /// События потерялись (переполнение очереди ОС, ошибка наблюдения) — перечитать всё,
    /// что показано.
    Rescan,
}

/// Наблюдатель за корнем проекта; наблюдение прекращается, когда его уничтожают.
pub struct Watcher {
    _watcher: RecommendedWatcher,
}

impl Watcher {
    /// Наблюдает за `root` рекурсивно. `on_change` зовётся из потока наблюдателя на каждое
    /// событие ОС (часто — по несколько в миллисекунду) — склеивать вызовы должен получатель.
    pub fn new(root: &Path, on_change: impl Fn(FsChange) + Send + 'static) -> io::Result<Self> {
        let paths = RootPaths::new(root);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let change = match event {
                    Ok(event) if event.need_rescan() => FsChange::Rescan,
                    Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
                    Ok(event) => {
                        let changed: Vec<PathBuf> = event
                            .paths
                            .iter()
                            .filter_map(|path| paths.project_path(path))
                            .collect();
                        if changed.is_empty() {
                            return;
                        }
                        FsChange::Paths(changed)
                    }
                    Err(_) => FsChange::Rescan,
                };
                on_change(change);
            })
            .map_err(io::Error::other)?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(io::Error::other)?;
        Ok(Self { _watcher: watcher })
    }
}

/// Перевод путей событий к корню, каким его дали: FSEvents сообщает настоящие пути
/// (`/private/tmp/x/a.rs` при корне `/tmp/x`).
struct RootPaths {
    root: PathBuf,
    canonical: Option<PathBuf>,
}

impl RootPaths {
    fn new(root: &Path) -> Self {
        let canonical = fs::canonicalize(root).ok().filter(|real| real != root);
        Self {
            root: root.to_path_buf(),
            canonical,
        }
    }

    /// Путь события внутри проекта — с префиксом `root`; вне корня, внутри `.git` и про
    /// `.DS_Store` — `None`.
    fn project_path(&self, path: &Path) -> Option<PathBuf> {
        let rest = path.strip_prefix(&self.root).ok().or_else(|| {
            let canonical = self.canonical.as_deref()?;
            path.strip_prefix(canonical).ok()
        })?;
        if rest.iter().any(is_skipped) {
            return None;
        }
        Some(self.root.join(rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn event_paths_are_mapped_to_the_given_root() {
        let paths = RootPaths {
            root: PathBuf::from("/tmp/p"),
            canonical: Some(PathBuf::from("/private/tmp/p")),
        };
        let mapped = |path: &str| paths.project_path(Path::new(path));
        assert_eq!(
            mapped("/private/tmp/p/src/a.rs"),
            Some(PathBuf::from("/tmp/p/src/a.rs"))
        );
        assert_eq!(mapped("/tmp/p/a.rs"), Some(PathBuf::from("/tmp/p/a.rs")));
        assert_eq!(mapped("/tmp/p"), Some(PathBuf::from("/tmp/p")));
        assert_eq!(
            mapped("/tmp/p/.gitignore"),
            Some(PathBuf::from("/tmp/p/.gitignore"))
        );
        assert_eq!(mapped("/tmp/p/.git/index"), None);
        assert_eq!(mapped("/tmp/p/sub/.hg/store"), None);
        assert_eq!(mapped("/tmp/p/.DS_Store"), None);
        assert_eq!(mapped("/tmp/other/a.rs"), None);
        // Служебные имена выше корня не мешают.
        let paths = RootPaths {
            root: PathBuf::from("/x/.git/worktree"),
            canonical: None,
        };
        assert_eq!(
            paths.project_path(Path::new("/x/.git/worktree/a.rs")),
            Some(PathBuf::from("/x/.git/worktree/a.rs"))
        );
    }

    /// Ждёт, пока среди пришедших путей не окажутся все `expected` (не дольше 5 с).
    fn wait_for(events: &mpsc::Receiver<FsChange>, seen: &mut Vec<PathBuf>, expected: &[&Path]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !expected.iter().all(|path| seen.iter().any(|p| p == path)) {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(FsChange::Paths(paths)) => seen.extend(paths),
                Ok(FsChange::Rescan) => {}
                Err(_) => panic!("no events for {expected:?}; got {seen:?}"),
            }
        }
    }

    #[test]
    fn changes_on_disk_are_reported_under_the_given_root() {
        // Временный каталог — `/var/folders/…`, а FSEvents сообщает `/private/var/…`.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir(root.join(".git")).unwrap();
        let (sender, events) = mpsc::channel();
        let _watcher = Watcher::new(&root, move |change| {
            sender.send(change).ok();
        })
        .unwrap();
        // Поток событий FSEvents запускается не мгновенно.
        std::thread::sleep(Duration::from_millis(300));

        let mut seen = Vec::new();
        let created = root.join("new.rs");
        fs::write(&created, "x").unwrap();
        wait_for(&events, &mut seen, &[&created]);

        let renamed = root.join("renamed.rs");
        fs::rename(&created, &renamed).unwrap();
        wait_for(&events, &mut seen, &[&renamed]);

        fs::create_dir(root.join("sub")).unwrap();
        let nested = root.join("sub/deep.txt");
        fs::write(&nested, "y").unwrap();
        fs::write(root.join(".git/index"), "z").unwrap();
        let gitignore = root.join(".gitignore");
        fs::write(&gitignore, "*.log\n").unwrap();
        wait_for(&events, &mut seen, &[&nested, &gitignore]);

        fs::remove_file(&renamed).unwrap();
        seen.retain(|path| path != &renamed);
        wait_for(&events, &mut seen, &[&renamed]);

        assert!(seen.iter().all(|path| path.starts_with(&root)), "{seen:?}");
        assert!(
            !seen.iter().any(|path| path.iter().any(|c| c == ".git")),
            "{seen:?}"
        );
    }
}
