//! A plugin's translations: `locales/<language>.toml` in its folder, English text = translation,
//! as Flux's own tables (ADR-018). Flux translates the manifest's strings with them (the palette,
//! the manager, Settings), and the plugin its own through the `i18n` interface.
//!
//! ```toml
//! "Refresh" = "Обновить"
//! "{0} items in {1} files" = "{0} элементов в {1} файлах"
//! ```

use std::collections::HashMap;

use crate::registry::PluginFiles;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Locales(HashMap<String, HashMap<String, String>>);

impl Locales {
    /// The tables of the plugin's `locales/` folder; a broken table is skipped (the plugin's log
    /// says so when it starts).
    pub fn load(files: &PluginFiles) -> Locales {
        let mut tables = HashMap::new();
        for path in files.list("locales/") {
            let Some(language) = path
                .strip_prefix("locales/")
                .and_then(|name| name.strip_suffix(".toml"))
            else {
                continue;
            };
            let Some(bytes) = files.read(&path) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            if let Ok(table) = toml::from_str::<HashMap<String, String>>(text) {
                tables.insert(language.to_string(), table);
            }
        }
        Locales(tables)
    }

    /// The text in `language`; the text itself when there is no translation.
    pub fn translate<'a>(&'a self, language: &str, text: &'a str) -> &'a str {
        self.0
            .get(language)
            .and_then(|table| table.get(text))
            .map_or(text, String::as_str)
    }

    /// The languages with a table.
    pub fn languages(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}
