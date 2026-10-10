mod app_menu;
mod blame;
mod branch_dialogs;
mod branches_popup;
mod bundled;
mod claude;
mod claude_actions;
mod claude_cards;
mod claude_chat;
mod claude_composer;
mod claude_diff;
mod claude_history;
mod claude_panel;
mod claude_session;
mod claude_tools;
mod claude_transcript;
mod command_palette;
mod commit_details;
mod code_actions;
mod commit_panel;
mod compare_dialog;
mod completion;
mod conflicts_dialog;
mod context_menu;
mod diagnostics;
mod dialog;
mod diff_view;
mod display;
mod editor;
mod editor_menu;
mod element;
mod file_finder;
mod file_tree;
mod find_bar;
mod git;
mod git_graph;
mod git_gutter;
mod git_log;
mod git_sync;
mod git_window;
mod go_to_line;
mod highlighter;
mod hover;
mod i18n;
mod icons;
mod input;
mod input_dialog;
mod launchpad;
mod locations;
mod log_actions;
mod lsp;
mod markdown;
mod merge_view;
mod navigation;
mod notification_center;
mod notifications;
mod notifications_panel;
mod picker;
mod plugin_calls;
mod plugin_manager;
mod plugin_menus;
mod plugin_review;
mod plugin_settings;
mod plugin_view;
mod plugin_terminals;
mod plugins;
mod popup;
mod project_search;
mod prompt_input;
mod push_dialog;
mod rebase_dialog;
mod recent;
mod rename;
#[cfg(feature = "scenario")]
mod scenario;
mod settings;
mod settings_view;
mod start_screen;
mod stash_panel;
mod terminal_element;
mod terminal_group;
mod terminal_links;
mod terminal_panel;
mod terminal_search;
mod terminal_view;
mod theme;
mod ui;
mod vcs_menu;
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
    // A UI scenario doesn't touch the user's plugins, their data and logs (as it doesn't touch
    // the settings): unless given, they live in a fresh temporary folder.
    #[cfg(feature = "scenario")]
    if std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var_os("FLUX_PLUGINS_HOME").is_none()
    {
        let home =
            std::env::temp_dir().join(format!("flux-scenario-plugins-{}", std::process::id()));
        // SAFETY: nothing else runs yet: no thread reads the environment.
        unsafe { std::env::set_var("FLUX_PLUGINS_HOME", home) };
    }
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
            input_dialog::init(cx);
            picker::init(cx);
            command_palette::init(cx);
            context_menu::init(cx);
            file_finder::init(cx);
            file_tree::init(cx);
            find_bar::init(cx);
            go_to_line::init(cx);
            project_search::init(cx);
            settings::init(cx);
            dialog::init(cx);
            settings_view::init(cx);
            app_menu::init(cx);
            lsp::init(cx);
            diagnostics::init(cx);
            completion::init(cx);
            hover::init(cx);
            navigation::init(cx);
            terminal_view::init(cx);
            terminal_search::init(cx);
            terminal_group::init(cx);
            terminal_panel::init(cx);
            git::init(cx);
            git_gutter::init(cx);
            commit_panel::init(cx);
            push_dialog::init(cx);
            diff_view::init(cx);
            branches_popup::init(cx);
            branch_dialogs::init(cx);
            git_sync::init(cx);
            compare_dialog::init(cx);
            commit_details::init(cx);
            log_actions::init(cx);
            stash_panel::init(cx);
            conflicts_dialog::init(cx);
            merge_view::init(cx);
            git_log::init(cx);
            git_window::init(cx);
            notifications_panel::init(cx);
            rebase_dialog::init(cx);
            plugins::init(cx);
            plugin_view::init(cx);
            claude::init(cx);
            claude_panel::init(cx);
            claude_composer::init(cx);
            claude_transcript::init(cx);
            claude_cards::init(cx);
            claude_diff::init(cx);
            claude_actions::init(cx);
            editor_menu::init(cx);
            code_actions::init(cx);
            prompt_input::init(cx);
            plugin_manager::init(cx);
            plugin_settings::init(cx);

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
