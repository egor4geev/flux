//! The plugins bundled into Flux (stage 8, ADR-029), as in JetBrains IDEs: shipped inside the
//! binary, they can be turned off but not removed. `build.rs` builds the folders of `plugins/` for
//! `wasm32-wasip2` and embeds their files; a plugin it couldn't build is left out with a warning.

use flux_plugin::registry::Bundled;

static PLUGINS: &[Bundled] = include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

/// The bundled plugins: the TODO window.
pub fn plugins() -> &'static [Bundled] {
    PLUGINS
}

#[cfg(test)]
mod tests {
    use flux_plugin::registry::{PluginSource, load_bundled};

    use super::*;

    #[test]
    fn bundled_plugins_load() {
        let entries: Vec<_> = plugins()
            .iter()
            .map(|plugin| load_bundled(plugin).expect("a bundled manifest reads"))
            .collect();
        for entry in &entries {
            assert_eq!(entry.source, PluginSource::Bundled);
            assert_eq!(entry.problem, None, "{}", entry.id());
            if entry.manifest.wasm.is_some() {
                assert!(entry.wasm().is_some_and(|wasm| wasm.starts_with(b"\0asm")));
            }
        }
        // Without the wasm32-wasip2 target, build.rs leaves the plugins out (and says why).
        if env!("FLUX_BUNDLED_SKIPPED").is_empty() {
            let todo = entries
                .iter()
                .find(|entry| entry.id() == "flux.todo")
                .expect("the TODO plugin is bundled");
            assert_eq!(todo.manifest.tool_windows[0].id, "todo");
            assert_eq!(todo.translate("ru", "Refresh TODO"), "Обновить TODO");
            assert!(todo.files.read("icons/todo.svg").is_some());
        }
    }
}
