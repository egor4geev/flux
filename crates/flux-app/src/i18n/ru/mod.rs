//! Russian translations: English source string → Russian, one table per area of the UI.

mod app_log;
mod appearance;
mod appearance_common;
mod blame;
mod branches;
mod catalog;
mod claude;
mod claude_actions;
mod claude_agents;
mod claude_cards;
mod claude_changes;
mod claude_commands;
mod claude_composer;
mod claude_console;
mod claude_console_core;
mod claude_history;
mod claude_session;
mod claude_tabs;
mod claude_transcript;
mod claude_turns;
mod code_actions;
mod command_usage;
mod commands;
mod commit;
mod common;
mod completion;
mod conflicts;
mod dialog;
mod diff;
mod feedback;
mod git;
mod git_gutter;
mod git_log;
mod icon_themes;
mod image_view;
mod interface;
mod languages;
mod log_actions;
mod lsp;
mod lsp_settings;
mod lsp_status;
mod markdown_preview;
mod menu;
mod messages;
mod motion;
mod navigation;
mod notification_center;
mod notifications_panel;
mod pickers;
mod plugin_api;
mod plugin_claude;
mod plugin_form;
mod plugin_git;
mod plugin_links;
mod plugin_manager;
mod plugin_menus;
mod plugin_picker;
mod plugin_review;
mod plugin_tabs;
mod plugin_view;
mod plugins;
mod popups;
mod project_popup;
mod projects;
mod rebase;
mod scrollbar;
mod search;
mod search_files;
mod settings;
mod soft_wrap;
mod start;
mod stash;
mod sync;
mod task_contexts;
mod terminal;
mod terminal_panel;
mod terminal_search;
mod themes;
mod tree;
mod ui_scale;
mod updates;
mod welcome;
mod welcome_claude;
mod welcome_setup;
mod welcome_shortcuts;
mod welcome_tour;
mod welcome_whats_new;
mod windows;
mod workspace;

/// Tables of plain strings and templates (`tr`, `trf`).
pub(super) const STRINGS: &[&[(&str, &str)]] = &[
    common::STRINGS,
    workspace::STRINGS,
    start::STRINGS,
    tree::STRINGS,
    search::STRINGS,
    pickers::STRINGS,
    commands::STRINGS,
    lsp::STRINGS,
    navigation::STRINGS,
    settings::STRINGS,
    menu::STRINGS,
    completion::STRINGS,
    terminal::STRINGS,
    terminal_search::STRINGS,
    terminal_panel::STRINGS,
    git::STRINGS,
    git_gutter::STRINGS,
    diff::STRINGS,
    commit::STRINGS,
    branches::STRINGS,
    sync::STRINGS,
    stash::STRINGS,
    conflicts::STRINGS,
    git_log::STRINGS,
    log_actions::STRINGS,
    rebase::STRINGS,
    blame::STRINGS,
    dialog::STRINGS,
    messages::STRINGS,
    motion::STRINGS,
    notification_center::STRINGS,
    notifications_panel::STRINGS,
    popups::STRINGS,
    plugins::STRINGS,
    plugin_view::STRINGS,
    plugin_manager::STRINGS,
    claude::STRINGS,
    claude_transcript::STRINGS,
    claude_composer::STRINGS,
    claude_cards::STRINGS,
    claude_session::STRINGS,
    claude_history::STRINGS,
    claude_changes::STRINGS,
    claude_actions::STRINGS,
    code_actions::STRINGS,
    plugin_api::STRINGS,
    plugin_menus::STRINGS,
    plugin_review::STRINGS,
    plugin_tabs::STRINGS,
    plugin_form::STRINGS,
    plugin_picker::STRINGS,
    plugin_git::STRINGS,
    plugin_claude::STRINGS,
    plugin_links::STRINGS,
    task_contexts::STRINGS,
    appearance_common::STRINGS,
    languages::STRINGS,
    themes::STRINGS,
    appearance::STRINGS,
    icon_themes::STRINGS,
    catalog::STRINGS,
    projects::STRINGS,
    windows::STRINGS,
    claude_commands::STRINGS,
    claude_tabs::STRINGS,
    claude_console::STRINGS,
    claude_console_core::STRINGS,
    claude_turns::STRINGS,
    claude_agents::STRINGS,
    image_view::STRINGS,
    ui_scale::STRINGS,
    interface::STRINGS,
    project_popup::STRINGS,
    command_usage::STRINGS,
    markdown_preview::STRINGS,
    search_files::STRINGS,
    soft_wrap::STRINGS,
    scrollbar::STRINGS,
    lsp_status::STRINGS,
    lsp_settings::STRINGS,
    feedback::STRINGS,
    app_log::STRINGS,
    welcome::STRINGS,
    welcome_setup::STRINGS,
    welcome_claude::STRINGS,
    welcome_tour::STRINGS,
    welcome_shortcuts::STRINGS,
    welcome_whats_new::STRINGS,
    updates::STRINGS,
];

/// Plural forms (`trn`): the English singular → "1 файл", "2 файла", "5 файлов"; one table per
/// area, as the strings.
pub(super) const PLURALS: &[&[(&str, [&str; 3])]] = &[
    common::PLURALS,
    git::PLURALS,
    branches::PLURALS,
    sync::PLURALS,
    stash::PLURALS,
    conflicts::PLURALS,
    git_log::PLURALS,
    log_actions::PLURALS,
    rebase::PLURALS,
    blame::PLURALS,
    plugins::PLURALS,
    plugin_view::PLURALS,
    plugin_manager::PLURALS,
    claude::PLURALS,
    claude_transcript::PLURALS,
    claude_composer::PLURALS,
    claude_cards::PLURALS,
    claude_session::PLURALS,
    claude_history::PLURALS,
    claude_changes::PLURALS,
    claude_actions::PLURALS,
    code_actions::PLURALS,
    plugin_api::PLURALS,
    plugin_menus::PLURALS,
    plugin_review::PLURALS,
    plugin_tabs::PLURALS,
    plugin_form::PLURALS,
    plugin_picker::PLURALS,
    plugin_git::PLURALS,
    plugin_claude::PLURALS,
    plugin_links::PLURALS,
    task_contexts::PLURALS,
    appearance_common::PLURALS,
    languages::PLURALS,
    themes::PLURALS,
    appearance::PLURALS,
    icon_themes::PLURALS,
    catalog::PLURALS,
    projects::PLURALS,
    windows::PLURALS,
    claude_commands::PLURALS,
    claude_tabs::PLURALS,
    claude_console::PLURALS,
    claude_console_core::PLURALS,
    claude_turns::PLURALS,
    claude_agents::PLURALS,
    image_view::PLURALS,
    ui_scale::PLURALS,
    interface::PLURALS,
    project_popup::PLURALS,
    command_usage::PLURALS,
    markdown_preview::PLURALS,
    search_files::PLURALS,
    soft_wrap::PLURALS,
    scrollbar::PLURALS,
    lsp_status::PLURALS,
    lsp_settings::PLURALS,
    feedback::PLURALS,
    updates::PLURALS,
    app_log::PLURALS,
    welcome::PLURALS,
    welcome_setup::PLURALS,
    welcome_claude::PLURALS,
    welcome_tour::PLURALS,
    welcome_shortcuts::PLURALS,
    welcome_whats_new::PLURALS,
    motion::PLURALS,
];
