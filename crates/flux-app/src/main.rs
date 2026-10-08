mod command_palette;
mod context_menu;
mod display;
mod editor;
mod element;
mod file_finder;
mod file_tree;
mod find_bar;
mod go_to_line;
mod highlighter;
mod i18n;
mod icons;
mod input;
mod launchpad;
mod picker;
mod project_search;
mod recent;
#[cfg(feature = "scenario")]
mod scenario;
mod start_screen;
mod theme;
mod ui;
mod workspace;

use std::path::{PathBuf, absolute};

use flux_search::find_vcs_root;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, point, px, size,
};

use theme::Theme;
use workspace::Workspace;

fn main() {
    // `flux [paths...]`: a directory among the arguments is the project root; files go into tabs.
    // Paths are absolute: tabs are compared by path, and files with the same name are told apart by
    // path.
    let args: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .map(|path| absolute(&path).unwrap_or(path))
        .collect();
    let (dirs, paths): (Vec<_>, Vec<_>) = args.into_iter().partition(|path| path.is_dir());
    let root = project_root(dirs);
    i18n::init();

    Application::new()
        .with_assets(icons::Assets)
        .run(move |cx: &mut App| {
            cx.set_global(Theme::flux_night());
            theme::init_fonts(cx);
            editor::bind_keys(cx);
            workspace::init(cx);
            input::init(cx);
            picker::init(cx);
            command_palette::init(cx);
            context_menu::init(cx);
            file_finder::init(cx);
            file_tree::init(cx);
            find_bar::init(cx);
            go_to_line::init(cx);
            project_search::init(cx);

            let bounds = Bounds::centered(None, size(px(1240.), px(820.)), cx);
            // A custom title bar on a glass window frame: the system title bar is transparent, the
            // traffic lights are centered in the title bar, and the desktop behind the window is
            // blurred.
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Flux".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(13.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            };
            #[cfg(feature = "scenario")]
            let options = scenario::window_options(options);
            let _window = cx
                .open_window(options, |window, cx| {
                    cx.new(|cx| Workspace::new(root, paths, window, cx))
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

/// Project root: the directory from the arguments; without one, the nearest ancestor of the current
/// directory that contains a repository (`.git`, etc.), otherwise the current directory itself. If
/// not launched from a terminal (the current directory is `/`), there is no project.
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
        eprintln!(
            "flux: {}: one project folder per window, skipped",
            extra.display()
        );
    }
    Some(std::fs::canonicalize(&root).unwrap_or(root))
}
