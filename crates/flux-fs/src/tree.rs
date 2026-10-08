//! Состояние дерева файлов проекта: какие каталоги прочитаны и раскрыты, видимые строки.
//! Ничего не читает с диска сам: списки каталогов приносит вызывающий ([`crate::list_dir`]
//! в фоне). Что дочитать, говорит [`FileTree::pending_loads`], что перечитать после
//! изменений на диске — [`FileTree::refresh_plan`].
//!
//! Каталоги известны по абсолютным путям. Прочитанный каталог помнит свой список, даже
//! когда свёрнут: раскрытие снова — без чтения, а наблюдение за диском держит список свежим.
//! Раскрытие вложенных каталогов сохраняется, пока свёрнут их родитель.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::list::{DirEntry, EntryKind};
use crate::ops::remap;
use crate::watch::FsChange;

/// Файлы правил: их изменение меняет флаг `ignored` у всего каталога.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// Видимая строка дерева.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    /// Вложенность: дети корня — 0.
    pub depth: usize,
    pub kind: EntryKind,
    pub ignored: bool,
    /// Каталог раскрыт; у файлов — `false`.
    pub expanded: bool,
}

#[derive(Debug, Default)]
struct Dir {
    /// Прочитанный список; `None` — ещё не прочитан.
    entries: Option<Vec<DirEntry>>,
    expanded: bool,
}

#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    /// Каталоги, о которых что-то известно: прочитанные или раскрытые. Корень есть всегда
    /// и всегда раскрыт.
    dirs: HashMap<PathBuf, Dir>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(
            root.clone(),
            Dir {
                entries: None,
                expanded: true,
            },
        );
        Self { root, dirs }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Видимые строки по порядку: дети раскрытых и прочитанных каталогов.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let Some(entries) = self.entries(dir) else {
            return;
        };
        for entry in entries {
            let path = dir.join(&entry.name);
            let expanded = entry.kind == EntryKind::Dir && self.is_expanded(&path);
            rows.push(Row {
                path: path.clone(),
                name: entry.name.clone(),
                depth,
                kind: entry.kind,
                ignored: entry.ignored,
                expanded,
            });
            if expanded {
                self.push_rows(&path, depth + 1, rows);
            }
        }
    }

    fn entries(&self, dir: &Path) -> Option<&Vec<DirEntry>> {
        self.dirs.get(dir)?.entries.as_ref()
    }

    /// Запись о `path` в списке его каталога; о корне записи нет.
    pub fn entry(&self, path: &Path) -> Option<&DirEntry> {
        let name = path.file_name()?.to_str()?;
        self.entries(path.parent()?)?
            .iter()
            .find(|entry| entry.name == name)
    }

    /// Каталог ли это: корень или каталог в списке своего родителя.
    pub fn is_dir(&self, path: &Path) -> bool {
        path == self.root || self.entry(path).is_some_and(|e| e.kind == EntryKind::Dir)
    }

    pub fn is_loaded(&self, dir: &Path) -> bool {
        self.entries(dir).is_some()
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.dirs.get(dir).is_some_and(|dir| dir.expanded)
    }

    /// Исключён ли `path` правилами `.gitignore` — по списку родителя; корень — нет.
    /// Для каталога это аргумент `dir_ignored` у [`crate::list_dir`] его детей.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.entry(path).is_some_and(|entry| entry.ignored)
    }

    /// Каталоги, которые видно (все предки раскрыты и прочитаны), но сами ещё не прочитаны, —
    /// с флагом `ignored` для `list_dir`. Родители — раньше детей.
    pub fn pending_loads(&self) -> Vec<(PathBuf, bool)> {
        let mut pending = Vec::new();
        self.collect_pending(&self.root, false, &mut pending);
        pending
    }

    fn collect_pending(&self, dir: &Path, ignored: bool, pending: &mut Vec<(PathBuf, bool)>) {
        let Some(entries) = self.entries(dir) else {
            pending.push((dir.to_path_buf(), ignored));
            return;
        };
        for entry in entries.iter().filter(|entry| entry.kind == EntryKind::Dir) {
            let path = dir.join(&entry.name);
            if self.is_expanded(&path) {
                self.collect_pending(&path, entry.ignored, pending);
            }
        }
    }

    /// Список каталога прочитан. Состояние оставшихся подкаталогов сохраняется, исчезнувших
    /// (и всего внутри них) — забывается. Возвращает прочитанные подкаталоги, у которых
    /// сменился флаг `ignored`: их нужно перечитать — с ним меняются флаги всего внутри.
    /// Список каталога, о котором дерево уже забыло, отбрасывается.
    pub fn set_listing(&mut self, dir: &Path, entries: Vec<DirEntry>) -> Vec<PathBuf> {
        let Some(state) = self.dirs.get_mut(dir) else {
            return Vec::new();
        };
        let old = state.entries.replace(entries);
        let children: Vec<PathBuf> = self
            .dirs
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .cloned()
            .collect();
        let mut stale = Vec::new();
        for child in children {
            let name = child.file_name().and_then(|name| name.to_str());
            let find = |entries: &[DirEntry]| {
                entries
                    .iter()
                    .find(|entry| Some(entry.name.as_str()) == name)
                    .cloned()
            };
            match self.entries(dir).and_then(|entries| find(entries)) {
                Some(entry) if entry.kind == EntryKind::Dir => {
                    let was = old.as_deref().and_then(find).map(|entry| entry.ignored);
                    if was.is_some_and(|was| was != entry.ignored) && self.is_loaded(&child) {
                        stale.push(child);
                    }
                }
                _ => self.forget(&child),
            }
        }
        stale
    }

    /// Забывает каталог и всё внутри него.
    fn forget(&mut self, dir: &Path) {
        if dir != self.root {
            self.dirs.retain(|path, _| !path.starts_with(dir));
        }
    }

    /// Раскрыть каталог; непрочитанный появится в [`Self::pending_loads`].
    pub fn expand(&mut self, dir: &Path) {
        if dir.starts_with(&self.root) {
            self.dirs.entry(dir.to_path_buf()).or_default().expanded = true;
        }
    }

    pub fn collapse(&mut self, dir: &Path) {
        if dir == self.root {
            return;
        }
        if let Some(state) = self.dirs.get_mut(dir) {
            state.expanded = false;
            // Раскрыть и тут же свернуть, не дождавшись чтения: помнить нечего.
            if state.entries.is_none() {
                self.dirs.remove(dir);
            }
        }
    }

    pub fn toggle(&mut self, dir: &Path) {
        if self.is_expanded(dir) {
            self.collapse(dir);
        } else {
            self.expand(dir);
        }
    }

    /// Свернуть все каталоги; прочитанные списки остаются.
    pub fn collapse_all(&mut self) {
        let root = self.root.clone();
        self.dirs.retain(|path, dir| {
            if *path != root {
                dir.expanded = false;
            }
            dir.entries.is_some() || *path == root
        });
    }

    /// Раскрыть каталоги от корня до `path` (сам `path` — нет): после их чтения строка `path`
    /// станет видна. `false` — путь вне корня.
    pub fn reveal(&mut self, path: &Path) -> bool {
        let Ok(rest) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut dir = self.root.clone();
        let mut components = rest.components().peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            dir.push(component);
            self.expand(&dir);
        }
        true
    }

    /// `from` переименован или перемещён в `to`: состояние каталогов внутри едет вместе с ним.
    /// В том же каталоге запись переименовывается на месте (порядок поправит перечитывание),
    /// из другого — убирается; новый родитель допишет её, когда его перечитают.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        if from == to || from == self.root {
            return;
        }
        let moved: Vec<PathBuf> = self
            .dirs
            .keys()
            .filter(|path| path.starts_with(from))
            .cloned()
            .collect();
        for old in moved {
            if let (Some(state), Some(new)) = (self.dirs.remove(&old), remap(&old, from, to)) {
                self.dirs.insert(new, state);
            }
        }
        let (Some(from_dir), Some(from_name)) =
            (from.parent(), from.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        let to_name = to.file_name().and_then(|name| name.to_str());
        let same_dir = to.parent() == Some(from_dir);
        if let Some(entries) = self.dirs.get_mut(from_dir).and_then(|d| d.entries.as_mut())
            && let Some(index) = entries.iter().position(|entry| entry.name == from_name)
        {
            match to_name.filter(|_| same_dir) {
                Some(name) => entries[index].name = name.to_string(),
                None => {
                    entries.remove(index);
                }
            }
        }
    }

    /// `path` удалён: убрать его из списка родителя и забыть каталоги внутри.
    pub fn remove(&mut self, path: &Path) {
        if path == self.root {
            return;
        }
        self.forget(path);
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            return;
        };
        if let Some(entries) = self.dirs.get_mut(dir).and_then(|d| d.entries.as_mut()) {
            entries.retain(|entry| entry.name != name);
        }
    }

    /// Прочитанные каталоги — родители раньше детей.
    pub fn loaded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter(|(_, dir)| dir.entries.is_some())
            .map(|(path, _)| path.clone())
            .collect();
        sort_parents_first(&mut dirs);
        dirs
    }

    /// Что перечитать после изменений на диске. Для каждого пути — его прочитанный каталог
    /// и сам путь, если это прочитанный каталог; изменился `.gitignore` или `.ignore` — ещё
    /// и все прочитанные каталоги внутри его каталога. `Rescan` — все прочитанные. Пути вне
    /// корня пропускаются. Родители — раньше детей.
    pub fn refresh_plan(&self, changes: &[FsChange]) -> Vec<PathBuf> {
        let mut plan = BTreeSet::new();
        for change in changes {
            let paths = match change {
                FsChange::Rescan => return self.loaded_dirs(),
                FsChange::Paths(paths) => paths,
            };
            for path in paths.iter().filter(|path| path.starts_with(&self.root)) {
                if self.is_loaded(path) {
                    plan.insert(path.clone());
                }
                let Some(dir) = path.parent().filter(|dir| self.is_loaded(dir)) else {
                    continue;
                };
                plan.insert(dir.to_path_buf());
                let rules = path
                    .file_name()
                    .is_some_and(|name| IGNORE_FILES.iter().any(|rule| name == *rule));
                if rules {
                    plan.extend(
                        self.loaded_dirs()
                            .into_iter()
                            .filter(|loaded| loaded.starts_with(dir)),
                    );
                }
            }
        }
        let mut plan: Vec<PathBuf> = plan.into_iter().collect();
        sort_parents_first(&mut plan);
        plan
    }

    /// Каталог для нового файла или вставки: выбранный каталог, каталог выбранного файла,
    /// без выбора — корень.
    pub fn target_dir(&self, selected: Option<&Path>) -> PathBuf {
        match selected {
            Some(path) if self.is_dir(path) => path.to_path_buf(),
            Some(path) => path
                .parent()
                .filter(|dir| dir.starts_with(&self.root))
                .unwrap_or(&self.root)
                .to_path_buf(),
            None => self.root.clone(),
        }
    }
}

/// Мельче — раньше: родитель перед ребёнком, порядок полный.
fn sort_parents_first(dirs: &mut [PathBuf]) {
    dirs.sort_by(|a, b| {
        let depth = |path: &PathBuf| path.components().count();
        depth(a).cmp(&depth(b)).then_with(|| a.cmp(b))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/p";

    fn path(rel: &str) -> PathBuf {
        if rel.is_empty() {
            PathBuf::from(ROOT)
        } else {
            Path::new(ROOT).join(rel)
        }
    }

    /// Список каталога: `name/` — каталог, `!` в начале — исключён `.gitignore`.
    fn listing(names: &[&str]) -> Vec<DirEntry> {
        names
            .iter()
            .map(|name| {
                let (ignored, name) = match name.strip_prefix('!') {
                    Some(rest) => (true, rest),
                    None => (false, *name),
                };
                let (kind, name) = match name.strip_suffix('/') {
                    Some(dir) => (EntryKind::Dir, dir),
                    None => (EntryKind::File, name),
                };
                DirEntry {
                    name: name.into(),
                    kind,
                    ignored,
                }
            })
            .collect()
    }

    /// Строки как «отступ + имя», каталоги — с `/`, раскрытые — с `/-`.
    fn shape(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|row| {
                let mark = match (row.kind, row.expanded) {
                    (EntryKind::Dir, true) => "/-",
                    (EntryKind::Dir, false) => "/",
                    (EntryKind::File, _) => "",
                };
                format!("{}{}{mark}", "  ".repeat(row.depth), row.name)
            })
            .collect()
    }

    fn tree() -> FileTree {
        let mut tree = FileTree::new(path(""));
        tree.set_listing(&path(""), listing(&["src/", "!target/", "Cargo.toml"]));
        tree
    }

    #[test]
    fn root_is_pending_until_read() {
        let tree = FileTree::new(path(""));
        assert_eq!(tree.pending_loads(), [(path(""), false)]);
        assert!(tree.rows().is_empty());
        let tree = self::tree();
        assert!(tree.pending_loads().is_empty());
        assert_eq!(shape(&tree), ["src/", "target/", "Cargo.toml"]);
    }

    #[test]
    fn expanded_dirs_show_children_and_load_with_their_ignored_flag() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.expand(&path("target"));
        assert_eq!(
            tree.pending_loads(),
            [(path("src"), false), (path("target"), true)]
        );
        // Раскрыт, но не прочитан — пока без детей.
        assert_eq!(shape(&tree), ["src/-", "target/-", "Cargo.toml"]);
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        assert_eq!(
            shape(&tree),
            ["src/-", "  app/", "  main.rs", "target/-", "Cargo.toml"]
        );
        let rows = tree.rows();
        assert_eq!(rows[2].path, path("src/main.rs"));
        assert_eq!(rows[2].depth, 1);
        assert!(rows[3].ignored);
    }

    #[test]
    fn collapsing_a_parent_keeps_nested_expansion() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        tree.collapse(&path("src"));
        assert_eq!(shape(&tree), ["src/", "target/", "Cargo.toml"]);
        // Свёрнутый, но прочитанный каталог не просит чтения.
        assert!(tree.pending_loads().is_empty());
        tree.toggle(&path("src"));
        assert_eq!(
            shape(&tree),
            [
                "src/-",
                "  app/-",
                "    lib.rs",
                "  main.rs",
                "target/",
                "Cargo.toml"
            ]
        );
    }

    #[test]
    fn collapse_all_keeps_listings() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["main.rs"]));
        tree.expand(&path("target"));
        tree.collapse_all();
        assert_eq!(shape(&tree), ["src/", "target/", "Cargo.toml"]);
        assert!(tree.is_loaded(&path("src")));
        // Раскрытый, но так и не прочитанный — забыт.
        assert!(!tree.dirs.contains_key(&path("target")));
        assert!(tree.pending_loads().is_empty());
    }

    #[test]
    fn relisting_forgets_vanished_dirs_and_keeps_the_rest() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "old/"]));
        tree.expand(&path("src/app"));
        tree.expand(&path("src/old"));
        tree.set_listing(&path("src/old"), listing(&["deep/"]));
        tree.expand(&path("src/old/deep"));
        tree.set_listing(&path("src"), listing(&["app/", "new.rs"]));
        assert!(tree.is_expanded(&path("src/app")));
        assert!(!tree.dirs.contains_key(&path("src/old")));
        assert!(!tree.dirs.contains_key(&path("src/old/deep")));
        // Каталог стал файлом с тем же именем — тоже забыт.
        tree.set_listing(&path("src"), listing(&["app"]));
        assert!(!tree.dirs.contains_key(&path("src/app")));
        // Список забытого каталога отбрасывается.
        assert!(
            tree.set_listing(&path("src/old"), listing(&["x"]))
                .is_empty()
        );
        assert!(!tree.is_loaded(&path("src/old")));
    }

    #[test]
    fn changed_ignored_flag_asks_to_reread_loaded_subdirs() {
        let mut tree = tree();
        tree.expand(&path("target"));
        tree.set_listing(&path("target"), listing(&["!debug/"]));
        tree.expand(&path("src"));
        // `target` больше не исключён (правку .gitignore откатили).
        let stale = tree.set_listing(&path(""), listing(&["src/", "target/", "Cargo.toml"]));
        assert_eq!(stale, [path("target")]);
        assert!(!tree.is_ignored(&path("target")));
        // `src` не прочитан — перечитывать нечего; флаг не менялся — тем более.
        let stale = tree.set_listing(&path(""), listing(&["src/", "target/", "Cargo.toml"]));
        assert!(stale.is_empty());
    }

    #[test]
    fn reveal_expands_ancestors_one_level_at_a_time() {
        let mut tree = tree();
        assert!(tree.reveal(&path("src/app/deep/lib.rs")));
        // Читать можно только `src`: о `src/app` неизвестно, пока не прочитан `src`.
        assert_eq!(tree.pending_loads(), [(path("src"), false)]);
        tree.set_listing(&path("src"), listing(&["app/"]));
        assert_eq!(tree.pending_loads(), [(path("src/app"), false)]);
        tree.set_listing(&path("src/app"), listing(&["!deep/"]));
        assert_eq!(tree.pending_loads(), [(path("src/app/deep"), true)]);
        tree.set_listing(&path("src/app/deep"), listing(&["!lib.rs"]));
        assert!(
            tree.rows()
                .iter()
                .any(|row| row.path == path("src/app/deep/lib.rs"))
        );
        // Сам файл не «раскрывается», вне корня — `false`.
        assert!(!tree.dirs.contains_key(&path("src/app/deep/lib.rs")));
        assert!(!tree.reveal(Path::new("/elsewhere/x.rs")));
        assert!(tree.reveal(&path("")));
    }

    #[test]
    fn reveal_through_a_missing_dir_stops_quietly() {
        let mut tree = tree();
        tree.reveal(&path("gone/file.rs"));
        // `gone` нет в списке корня — читать нечего, состояние забывается при перечитывании.
        assert!(tree.pending_loads().is_empty());
        tree.set_listing(&path(""), listing(&["src/"]));
        assert!(!tree.dirs.contains_key(&path("gone")));
    }

    #[test]
    fn rename_in_place_keeps_position_and_expansion() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        tree.rename(&path("src"), &path("source"));
        assert_eq!(
            shape(&tree),
            [
                "source/-",
                "  app/-",
                "    lib.rs",
                "  main.rs",
                "target/",
                "Cargo.toml"
            ]
        );
        assert!(tree.is_loaded(&path("source/app")));
        assert!(!tree.dirs.contains_key(&path("src")));
        tree.rename(&path("source/main.rs"), &path("source/lib.rs"));
        assert_eq!(tree.rows()[3].path, path("source/lib.rs"));
    }

    #[test]
    fn move_to_another_dir_takes_the_subtree_along() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        tree.rename(&path("src/app"), &path("target/app"));
        // Из `src` запись ушла; в `target` её допишет перечитывание.
        assert_eq!(shape(&tree), ["src/-", "target/", "Cargo.toml"]);
        assert!(tree.is_expanded(&path("target/app")));
        assert!(tree.is_loaded(&path("target/app")));
        tree.expand(&path("target"));
        tree.set_listing(&path("target"), listing(&["!app/"]));
        assert_eq!(
            shape(&tree),
            ["src/-", "target/-", "  app/-", "    lib.rs", "Cargo.toml"]
        );
    }

    #[test]
    fn remove_drops_the_row_and_the_subtree() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/"]));
        tree.expand(&path("src/app"));
        tree.remove(&path("src"));
        assert_eq!(shape(&tree), ["target/", "Cargo.toml"]);
        assert!(!tree.dirs.contains_key(&path("src/app")));
        tree.remove(&path(""));
        assert!(tree.is_loaded(&path("")));
    }

    #[test]
    fn refresh_plan_covers_loaded_parents_and_dirs() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["app/", "main.rs"]));
        tree.expand(&path("src/app"));
        tree.set_listing(&path("src/app"), listing(&["lib.rs"]));
        let paths = |paths: &[&str]| vec![FsChange::Paths(paths.iter().map(|p| path(p)).collect())];
        assert_eq!(tree.refresh_plan(&paths(&["src/main.rs"])), [path("src")]);
        // Сам прочитанный каталог и его родитель; родитель — первым.
        assert_eq!(
            tree.refresh_plan(&paths(&["src/app"])),
            [path("src"), path("src/app")]
        );
        // Непрочитанный каталог и пути вне корня — нечего перечитывать.
        assert!(tree.refresh_plan(&paths(&["target/debug/x"])).is_empty());
        assert!(
            tree.refresh_plan(&[FsChange::Paths(vec![PathBuf::from("/q/x")])])
                .is_empty()
        );
        // Правила `.gitignore` — каталог и всё прочитанное внутри.
        assert_eq!(
            tree.refresh_plan(&paths(&[".gitignore"])),
            [path(""), path("src"), path("src/app")]
        );
        assert_eq!(
            tree.refresh_plan(&paths(&["src/.ignore", "src/app/lib.rs"])),
            [path("src"), path("src/app")]
        );
        assert_eq!(
            tree.refresh_plan(&[FsChange::Paths(vec![]), FsChange::Rescan]),
            [path(""), path("src"), path("src/app")]
        );
    }

    #[test]
    fn target_dir_is_the_selected_dir_or_the_file_dir() {
        let mut tree = tree();
        tree.expand(&path("src"));
        tree.set_listing(&path("src"), listing(&["main.rs"]));
        assert_eq!(tree.target_dir(Some(&path("src"))), path("src"));
        assert_eq!(tree.target_dir(Some(&path("src/main.rs"))), path("src"));
        assert_eq!(tree.target_dir(Some(&path("Cargo.toml"))), path(""));
        assert_eq!(tree.target_dir(None), path(""));
    }
}
