mod app_log;
mod app_menu;
mod appearance_settings;
mod blame;
mod branch_dialogs;
mod branches_popup;
mod bundled;
mod claude;
mod claude_actions;
mod claude_agent_view;
mod claude_cards;
mod claude_chat;
mod claude_composer;
mod claude_diff;
mod claude_history;
mod claude_panel;
mod claude_search;
mod claude_select;
mod claude_session;
mod claude_status;
mod claude_tools;
mod claude_transcript;
mod claude_turns;
mod code_actions;
mod command_palette;
mod command_usage;
mod commit_details;
mod commit_panel;
mod compare_dialog;
mod completion;
mod conflicts_dialog;
mod context_menu;
mod contributions;
mod diagnostics;
mod dialog;
mod diff_view;
mod display;
mod editor;
mod editor_menu;
mod editor_settings;
mod element;
mod feedback;
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
mod icon_themes;
mod icons;
mod image_view;
mod input;
mod input_dialog;
mod launchpad;
mod leftovers;
mod locations;
mod log_actions;
mod lsp;
mod lsp_settings;
mod lsp_status;
mod lsp_watch;
mod markdown;
mod markdown_preview;
mod merge_view;
mod navigation;
mod notification_center;
mod notifications;
mod notifications_panel;
mod open_urls;
mod picker;
mod plugin_calls;
mod plugin_catalog;
mod plugin_claude;
mod plugin_form;
mod plugin_git;
mod plugin_links;
mod plugin_manager;
mod plugin_menus;
mod plugin_picker;
mod plugin_review;
mod plugin_settings;
mod plugin_suggestions;
mod plugin_terminals;
mod plugin_updates;
mod plugin_view;
mod plugins;
mod popup;
mod project_popup;
mod project_search;
mod project_state;
mod prompt_input;
mod push_dialog;
mod quick_switch;
mod rebase_dialog;
mod recent;
mod rename;
#[cfg(feature = "scenario")]
mod scenario;
mod scrollbar;
mod settings;
mod settings_file;
mod settings_view;
mod start_screen;
mod stash_panel;
mod system_notify;
mod task_contexts;
mod terminal_element;
mod terminal_group;
mod terminal_links;
mod terminal_panel;
mod terminal_search;
mod terminal_view;
mod theme;
mod ui;
mod ui_scale;
mod update_notes;
mod update_settings;
mod updates;
mod vcs_menu;
mod welcome;
mod workspace;
mod wrap;

use std::path::{PathBuf, absolute};

use flux_search::find_vcs_root;

use gpui::{App, Application};

use theme::Theme;

fn main() {
    // Flux's log and crash reports (part 10.6), before anything can print or panic.
    app_log::start();
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
    // Nor the user's keychain: plugins' secrets go to a file in the plugins' folder (a rebuilt
    // unsigned Flux isn't on a keychain item's access list — macOS would ask the user).
    #[cfg(feature = "scenario")]
    if std::env::var_os("FLUX_SCENARIO").is_some()
        && std::env::var_os("FLUX_PLUGIN_SECRETS_FILE").is_none()
        && let Some(home) = std::env::var_os("FLUX_PLUGINS_HOME")
    {
        let file = PathBuf::from(home).join("secrets.json");
        // SAFETY: as above.
        unsafe { std::env::set_var("FLUX_PLUGIN_SECRETS_FILE", file) };
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
    // Compiled WebAssembly grammars of plugins: next to the compiled plugins' components.
    flux_syntax::set_wasm_cache_dir(flux_plugin::paths::cache_dir().with_file_name("grammars"));

    let app = Application::new().with_assets(icons::Assets);
    // Folders and files macOS asks to open (the Dock menu's recent folders, a folder dropped on the
    // Dock icon): they may come right after the launch, before the windows have opened.
    let urls = open_urls::listen(&app);
    app.run(move |cx: &mut App| {
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
        // The interface language, the theme and the icons as the settings choose them, from
        // the plugins that bring them (stage 8.3): before any window opens.
        i18n::apply_choice(settings::appearance(cx).language);
        contributions::init(cx);
        quick_switch::init(cx);
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
        image_view::init(cx);
        claude_history::init(cx);
        claude_cards::init(cx);
        claude_diff::init(cx);
        claude_actions::init(cx);
        editor_menu::init(cx);
        code_actions::init(cx);
        prompt_input::init(cx);
        plugin_manager::init(cx);
        plugin_settings::init(cx);
        task_contexts::init(cx);
        ui_scale::init(cx);
        project_popup::init(cx);
        command_usage::init(cx);
        markdown_preview::init(cx);
        wrap::init(cx);
        editor_settings::init(cx);
        scrollbar::init(cx);
        lsp_status::init(cx);
        lsp_settings::init(cx);
        settings_file::init(cx);
        feedback::init(cx);
        app_log::init(cx);
        welcome::init(cx);
        updates::init(cx);
        update_notes::init(cx);

        // The project of the command line, or the projects open when Flux quit (part 10.1);
        // on the first launch the welcome guide — alone when started from the Dock — and after
        // an update What's New (part 10.7).
        let _window = welcome::startup(root, paths, cx).expect("failed to open window");
        open_urls::run(urls, cx);
        // The recent projects for the Dock menu, also those opened before Flux reported them.
        recent::tell_the_system(cx);
        #[cfg(feature = "scenario")]
        scenario::run(_window, cx);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // A scenario in the background doesn't take the keyboard from the app the user works
        // in (`FLUX_SCENARIO_BACKGROUND`).
        #[cfg(feature = "scenario")]
        let activate = !scenario::background();
        #[cfg(not(feature = "scenario"))]
        let activate = true;
        if activate {
            cx.activate(true);
        }
    });
}

/// Project root: the directory from the arguments; without one, the nearest ancestor of the current
/// directory that contains a repository (`.git`, etc.), otherwise the current directory itself. If
/// not launched from a terminal (the current directory is `/`), there is no project: the projects
/// open when Flux quit come back instead (a scenario pretends so with `FLUX_SCENARIO_FINDER`).
fn project_root(dirs: Vec<PathBuf>) -> Option<PathBuf> {
    let mut dirs = dirs.into_iter();
    let root = match dirs.next() {
        Some(dir) => dir,
        None => {
            #[cfg(feature = "scenario")]
            if scenario::launched_from_finder() {
                return None;
            }
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
