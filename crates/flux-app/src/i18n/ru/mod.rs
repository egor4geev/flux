//! Russian translations: English source string → Russian, one table per area of the UI.

mod commands;
mod common;
mod completion;
mod lsp;
mod menu;
mod navigation;
mod pickers;
mod search;
mod settings;
mod start;
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
];

/// Plural forms (`trn`): the English singular → "1 файл", "2 файла", "5 файлов".
pub(super) const PLURALS: &[(&str, [&str; 3])] = common::PLURALS;
