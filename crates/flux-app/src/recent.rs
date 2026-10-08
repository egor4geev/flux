//! Недавние проекты: каталоги, последние — первыми. Хранятся между запусками в файле
//! `~/Library/Application Support/flux/recent-projects` (путь на строку).
//!
//! Путь файла переопределяет `FLUX_RECENT_FILE`. В сценариях проверки (`FLUX_SCENARIO`)
//! без него список не читается и не пишется: прогоны агентов не попадают в список автора.
//! Ошибки ввода-вывода не мешают работе — список просто не сохранится (сообщение в stderr).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Сколько проектов помнить.
pub const MAX_RECENT: usize = 10;

/// Недавние проекты, последние — первыми; исчезнувшие каталоги пропускаются.
pub fn load() -> Vec<PathBuf> {
    store_path().map_or_else(Vec::new, |file| load_from(&file))
}

/// Запоминает проект первым в списке; возвращает новый список (исчезнувшие каталоги в нём
/// пропущены, но в файле остаются: том мог быть просто не подключён).
pub fn record(root: &Path) -> Vec<PathBuf> {
    match store_path() {
        Some(file) => record_in(&file, root),
        None => vec![root.to_path_buf()],
    }
}

fn store_path() -> Option<PathBuf> {
    store_path_for(
        std::env::var_os("FLUX_RECENT_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        std::env::var_os("HOME"),
    )
}

/// Где хранится список: явный файл, иначе (не в сценарии) — в Application Support.
fn store_path_for(
    recent_file: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = recent_file.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    let home = PathBuf::from(home?);
    Some(home.join("Library/Application Support/flux/recent-projects"))
}

fn load_from(file: &Path) -> Vec<PathBuf> {
    read(file).into_iter().filter(|dir| dir.is_dir()).collect()
}

fn record_in(file: &Path, root: &Path) -> Vec<PathBuf> {
    let list = push_front(read(file), root);
    if let Err(error) = write(file, &list) {
        eprintln!("flux: {}: {error}", file.display());
    }
    list.into_iter()
        .filter(|dir| dir == root || dir.is_dir())
        .collect()
}

fn read(file: &Path) -> Vec<PathBuf> {
    match fs::read_to_string(file) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            eprintln!("flux: {}: {error}", file.display());
            Vec::new()
        }
    }
}

/// Файл → список: путь на строку; пустые строки, относительные пути и повторы
/// пропускаются, лишнее сверх [`MAX_RECENT`] отбрасывается.
fn parse(text: &str) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let path = Path::new(line.trim_end_matches('\r'));
        if path.is_absolute() && !list.iter().any(|known| known == path) {
            list.push(path.to_path_buf());
        }
    }
    list.truncate(MAX_RECENT);
    list
}

/// `root` — первым; его прежнее место освобождается; длина — не больше [`MAX_RECENT`].
fn push_front(mut list: Vec<PathBuf>, root: &Path) -> Vec<PathBuf> {
    list.retain(|known| known != root);
    list.insert(0, root.to_path_buf());
    list.truncate(MAX_RECENT);
    list
}

/// Список → текст файла. Пути, которые не записать строкой (не UTF-8, с переводом
/// строки), пропускаются — иначе при чтении получился бы другой путь.
fn serialize(list: &[PathBuf]) -> String {
    list.iter()
        .filter_map(|path| path.to_str())
        .filter(|path| !path.contains(['\n', '\r']))
        .map(|path| format!("{path}\n"))
        .collect()
}

/// Запись через временный файл рядом и `rename`: файл никогда не бывает недописанным.
/// Имя временного файла — с номером процесса: два окна flux не пишут в один временный файл.
fn write(file: &Path, list: &[PathBuf]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = file.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    fs::write(&temp, serialize(list))?;
    fs::rename(&temp, file).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    /// Свой временный каталог на тест: тесты идут параллельно.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("flux-recent-test-{}-{name}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_skips_blanks_relative_paths_and_repeats() {
        let text = "/a/one\n\nrelative/two\n/a/one\r\n/b/three\n";
        assert_eq!(parse(text), paths(&["/a/one", "/b/three"]));
        let many: String = (0..MAX_RECENT + 5).map(|n| format!("/p/{n}\n")).collect();
        assert_eq!(parse(&many).len(), MAX_RECENT);
    }

    #[test]
    fn recorded_project_moves_to_the_front() {
        let list = paths(&["/a", "/b", "/c"]);
        assert_eq!(
            push_front(list.clone(), Path::new("/c")),
            paths(&["/c", "/a", "/b"])
        );
        assert_eq!(
            push_front(list, Path::new("/new")),
            paths(&["/new", "/a", "/b", "/c"])
        );
        let full: Vec<PathBuf> = (0..MAX_RECENT).map(|n| format!("/p/{n}").into()).collect();
        let list = push_front(full, Path::new("/newest"));
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], Path::new("/newest"));
        assert_eq!(
            list.last().unwrap(),
            Path::new(&format!("/p/{}", MAX_RECENT - 2))
        );
    }

    #[test]
    fn unwritable_paths_are_left_out() {
        let list = paths(&["/a", "/with\nnewline", "/b"]);
        assert_eq!(serialize(&list), "/a\n/b\n");
    }

    #[test]
    fn store_path_honours_override_and_scenarios() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            store_path_for(None, false, home.clone()),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/recent-projects"
            ))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(Some("/tmp/r".into()), true, home.clone()),
            Some(PathBuf::from("/tmp/r"))
        );
        assert_eq!(
            store_path_for(Some(OsString::new()), false, home),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/flux/recent-projects"
            ))
        );
        assert_eq!(store_path_for(None, false, None), None);
    }

    #[test]
    fn record_writes_the_file_and_load_reads_existing_dirs() {
        let dir = temp_dir("roundtrip");
        let (one, two) = (dir.join("one"), dir.join("two"));
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        // Каталог хранилища создаётся при первой записи.
        let file = dir.join("store/recent-projects");

        assert_eq!(record_in(&file, &one), vec![one.clone()]);
        assert_eq!(record_in(&file, &two), vec![two.clone(), one.clone()]);
        assert_eq!(record_in(&file, &one), vec![one.clone(), two.clone()]);
        assert_eq!(load_from(&file), vec![one.clone(), two.clone()]);

        // Исчезнувший каталог не показывается, но остаётся в файле.
        fs::remove_dir_all(&two).unwrap();
        assert_eq!(load_from(&file), vec![one.clone()]);
        assert_eq!(read(&file), vec![one.clone(), two.clone()]);
        // Временных файлов не осталось.
        let leftovers = fs::read_dir(dir.join("store")).unwrap().count();
        assert_eq!(leftovers, 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_empty_list() {
        let dir = temp_dir("missing");
        assert!(load_from(&dir.join("nothing")).is_empty());
        fs::remove_dir_all(&dir).ok();
    }
}
