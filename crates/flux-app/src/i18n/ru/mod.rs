//! Russian translations: English source string → Russian, one table per area of the UI.

mod blame;
mod branches;
mod commands;
mod commit;
mod common;
mod completion;
mod conflicts;
mod diff;
mod git;
mod git_gutter;
mod git_log;
mod log_actions;
mod lsp;
mod menu;
mod navigation;
mod pickers;
mod rebase;
mod search;
mod settings;
mod start;
mod stash;
mod sync;
mod terminal;
mod terminal_panel;
mod terminal_search;
mod tree;
mod workspace;
mod dialog;
mod messages;
mod notification_center;
mod notifications_panel;
mod popups;
mod plugins;
mod plugin_view;
mod plugin_manager;
mod claude;
mod claude_transcript;
mod claude_composer;
mod claude_cards;

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
];
