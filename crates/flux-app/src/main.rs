mod command_palette;
mod display;
mod editor;
mod element;
mod file_finder;
mod find_bar;
mod go_to_line;
mod highlighter;
mod input;
mod picker;
mod project_search;
#[cfg(feature = "scenario")]
mod scenario;
mod theme;
mod workspace;

use std::path::{PathBuf, absolute};

use flux_search::find_vcs_root;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

use theme::Theme;
use workspace::Workspace;

fn main() {
    // `flux [пути...]`: каталог среди аргументов — корень проекта, файлы — во вкладки. Пути
    // абсолютные: по ним сравниваются вкладки и различаются одноимённые файлы.
    let args: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .map(|path| absolute(&path).unwrap_or(path))
        .collect();
    let (dirs, paths): (Vec<_>, Vec<_>) = args.into_iter().partition(|path| path.is_dir());
    // Только каталог (`flux .`) — пустое окно проекта, без безымянного документа.
    let untitled = dirs.is_empty();
    let root = project_root(dirs);

    Application::new().run(move |cx: &mut App| {
        cx.set_global(Theme::github_dark());
        editor::bind_keys(cx);
        workspace::init(cx);
        input::init(cx);
        picker::init(cx);
        command_palette::init(cx);
        file_finder::init(cx);
        find_bar::init(cx);
        go_to_line::init(cx);
        project_search::init(cx);

        let bounds = Bounds::centered(None, size(px(1100.), px(750.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("flux".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _window = cx
            .open_window(options, |window, cx| {
                cx.new(|cx| Workspace::new(root, paths, untitled, window, cx))
            })
            .expect("failed to open window");
        #[cfg(feature = "scenario")]
        scenario::run(_window.into(), cx);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Корень проекта: каталог из аргументов; без него — ближайший предок текущего каталога
/// с репозиторием (`.git` и т.п.), иначе сам текущий каталог. Запуск не из терминала
/// (текущий каталог — `/`) — без проекта.
fn project_root(dirs: Vec<PathBuf>) -> Option<PathBuf> {
    let mut dirs = dirs.into_iter();
    let root = match dirs.next() {
        Some(dir) => dir,
        None => {
            let cwd = std::env::current_dir().ok()?;
            cwd.parent()?;
            find_vcs_root(&cwd).unwrap_or(cwd)
        }
    };
    for extra in dirs {
        eprintln!("flux: {}: one project folder per window, skipped", extra.display());
    }
    Some(std::fs::canonicalize(&root).unwrap_or(root))
}
