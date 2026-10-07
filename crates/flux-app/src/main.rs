mod editor;
mod element;
mod theme;

use std::path::PathBuf;

use flux_core::Document;
use gpui::{
    App, AppContext, Application, Bounds, Focusable, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use editor::{AfterClose, Editor};

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let document = match &path {
        Some(path) => Document::open(path).unwrap_or_else(|err| {
            eprintln!("flux: cannot open {}: {err}", path.display());
            std::process::exit(1);
        }),
        None => Document::from_text(""),
    };

    Application::new().run(move |cx: &mut App| {
        editor::bind_keys(cx);

        let bounds = Bounds::centered(None, size(px(1100.), px(750.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("flux".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let editor = cx.new(|cx| Editor::new(document, cx));
            window.focus(&editor.focus_handle(cx));

            // Красная кнопка окна тоже спрашивает про несохранённые изменения.
            let guarded = editor.clone();
            window.on_window_should_close(cx, move |window, cx| {
                guarded.update(cx, |editor, cx| {
                    editor.request_close(AfterClose::CloseWindow, window, cx)
                })
            });
            editor
        })
        .expect("failed to open window");

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
