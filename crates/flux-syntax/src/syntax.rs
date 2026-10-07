//! Дерево разбора документа как синхронный автомат состояний без потоков.
//!
//! Цикл жизни:
//! 1. [`Syntax::edit`] — на каждую правку, в UI-потоке: только `Tree::edit`,
//!    микросекунды. Дерево остаётся пригодным для подсветки, пока идёт разбор.
//! 2. [`Syntax::parse_job`] — снимок текста и дерева ([`ParseJob`], `Send + 'static`).
//! 3. [`ParseJob::run`] где угодно (обычно в фоновом executor'е) или
//!    [`ParseJob::run_with_budget`] прямо в UI-потоке с бюджетом ~1 мс.
//! 4. [`Syntax::finish`] — ставит новое дерево, доигрывая на нём правки,
//!    сделанные после старта работы.
//!
//! Одновременно идёт не больше одной работы: правки за время разбора копятся
//! и уходят в следующий разбор, поэтому длинный разбор не плодит очередь.

use std::ops::{ControlFlow, Range};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use flux_core::{ChangeSet, Rope};
use tree_sitter::{InputEdit, ParseOptions, ParseState, Parser, Point, Tree};

use crate::edit::input_edits;
use crate::highlight::{self, HighlightMap, HighlightSpan};
use crate::language::Language;
use crate::text::{chunk_from, count_newlines};

pub struct Syntax {
    language: &'static Language,
    /// Последнее дерево, отредактированное под текущий текст. Может быть
    /// устаревшим (правки после разбора), но смещения узлов уже сдвинуты.
    tree: Option<Tree>,
    /// Число `\n` в текущем тексте, если известно. При дереве известно всегда:
    /// по нему правки понимают, совпадают ли строки ropey со строками tree-sitter.
    newlines: Option<usize>,
    /// Текст изменился после снимка последней работы (или работ ещё не было).
    dirty: bool,
    /// Версия текста: число правок с момента создания.
    version: u64,
    job: Option<InFlight>,
    /// Парсер между работами — чтобы не выделять память заново на каждую.
    parser: Option<Parser>,
}

/// Работа, отданная приложению.
struct InFlight {
    /// Жива ли работа: ссылку держат [`ParseJob`], затем [`ParseResult`].
    token: Weak<()>,
    /// Версия текста в снимке.
    version: u64,
    /// Правки после снимка — доиграть на результат работы.
    pending: Vec<PendingEdit>,
}

enum PendingEdit {
    /// Посчитанные правки дерева (по возрастанию позиции).
    Ready(Vec<InputEdit>),
    /// Дерева ещё нет, число строк неизвестно: правки посчитает `finish`
    /// по числу строк из результата работы.
    Deferred { old_text: Rope, changes: ChangeSet },
}

impl Syntax {
    /// Пустое состояние: подсветки нет до первого разбора.
    pub fn new(language: &'static Language) -> Self {
        Self {
            language,
            tree: None,
            newlines: None,
            dirty: true,
            version: 0,
            job: None,
            parser: None,
        }
    }

    pub fn language(&self) -> &'static Language {
        self.language
    }

    /// Текущее дерево; после правок без разбора — отредактированное, устаревшее.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Идёт ли работа (выдана и ещё не завершена и не брошена).
    pub fn is_parsing(&self) -> bool {
        self.job.as_ref().is_some_and(InFlight::is_alive)
    }

    /// Вернёт ли [`Syntax::parse_job`] работу прямо сейчас.
    pub fn needs_parse(&self) -> bool {
        let pending = match &self.job {
            Some(job) if job.is_alive() => false,
            // Брошенная работа: её снимок так и не разобран.
            Some(_) => true,
            None => self.dirty,
        };
        pending && self.language.grammar().is_some()
    }

    /// Правка документа: `old_text` — текст до `changes`. Дёшево: только
    /// сдвигает узлы дерева. Если идёт разбор, правка запоминается, чтобы
    /// доиграть её на его результате.
    pub fn edit(&mut self, old_text: &Rope, changes: &ChangeSet) {
        if changes.is_empty() {
            return;
        }
        if changes.len() != old_text.len_chars() {
            // Правка не от этого текста: дереву больше верить нельзя.
            self.reset();
            return;
        }
        self.version += 1;
        self.dirty = true;
        self.forget_abandoned_job();
        if self.tree.is_none() && self.job.is_none() {
            // Править нечего: следующий разбор всё равно пойдёт с нуля.
            self.newlines = None;
            return;
        }
        match self.newlines {
            Some(newlines) => {
                let batch = input_edits(old_text, changes, newlines);
                if let Some(tree) = &mut self.tree {
                    apply_edits(tree, &batch.edits);
                }
                self.newlines = Some(batch.newlines);
                if let Some(job) = &mut self.job {
                    job.pending.push(PendingEdit::Ready(batch.edits));
                }
            }
            None => {
                // Число строк неизвестно, значит, дерева нет, а идёт первый разбор.
                debug_assert!(self.tree.is_none());
                if let Some(job) = &mut self.job {
                    job.pending.push(PendingEdit::Deferred {
                        old_text: old_text.clone(),
                        changes: changes.clone(),
                    });
                }
            }
        }
    }

    /// Работа по разбору `text` — текущего текста документа. `None`, если
    /// разбор не нужен или уже идёт (тогда после [`Syntax::finish`] спросите снова).
    pub fn parse_job(&mut self, text: &Rope) -> Option<ParseJob> {
        if self.is_parsing() {
            return None;
        }
        self.forget_abandoned_job();
        if !self.dirty {
            return None;
        }
        self.language.grammar()?;
        let token = Arc::new(());
        self.job = Some(InFlight {
            token: Arc::downgrade(&token),
            version: self.version,
            pending: Vec::new(),
        });
        self.dirty = false;
        Some(ParseJob {
            language: self.language,
            text: text.clone(),
            old_tree: self.tree.clone(),
            parser: self.parser.take(),
            newlines: self.newlines,
            token,
            version: self.version,
        })
    }

    /// Ставит результат работы, выданной этим `Syntax`, доигрывая правки,
    /// сделанные после её старта; если такие были, остаётся «нужен разбор».
    /// Чужой или устаревший (после [`Syntax::reset`]) результат отбрасывается:
    /// возвращается `false`.
    pub fn finish(&mut self, result: ParseResult) -> bool {
        let ParseResult {
            tree,
            parser,
            newlines,
            token,
            version,
        } = result;
        let Some(job) = self
            .job
            .take_if(|job| std::ptr::eq(job.token.as_ptr(), Arc::as_ptr(&token)))
        else {
            return false;
        };
        debug_assert_eq!(job.version, version);
        debug_assert_eq!(job.version + job.pending.len() as u64, self.version);
        self.parser.get_or_insert(parser);
        let Some(mut tree) = tree else {
            // Разбор не удался: остаётся прежнее (отредактированное) дерево.
            return true;
        };
        let mut newlines = newlines;
        for pending in job.pending {
            match pending {
                PendingEdit::Ready(edits) => apply_edits(&mut tree, &edits),
                PendingEdit::Deferred { old_text, changes } => {
                    let batch = input_edits(&old_text, &changes, newlines);
                    apply_edits(&mut tree, &batch.edits);
                    newlines = batch.newlines;
                }
            }
        }
        self.tree = Some(tree);
        self.newlines.get_or_insert(newlines);
        true
    }

    /// Забыть дерево: следующий разбор — с нуля, подсветки до него нет.
    /// Для правок, которые нельзя выразить через [`ChangeSet`] (файл
    /// перечитан с диска). Результат уже выданной работы будет отброшен.
    pub fn reset(&mut self) {
        self.tree = None;
        self.newlines = None;
        self.job = None;
        self.dirty = true;
        self.version += 1;
    }

    /// Подсветка строк `lines` (строки ropey): по вектору спанов на каждую
    /// существующую строку диапазона. Колонки — в символах внутри строки,
    /// без перевода строки. До первого разбора — пустые векторы.
    pub fn highlight_lines(
        &self,
        text: &Rope,
        lines: Range<usize>,
        map: &HighlightMap,
    ) -> Vec<Vec<HighlightSpan>> {
        let tree = self
            .tree
            .as_ref()
            .filter(|_| std::ptr::eq(map.language(), self.language));
        debug_assert!(
            std::ptr::eq(map.language(), self.language),
            "highlight map is for another language"
        );
        highlight::highlight_lines(tree, text, lines, map)
    }

    /// Работа брошена, не дойдя до `finish`: её снимок так и не разобран.
    fn forget_abandoned_job(&mut self) {
        if self.job.take_if(|job| !job.is_alive()).is_some() {
            self.dirty = true;
        }
    }
}

impl InFlight {
    fn is_alive(&self) -> bool {
        self.token.strong_count() > 0
    }
}

fn apply_edits(tree: &mut Tree, edits: &[InputEdit]) {
    for edit in edits.iter().rev() {
        tree.edit(edit);
    }
}

/// Разбор снимка текста. `Send + 'static`: можно отдать в фоновый поток.
pub struct ParseJob {
    language: &'static Language,
    text: Rope,
    /// Отредактированное дерево для инкрементального разбора.
    old_tree: Option<Tree>,
    /// Парсер с состоянием прерванного разбора, если он был.
    parser: Option<Parser>,
    newlines: Option<usize>,
    token: Arc<()>,
    version: u64,
}

/// Результат работы — отдать в [`Syntax::finish`].
pub struct ParseResult {
    tree: Option<Tree>,
    parser: Parser,
    /// Число `\n` в тексте снимка.
    newlines: usize,
    token: Arc<()>,
    version: u64,
}

impl ParseJob {
    /// Разобрать до конца. Заодно компилирует запрос подсветки языка, чтобы
    /// первая подсветка в UI-потоке не платила за это миллисекунды.
    pub fn run(mut self) -> ParseResult {
        self.language.query();
        let tree = self.parse(None);
        self.into_result(tree)
    }

    /// Разбирать не дольше `budget`. Не успели — `Err` с той же работой:
    /// парсер помнит, где остановился, и следующий запуск продолжит с того же
    /// места. Бюджет проверяется раз в сотню шагов парсера, так что небольшой
    /// текст успевает даже с нулевым бюджетом.
    pub fn run_with_budget(mut self, budget: Duration) -> Result<ParseResult, ParseJob> {
        // Бюджет, не помещающийся в Instant, — всё равно что без бюджета.
        match self.parse(Instant::now().checked_add(budget)) {
            Some(tree) => Ok(self.into_result(Some(tree))),
            None => Err(self),
        }
    }

    /// `deadline` — когда прервать разбор; `None` — разбирать до конца.
    fn parse(&mut self, deadline: Option<Instant>) -> Option<Tree> {
        let language = self.language;
        let Self {
            text,
            old_tree,
            parser,
            ..
        } = self;
        if parser.is_none() {
            *parser = new_parser(language);
        }
        let parser = parser.as_mut()?;
        let mut read = |byte: usize, _: Point| chunk_from(text, byte);
        let Some(deadline) = deadline else {
            return parser.parse_with_options(&mut read, old_tree.as_ref(), None);
        };
        let mut progress = |_: &ParseState| {
            if Instant::now() >= deadline {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        parser.parse_with_options(&mut read, old_tree.as_ref(), Some(options))
    }

    fn into_result(self, tree: Option<Tree>) -> ParseResult {
        let newlines = self.newlines.unwrap_or_else(|| count_newlines(&self.text));
        // Парсер есть всегда, кроме неподдержанной грамматики: её до работы не допускает parse_job.
        let parser = self.parser.unwrap_or_default();
        ParseResult {
            tree,
            parser,
            newlines,
            token: self.token,
            version: self.version,
        }
    }
}

fn new_parser(language: &Language) -> Option<Parser> {
    let mut parser = Parser::new();
    parser.set_language(language.grammar()?).ok()?;
    Some(parser)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::language_by_name;

    fn assert_send_static<T: Send + 'static>() {}

    #[test]
    fn job_and_result_can_move_to_another_thread() {
        assert_send_static::<ParseJob>();
        assert_send_static::<ParseResult>();
        assert_send_static::<Syntax>();
    }

    #[test]
    fn job_runs_on_another_thread() {
        let text = Rope::from_str("fn main() { let x = 1; }\n");
        let mut syntax = Syntax::new(language_by_name("rust").unwrap());
        let job = syntax.parse_job(&text).unwrap();
        let result = std::thread::spawn(move || job.run()).join().unwrap();
        assert!(syntax.finish(result));
        let root = syntax.tree().unwrap().root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(!root.has_error());
        assert_eq!(syntax.newlines, Some(1));
    }

    #[test]
    fn no_job_when_nothing_changed() {
        let text = Rope::from_str("a = 1\n");
        let mut syntax = Syntax::new(language_by_name("python").unwrap());
        assert!(syntax.needs_parse());
        let job = syntax.parse_job(&text).unwrap();
        assert!(syntax.is_parsing());
        assert!(syntax.parse_job(&text).is_none(), "one job at a time");
        assert!(syntax.finish(job.run()));
        assert!(!syntax.needs_parse());
        assert!(syntax.parse_job(&text).is_none());

        let mut new_text = text.clone();
        let empty = ChangeSet::identity(text.len_chars());
        syntax.edit(&text, &empty);
        assert!(!syntax.needs_parse(), "identity change is not an edit");
        let cs = ChangeSet::from_changes(text.len_chars(), [(0, 1, Some("b".into()))]);
        syntax.edit(&text, &cs);
        cs.apply(&mut new_text);
        assert!(syntax.needs_parse());
        assert!(syntax.parse_job(&new_text).is_some());
    }

    #[test]
    fn parser_is_reused_between_jobs() {
        let text = Rope::from_str("x\n");
        let mut syntax = Syntax::new(language_by_name("rust").unwrap());
        let job = syntax.parse_job(&text).unwrap();
        assert!(job.parser.is_none());
        syntax.finish(job.run());
        syntax.reset();
        let job = syntax.parse_job(&text).unwrap();
        assert!(job.parser.is_some());
    }
}
