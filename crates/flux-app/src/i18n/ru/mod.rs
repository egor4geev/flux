//! Russian translations: English source string → Russian, one table per area of the UI.

mod appearance;
mod appearance_common;
mod blame;
mod branches;
mod catalog;
mod claude;
mod claude_actions;
mod claude_cards;
mod claude_changes;
mod claude_composer;
mod claude_history;
mod claude_session;
mod claude_transcript;
mod code_actions;
mod commands;
mod commit;
mod common;
mod completion;
mod conflicts;
mod dialog;
mod diff;
mod git;
mod git_gutter;
mod git_log;
mod icon_themes;
mod languages;
mod log_actions;
mod lsp;
mod menu;
mod messages;
mod navigation;
mod notification_center;
mod notifications_panel;
mod pickers;
mod plugin_api;
mod plugin_manager;
mod plugin_menus;
mod plugin_review;
mod plugin_view;
mod plugins;
mod popups;
mod rebase;
mod search;
mod settings;
mod start;
mod stash;
mod sync;
mod terminal;
mod terminal_panel;
mod terminal_search;
mod themes;
mod tree;
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
    appearance_common::STRINGS,
    languages::STRINGS,
    themes::STRINGS,
    appearance::STRINGS,
    icon_themes::STRINGS,
    catalog::STRINGS,
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
    appearance_common::PLURALS,
    languages::PLURALS,
    themes::PLURALS,
    appearance::PLURALS,
    icon_themes::PLURALS,
    catalog::PLURALS,
];
