mod display;
mod editor;
mod element;
mod highlighter;
#[cfg(feature = "scenario")]
mod scenario;
mod theme;
mod workspace;

use std::path::{PathBuf, absolute};

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

use theme::Theme;
use workspace::Workspace;

fn main() {
    // `flux [пути...]`. Пути абсолютные: по ним сравниваются вкладки и различаются
    // одноимённые файлы из разных каталогов.
    let paths: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .map(|path| absolute(&path).unwrap_or(path))
        .collect();

    Application::new().run(move |cx: &mut App| {
        cx.set_global(Theme::github_dark());
        editor::bind_keys(cx);
        workspace::init(cx);

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
                cx.new(|cx| Workspace::new(paths, window, cx))
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
