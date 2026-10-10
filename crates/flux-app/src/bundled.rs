//! The plugins bundled into Flux (stage 8, ADR-029), as in JetBrains IDEs: shipped inside the
//! binary, they can be turned off but not removed. `build.rs` builds the folders of `plugins/` for
//! `wasm32-wasip2` and embeds their files; a plugin it couldn't build is left out with a warning.

use flux_plugin::registry::Bundled;

static PLUGINS: &[Bundled] = include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

/// The bundled plugins: the TODO window, JavaScript and TypeScript (stage 8.3).
pub fn plugins() -> &'static [Bundled] {
    PLUGINS
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

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
            for file in entry.manifest.named_files() {
                assert!(entry.files.read(file).is_some(), "{}: {file}", entry.id());
            }
        }
        // Without the wasm32-wasip2 target, build.rs leaves the plugins with code out (and says
        // why).
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

    fn javascript() -> Arc<flux_plugin::registry::PluginEntry> {
        let entry = plugins()
            .iter()
            .map(|plugin| load_bundled(plugin).unwrap())
            .find(|entry| entry.id() == "flux.javascript")
            .expect("JavaScript and TypeScript come with Flux");
        Arc::new(entry)
    }

    /// JavaScript and TypeScript come with Flux (stage 8.3): four languages on the grammars
    /// compiled into Flux, and the TypeScript language server for all of their files.
    #[test]
    fn javascript_and_typescript_are_bundled() {
        let entry = javascript();
        assert!(entry.manifest.wasm.is_none(), "no code, only contributions");
        assert_eq!(
            entry.translate("ru", "JavaScript and TypeScript"),
            "JavaScript и TypeScript"
        );
        let resolved = crate::contributions::resolve(&[entry]);
        assert!(resolved.problems.is_empty(), "{:?}", resolved.problems);
        let (owner, configs) = &resolved.languages[0];
        assert_eq!(owner, "flux.javascript");
        let extensions = |name: &str| -> Vec<&str> {
            configs
                .iter()
                .find(|config| config.name == name)
                .unwrap()
                .extensions
                .iter()
                .map(String::as_str)
                .collect()
        };
        assert_eq!(extensions("javascript"), ["js", "mjs", "cjs"]);
        assert_eq!(extensions("jsx"), ["jsx"]);
        assert_eq!(extensions("typescript"), ["ts", "mts", "cts"]);
        assert_eq!(extensions("tsx"), ["tsx"]);
        let lsp_id = |language: &str| resolved.lsp_ids[language].as_str();
        assert_eq!(lsp_id("javascript"), "javascript");
        assert_eq!(lsp_id("jsx"), "javascriptreact");
        assert_eq!(lsp_id("typescript"), "typescript");
        assert_eq!(lsp_id("tsx"), "typescriptreact");

        let server = &resolved.servers[0];
        assert_eq!(server.name, "typescript-language-server");
        let mut served = server.extensions.clone();
        served.sort();
        assert_eq!(
            served,
            ["cjs", "cts", "js", "jsx", "mjs", "mts", "ts", "tsx"]
        );
        assert_eq!(resolved.server_plugins[&server.name], "flux.javascript");
        assert!(server.install.is_some());
        let configs = std::slice::from_ref(server);
        for path in ["a.js", "b.mjs", "c.cjs", "App.jsx", "d.ts", "e.d.mts", "f.cts", "App.tsx"] {
            assert!(
                flux_lsp::server_for_path(configs, Path::new(path)).is_some(),
                "{path}"
            );
        }
    }

    /// Every query of the plugin compiles against its grammar, and every capture has a scope in
    /// Flux's theme.
    #[test]
    fn javascript_queries_compile_against_their_grammars() {
        let resolved = crate::contributions::resolve(&[javascript()]);
        // Its own owner: the real one is the window's (other tests may run meanwhile).
        let owner = "test.bundled-javascript";
        let configs: Vec<_> = resolved.languages[0]
            .1
            .iter()
            .map(|config| flux_syntax::LanguageConfig {
                name: format!("zz-{}", config.name),
                aliases: Vec::new(),
                extensions: Vec::new(),
                file_names: Vec::new(),
                ..config.clone()
            })
            .collect();
        flux_syntax::register(owner, configs);
        let theme = crate::theme::Theme::flux_night();
        let scopes = theme.syntax_scopes();
        for name in ["javascript", "jsx", "typescript", "tsx"] {
            let language = flux_syntax::language_by_name(&format!("zz-{name}")).unwrap();
            assert!(language.grammar().is_some(), "{name}: {:?}", language.grammar_error());
            assert!(language.query().is_some(), "{name}: the query doesn't compile");
            let map = flux_syntax::HighlightMap::new(&language, &scopes);
            for (i, capture) in language.capture_names().iter().enumerate() {
                assert!(
                    map.get(i as u32).is_some() || capture.starts_with('_'),
                    "{name}: @{capture} has no scope"
                );
            }
        }
        flux_syntax::unregister(owner);
    }
}
