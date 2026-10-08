//! Поиск файла проекта по имени (cmd-p): нечёткий поиск по путям относительно корня.
//!
//! При каждом открытии проект обходится заново (`flux_search::walk_files`, в фоне):
//! список всегда свежий, а обход обычного проекта — миллисекунды. Пути сразу уходят в
//! `PathMatcher` (nucleo): он сопоставляет их с запросом в своих потоках, пока обход ещё
//! идёт. О новых результатах nucleo сообщает из своих потоков — сигналы склеиваются в
//! канале, и Picker перерисовывается не чаще кадра.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use flux_search::{MAX_FILES, PathMatch, PathMatcher, WalkSummary, walk_files};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{
    AnyElement, App, AppContext, Context, DismissEvent, KeyBinding, SharedString, Task, WeakEntity,
    Window, actions, div, prelude::*, px,
};

use crate::icons::file_icon;
use crate::picker::{Picker, PickerDelegate, highlighted_text};
use crate::theme::{self, Theme};
use crate::workspace::Workspace;

actions!(file_finder, [Toggle]);

/// Перерисовка по сигналам nucleo — не чаще раза в кадр.
const REFRESH_INTERVAL: Duration = Duration::from_millis(16);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-p", Toggle, Some("Workspace"))]);
}

pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(root) = workspace.root().map(Path::to_path_buf) else {
        workspace.show_message("No project folder — open one with ⌘O".into(), cx);
        return;
    };
    let weak = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        let finder = FileFinder::new(root, weak, cx);
        Picker::new(finder, window, cx)
    });
}

pub struct FileFinder {
    root: PathBuf,
    workspace: WeakEntity<Workspace>,
    matcher: PathMatcher,
    /// Итог обхода; `None` — обход ещё идёт.
    walk: Option<WalkSummary>,
    /// Окно закрыли — обход останавливается.
    cancel: Arc<AtomicBool>,
    _tasks: [Task<()>; 2],
}

impl FileFinder {
    /// Сразу начинает обход проекта в фоне.
    fn new(
        root: PathBuf,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Picker<Self>>,
    ) -> Self {
        // nucleo зовёт `notify` из своих потоков на каждый добавленный путь и на каждый
        // досчитанный результат — в канал, а UI-задача их склеивает.
        let (notify, mut signals) = mpsc::unbounded::<()>();
        let matcher = PathMatcher::new(Arc::new(move || {
            notify.unbounded_send(()).ok();
        }));
        let refresh = cx.spawn(async move |picker, cx| {
            while signals.next().await.is_some() {
                while signals.try_recv().is_ok() {}
                let refreshed = picker.update(cx, |picker, cx| {
                    picker.delegate.matcher.tick();
                    cx.notify();
                });
                if refreshed.is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH_INTERVAL).await;
            }
        });

        let cancel = Arc::new(AtomicBool::new(false));
        let injector = matcher.injector();
        let walking = cx.background_spawn({
            let (root, cancel) = (root.clone(), cancel.clone());
            // Инжектор живёт до конца обхода: пока он жив, `is_running` считает, что
            // список растёт.
            async move { walk_files(&root, &cancel, |path| injector.push(path)) }
        });
        let walk = cx.spawn(async move |picker, cx| {
            let summary = walking.await;
            picker
                .update(cx, |picker, cx| {
                    picker.delegate.walk = Some(summary);
                    picker.delegate.matcher.tick();
                    cx.notify();
                })
                .ok();
        });

        Self {
            root,
            workspace,
            matcher,
            walk: None,
            cancel,
            _tasks: [refresh, walk],
        }
    }
}

impl Drop for FileFinder {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl PickerDelegate for FileFinder {
    fn placeholder(&self) -> SharedString {
        "Search files by name…".into()
    }

    fn match_count(&self) -> usize {
        self.matcher.match_count()
    }

    fn update_matches(&mut self, query: &str, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.matcher.set_query(query);
        self.matcher.tick();
        cx.notify();
    }

    fn confirm(&mut self, index: usize, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(found) = self.matcher.get(index) else {
            return;
        };
        let path = self.root.join(&*found.path);
        cx.emit(DismissEvent);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.open_files(vec![path], window, cx)
            })
            .ok();
    }

    fn render_match(
        &mut self,
        index: usize,
        _selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let ui = Theme::ui(cx);
        let Some(found) = self.matcher.get(index) else {
            return div().into_any_element();
        };
        let row = PathRow::new(&found);
        let file = file_icon(&row.name, &ui);
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_2p5()
            .whitespace_nowrap()
            .child(file.render())
            .child(div().flex_none().child(highlighted_text(
                row.name,
                &row.name_positions,
                ui.match_text,
            )))
            .when(!row.dir.is_empty(), |line| {
                line.child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(highlighted_text(row.dir, &row.dir_positions, ui.match_text)),
                )
            })
            .into_any_element()
    }

    fn render_footer(&self, _: &mut Window, _: &mut Context<Picker<Self>>) -> Option<AnyElement> {
        let total = self.matcher.item_count();
        let truncated = self.walk.is_some_and(|walk| walk.truncated);
        let mut footer = footer_text(self.matcher.match_count(), total, truncated);
        if self.walk.is_none() {
            footer.push_str(" · indexing…");
        }
        Some(footer.into_any_element())
    }

    fn empty_message(&self) -> SharedString {
        if self.walk.is_none() {
            "Indexing…".into()
        } else {
            "No matching files".into()
        }
    }
}

/// Строка списка: имя файла и каталог отдельно, совпавшие символы — в обеих частях.
#[derive(Debug, PartialEq, Eq)]
struct PathRow {
    name: String,
    name_positions: Vec<usize>,
    /// Каталог относительно корня без завершающего `/`; пусто — файл в корне.
    dir: String,
    dir_positions: Vec<usize>,
}

impl PathRow {
    /// Позиции `PathMatch` — индексы `char` во всём относительном пути `dir/name`.
    fn new(found: &PathMatch) -> Self {
        let path: &str = &found.path;
        let (dir, name) = match path.rfind('/') {
            Some(slash) => (&path[..slash], &path[slash + 1..]),
            None => ("", path),
        };
        let dir_chars = dir.chars().count();
        let name_start = path.chars().count() - name.chars().count();
        Self {
            name: name.to_string(),
            name_positions: found
                .positions
                .iter()
                .filter_map(|&p| p.checked_sub(name_start))
                .collect(),
            dir: dir.to_string(),
            dir_positions: found
                .positions
                .iter()
                .copied()
                .filter(|&p| p < dir_chars)
                .collect(),
        }
    }
}

/// «12 of 345 files»; обход упёрся в лимит — «12 of 100000+ files».
fn footer_text(matched: usize, total: usize, truncated: bool) -> String {
    let plus = if truncated { "+" } else { "" };
    let total = if truncated {
        total.max(MAX_FILES)
    } else {
        total
    };
    format!("{matched} of {total}{plus} files")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, positions: &[usize]) -> PathRow {
        PathRow::new(&PathMatch {
            path: path.into(),
            positions: positions.to_vec(),
        })
    }

    #[test]
    fn path_splits_into_name_and_directory() {
        let got = row("crates/flux-app/src/editor.rs", &[0, 16, 20, 21]);
        assert_eq!(got.name, "editor.rs");
        assert_eq!(got.dir, "crates/flux-app/src");
        // 20, 21 — «ed» в имени (имя начинается с 20-го символа); 0 и 16 — в каталоге.
        assert_eq!(got.name_positions, vec![0, 1]);
        assert_eq!(got.dir_positions, vec![0, 16]);
    }

    #[test]
    fn file_in_root_has_no_directory() {
        let got = row("Cargo.toml", &[0, 6]);
        assert_eq!(got.name, "Cargo.toml");
        assert_eq!(got.dir, "");
        assert_eq!(got.name_positions, vec![0, 6]);
        assert!(got.dir_positions.is_empty());
    }

    #[test]
    fn slash_position_belongs_to_neither_part() {
        // «a/b»: позиция 1 — сам `/`.
        let got = row("a/b", &[1]);
        assert!(got.name_positions.is_empty());
        assert!(got.dir_positions.is_empty());
    }

    #[test]
    fn positions_are_chars_in_non_ascii_paths() {
        // «заметки/план.md»: имя начинается с 8-го символа.
        let got = row("заметки/план.md", &[0, 8, 9]);
        assert_eq!(got.name, "план.md");
        assert_eq!(got.dir, "заметки");
        assert_eq!(got.name_positions, vec![0, 1]);
        assert_eq!(got.dir_positions, vec![0]);
    }

    #[test]
    fn footer_counts_files() {
        assert_eq!(footer_text(3, 53, false), "3 of 53 files");
        assert_eq!(footer_text(0, 0, false), "0 of 0 files");
        assert_eq!(
            footer_text(10, MAX_FILES, true),
            format!("10 of {MAX_FILES}+ files")
        );
    }
}
