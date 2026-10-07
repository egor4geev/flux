//! Нечёткий поиск (fuzzy) через nucleo:
//! - небольшие списки (палитра команд — сотни строк) — синхронно, [`match_list`];
//! - большие списки путей (поиск файла — десятки тысяч) — [`PathMatcher`]: nucleo
//!   сопоставляет в своих потоках, а список пополняется прямо во время обхода.
//!
//! Синтаксис запроса — как в fzf: слова через пробел ищутся независимо, `'слово` — точное
//! вхождение, `^начало`, `конец$`, `!исключить`. Регистр «умный»: заглавная буква в слове
//! делает это слово чувствительным к регистру.

use std::path::Path;
use std::sync::Arc;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Injector, Matcher, Nucleo, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;

/// Совпадение элемента списка с запросом.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Индекс элемента во входном списке.
    pub index: usize,
    /// Чем больше, тем лучше совпадение.
    pub score: u32,
    /// Совпавшие символы — индексы `char` в строке элемента, по возрастанию, без повторов.
    pub positions: Vec<usize>,
}

/// Сопоставляет запрос со списком строк. Результат — по убыванию оценки, при равной оценке —
/// в исходном порядке. Пустой запрос — все элементы по порядку, без позиций.
pub fn match_list<S: AsRef<str>>(query: &str, items: &[S]) -> Vec<FuzzyMatch> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() {
        return (0..items.len())
            .map(|index| FuzzyMatch {
                index,
                score: 0,
                positions: Vec::new(),
            })
            .collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut matches: Vec<FuzzyMatch> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let item = item.as_ref();
            indices.clear();
            let score =
                pattern.indices(Utf32Str::new(item, &mut buf), &mut matcher, &mut indices)?;
            Some(FuzzyMatch {
                index,
                score,
                positions: char_positions(item, &mut indices, Haystack::Str),
            })
        })
        .collect();
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    matches
}

/// Совпадение пути с запросом [`PathMatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMatch {
    /// Путь относительно корня проекта, разделитель — `/`.
    pub path: Arc<str>,
    /// Совпавшие символы — индексы `char` в `path`, по возрастанию, без повторов.
    pub positions: Vec<usize>,
}

/// Нечёткий поиск по большому списку путей (поиск файла). Пути добавляются через
/// [`PathInjector`] из любых потоков, сопоставление идёт в потоках nucleo; результаты
/// забирает [`tick`](Self::tick) — его стоит звать после каждого `notify` и после
/// смены запроса. Оценка учитывает границы `/` (как у путей в fzf).
pub struct PathMatcher {
    nucleo: Nucleo<Arc<str>>,
    /// Для позиций совпавших символов у показанных строк.
    matcher: Matcher,
    query: String,
    running: bool,
}

/// Пополнение [`PathMatcher`]. Дёшево клонируется и отправляется в другие потоки; пока
/// жив хотя бы один клон, [`PathMatcher::is_running`] считает, что список ещё растёт.
#[derive(Clone)]
pub struct PathInjector {
    injector: Injector<Arc<str>>,
}

impl PathInjector {
    /// Добавляет путь относительно корня проекта.
    pub fn push(&self, relative: &Path) {
        let path = path_string(relative);
        self.injector.push(path, |path, columns| {
            columns[0] = path.as_ref().into();
        });
    }
}

impl PathMatcher {
    /// `notify` вызывается из рабочих потоков nucleo, когда пора вызвать [`tick`](Self::tick):
    /// появились новые пути или досчитались совпадения. Сам по себе он не ограничен по
    /// частоте — перерисовку стоит прореживать.
    pub fn new(notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            nucleo: Nucleo::new(Config::DEFAULT.match_paths(), notify, None, 1),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            query: String::new(),
            running: false,
        }
    }

    pub fn injector(&self) -> PathInjector {
        PathInjector {
            injector: self.nucleo.injector(),
        }
    }

    /// Новый запрос; сопоставление начнётся на ближайшем [`tick`](Self::tick). Если запрос
    /// только дописан в конец, nucleo перебирает лишь прошлые совпадения.
    pub fn set_query(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        // Дописанный запрос не расширяет выборку, кроме случаев, когда старый кончался
        // экранированием (`\`) или якорем конца (`$`): тогда меняется смысл последнего
        // слова. Отрицание (`!слово`) nucleo проверяет сам.
        let append = query.starts_with(&self.query)
            && !self.query.ends_with('\\')
            && !self.query.ends_with('$');
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        self.query = query.to_string();
    }

    /// Забирает готовые результаты, не дожидаясь потоков. `true` — список совпадений
    /// изменился.
    pub fn tick(&mut self) -> bool {
        let status = self.nucleo.tick(0);
        self.running = status.running;
        status.changed
    }

    /// Ещё не всё готово (по последнему [`tick`](Self::tick)): сопоставление идёт или
    /// кто-то держит [`PathInjector`] — обход продолжается.
    pub fn is_running(&self) -> bool {
        self.running || self.nucleo.active_injectors() > 0
    }

    /// Сколько всего путей (по последнему [`tick`](Self::tick)).
    pub fn item_count(&self) -> usize {
        self.nucleo.snapshot().item_count() as usize
    }

    /// Сколько путей подходит под запрос (по последнему [`tick`](Self::tick)).
    pub fn match_count(&self) -> usize {
        self.nucleo.snapshot().matched_item_count() as usize
    }

    /// `n`-е совпадение по убыванию оценки (при пустом запросе — в порядке добавления)
    /// с позициями совпавших символов. Позиции считаются здесь, поэтому брать стоит только
    /// видимые строки.
    pub fn get(&mut self, n: usize) -> Option<PathMatch> {
        let snapshot = self.nucleo.snapshot();
        let item = snapshot.get_matched_item(u32::try_from(n).ok()?)?;
        let mut indices = Vec::new();
        snapshot.pattern().column_pattern(0).indices(
            item.matcher_columns[0].slice(..),
            &mut self.matcher,
            &mut indices,
        );
        let path = item.data.clone();
        let positions = char_positions(&path, &mut indices, Haystack::String);
        Some(PathMatch { path, positions })
    }
}

/// Путь строкой с `/` между компонентами — и для показа, и для сопоставления.
fn path_string(path: &Path) -> Arc<str> {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        path.into()
    } else {
        path.replace(std::path::MAIN_SEPARATOR, "/").into()
    }
}

/// Как nucleo представил строку, по которой считал позиции.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Haystack {
    /// `Utf32Str::new`: ASCII — побайтно; иначе — по графемам (первый символ графемы),
    /// но если первые символы всех графем — ASCII, снова побайтно.
    Str,
    /// `Utf32String::from`: ASCII — побайтно, иначе — всегда по графемам.
    String,
}

/// Индексы nucleo → индексы `char` в `text`, по возрастанию, без повторов.
fn char_positions(text: &str, indices: &mut Vec<u32>, haystack: Haystack) -> Vec<usize> {
    indices.sort_unstable();
    indices.dedup();
    if text.is_ascii() {
        return indices.iter().map(|&i| i as usize).collect();
    }
    let graphemes: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
    let by_bytes = haystack == Haystack::Str
        && graphemes
            .iter()
            .all(|(_, g)| g.chars().next().is_some_and(|c| c.is_ascii()));
    let char_at_byte = |byte: usize| text[..byte.min(text.len())].chars().count();
    indices
        .iter()
        .filter_map(|&i| {
            let i = i as usize;
            if by_bytes {
                text.is_char_boundary(i).then(|| char_at_byte(i))
            } else {
                graphemes.get(i).map(|&(byte, _)| char_at_byte(byte))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    fn labels(query: &str, items: &[&str]) -> Vec<String> {
        match_list(query, items)
            .into_iter()
            .map(|m| items[m.index].to_string())
            .collect()
    }

    #[test]
    fn empty_query_keeps_everything_in_order() {
        let items = ["b", "a", "c"];
        let matches = match_list("  ", &items);
        assert_eq!(
            matches.iter().map(|m| m.index).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(matches.iter().all(|m| m.positions.is_empty()));
    }

    #[test]
    fn filters_and_ranks_by_score() {
        let items = [
            "editor: move word left",
            "workspace: close tab",
            "editor: move left",
        ];
        assert_eq!(
            labels("mvleft", &items),
            ["editor: move left", "editor: move word left"]
        );
        assert_eq!(labels("close", &items), ["workspace: close tab"]);
        assert!(labels("zzz", &items).is_empty());
    }

    #[test]
    fn words_match_independently_and_smart_case() {
        let items = ["Save File", "save all", "open file"];
        assert_eq!(labels("file save", &items), ["Save File"]);
        assert_eq!(labels("Save", &items), ["Save File"]);
        assert_eq!(labels("save", &items).len(), 2);
    }

    #[test]
    fn positions_are_char_indices() {
        let m = &match_list("ml", &["move left"])[0];
        assert_eq!(m.positions, [0, 5]);
        // Кириллица: nucleo считает графемы, у нас — символы.
        let m = &match_list("пф", &["путь к файлу"])[0];
        assert_eq!(m.positions, [0, 7]);
        // Эмодзи из нескольких символов перед совпадением.
        let m = &match_list("ok", &["👍🏽 ok"])[0];
        assert_eq!(m.positions, [3, 4]);
        // Комбинируемый акцент: первые символы графем — ASCII, nucleo считает байты.
        let m = &match_list("ex", &["e\u{301}x"])[0];
        assert_eq!(m.positions, [0, 2]);
    }

    /// Ждёт, пока сопоставление и пополнение закончатся.
    fn settle(matcher: &mut PathMatcher) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            matcher.tick();
            if !matcher.is_running() {
                return;
            }
            assert!(Instant::now() < deadline, "PathMatcher never settled");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn paths(matcher: &mut PathMatcher) -> Vec<String> {
        (0..matcher.match_count())
            .map(|n| matcher.get(n).unwrap().path.to_string())
            .collect()
    }

    #[test]
    fn path_matcher_fills_from_threads_and_settles() {
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = notified.clone();
        let mut matcher = PathMatcher::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let injector = matcher.injector();
        let workers: Vec<_> = (0..4)
            .map(|t| {
                let injector = injector.clone();
                thread::spawn(move || {
                    for i in 0..2_500 {
                        injector.push(Path::new(&format!("dir{t}/file{i}.rs")));
                    }
                })
            })
            .collect();
        drop(injector);
        assert!(matcher.is_running(), "injectors are still alive");
        for worker in workers {
            worker.join().unwrap();
        }
        settle(&mut matcher);
        assert_eq!(matcher.item_count(), 10_000);
        assert_eq!(matcher.match_count(), 10_000, "empty query matches all");
        assert!(notified.load(Ordering::Relaxed) > 0);

        matcher.set_query("dir3/file2499");
        settle(&mut matcher);
        assert_eq!(matcher.get(0).unwrap().path.as_ref(), "dir3/file2499.rs");
        assert!(matcher.get(matcher.match_count()).is_none());
    }

    #[test]
    fn path_matcher_ranks_and_reports_char_positions() {
        let mut matcher = PathMatcher::new(Arc::new(|| {}));
        let injector = matcher.injector();
        for path in [
            "crates/flux-app/src/editor.rs",
            "crates/flux-app/src/element.rs",
            "docs/заметки/редактор.md",
            "README.md",
        ] {
            injector.push(Path::new(path));
        }
        drop(injector);
        settle(&mut matcher);
        assert_eq!(
            paths(&mut matcher),
            [
                "crates/flux-app/src/editor.rs",
                "crates/flux-app/src/element.rs",
                "docs/заметки/редактор.md",
                "README.md",
            ],
            "empty query keeps insertion order"
        );

        matcher.set_query("edrs");
        settle(&mut matcher);
        let first = matcher.get(0).unwrap();
        assert_eq!(first.path.as_ref(), "crates/flux-app/src/editor.rs");
        assert_eq!(first.positions.len(), 4);

        matcher.set_query("ed");
        settle(&mut matcher);
        let mut found = paths(&mut matcher);
        found.sort();
        assert_eq!(found, ["README.md", "crates/flux-app/src/editor.rs"]);
        // Дописали запрос — nucleo перебирает только прошлые совпадения.
        matcher.set_query("edit");
        settle(&mut matcher);
        assert_eq!(paths(&mut matcher), ["crates/flux-app/src/editor.rs"]);
        // Запрос стал короче — снова шире.
        matcher.set_query("e");
        settle(&mut matcher);
        assert_eq!(matcher.match_count(), 3);
        // `$` — точный конец пути, не нечёткий.
        matcher.set_query("md$");
        settle(&mut matcher);
        assert_eq!(matcher.match_count(), 2);

        // Кириллица в пути: позиции — индексы символов, а не графем или байтов.
        matcher.set_query("редmd");
        settle(&mut matcher);
        let found = matcher.get(0).unwrap();
        assert_eq!(found.path.as_ref(), "docs/заметки/редактор.md");
        let chars: Vec<char> = found.path.chars().collect();
        let picked: String = found.positions.iter().map(|&i| chars[i]).collect();
        assert_eq!(picked, "редmd");
    }

    #[test]
    fn negated_append_rescores() {
        let mut matcher = PathMatcher::new(Arc::new(|| {}));
        let injector = matcher.injector();
        for path in ["a.rs", "ab.rs", "abc.rs"] {
            injector.push(Path::new(path));
        }
        drop(injector);
        matcher.set_query("!ab");
        settle(&mut matcher);
        assert_eq!(paths(&mut matcher), ["a.rs"]);
        // «!abc» исключает меньше, чем «!ab», — дописанный запрос расширил выборку.
        matcher.set_query("!abc");
        settle(&mut matcher);
        let mut found = paths(&mut matcher);
        found.sort();
        assert_eq!(found, ["a.rs", "ab.rs"]);
    }
}
