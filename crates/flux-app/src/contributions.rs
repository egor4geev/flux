//! What the turned-on plugins add without code (stage 8.3, ADR-032): languages — their grammars
//! and highlighting queries go to flux-syntax's registry; language servers — the window's language
//! servers ([`servers`]); color themes — [`crate::theme`]; sets of file icons and the languages'
//! own file icons — [`crate::icon_themes`].
//!
//! The plugin store calls [`refresh`] with its turned-on plugins whenever they change (a plugin
//! turned on or off, installed, removed, reloaded); [`init`] does the same before the window opens,
//! so the first frame already has the chosen theme and icons. A refresh that changes nothing is
//! cheap: the plugins' entries are compared first.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use flux_lsp::config::{Install, ServerConfig};
use flux_plugin::manifest::{
    GrammarSourceSpec, InstallSpec, LanguageServerSpec, LanguageSpec, QueryPrecedence,
};
use flux_plugin::registry::{PluginEntry, PluginSource};
use flux_syntax::{GrammarSource, LanguageConfig, Precedence};
use gpui::{App, Global};

use crate::settings;

/// The contributions in place.
#[derive(Default)]
pub struct Contributions {
    /// The plugins whose contributions are in place, in order.
    plugins: Vec<Arc<PluginEntry>>,
    /// The language servers of those plugins, with the files they serve.
    servers: Vec<ServerConfig>,
    /// The plugin that brings each language server, by the server's name.
    server_plugins: HashMap<String, String>,
    /// Language id → the id language servers know it by.
    lsp_ids: HashMap<String, String>,
    /// What went wrong reading a plugin's contributions, by plugin id.
    problems: HashMap<String, Vec<String>>,
    /// Bumps on every change.
    generation: u64,
}

impl Global for Contributions {}

/// Puts the contributions of the turned-on plugins in place before the window opens: the plugins
/// are found as the store will find them (bundled, installed, under development).
pub fn init(cx: &mut App) {
    cx.set_global(Contributions::default());
    let dev = settings::dev_plugins(cx);
    let scan = flux_plugin::registry::scan(crate::bundled::plugins(), &dev);
    let entries: Vec<Arc<PluginEntry>> = scan
        .plugins
        .into_iter()
        .filter(|entry| entry.problem.is_none() && settings::plugin_enabled(entry.id(), cx))
        .map(Arc::new)
        .collect();
    refresh(&entries, cx);
}

/// Puts the contributions of `plugins` (the turned-on ones, in the store's order) in place and
/// takes away those of the others. Returns the problems found now, by plugin id: the store writes
/// them into the plugins' logs.
pub fn refresh(plugins: &[Arc<PluginEntry>], cx: &mut App) -> Vec<(String, String)> {
    if !cx.has_global::<Contributions>() {
        cx.set_global(Contributions::default());
    }
    let unchanged = {
        let current = &cx.global::<Contributions>().plugins;
        current.len() == plugins.len()
            && current
                .iter()
                .zip(plugins)
                .all(|(a, b)| Arc::ptr_eq(a, b) || same_entry(a, b))
    };
    if unchanged {
        return Vec::new();
    }
    let Resolved {
        languages,
        servers,
        server_plugins,
        lsp_ids,
        mut problems,
    } = resolve(plugins);

    // Languages: registered per plugin; the plugins that are gone take theirs away.
    let registered: HashSet<String> = cx
        .global::<Contributions>()
        .plugins
        .iter()
        .map(|entry| entry.id().to_string())
        .collect();
    let now: HashSet<String> = plugins.iter().map(|entry| entry.id().to_string()).collect();
    for gone in registered.difference(&now) {
        flux_syntax::unregister(gone);
    }
    for (owner, configs) in languages {
        flux_syntax::register(&owner, configs);
    }

    // Themes and icons.
    problems.extend(crate::theme::load_plugin_themes(plugins, cx));
    problems.extend(crate::icon_themes::load_plugin_icons(plugins, cx));

    let contributions = cx.global_mut::<Contributions>();
    contributions.plugins = plugins.to_vec();
    contributions.servers = servers;
    contributions.server_plugins = server_plugins;
    contributions.lsp_ids = lsp_ids;
    contributions.problems.clear();
    for (plugin, problem) in &problems {
        contributions
            .problems
            .entry(plugin.clone())
            .or_default()
            .push(problem.clone());
    }
    contributions.generation += 1;
    problems
}

/// What the turned-on plugins add, read from their files: nothing is put in place yet.
#[derive(Default)]
pub(crate) struct Resolved {
    /// The languages of each plugin, in the plugins' order (a plugin without languages too: its
    /// earlier ones go).
    pub(crate) languages: Vec<(String, Vec<LanguageConfig>)>,
    pub(crate) servers: Vec<ServerConfig>,
    pub(crate) server_plugins: HashMap<String, String>,
    pub(crate) lsp_ids: HashMap<String, String>,
    /// (plugin id, what went wrong).
    pub(crate) problems: Vec<(String, String)>,
}

/// Reads the languages and language servers of `plugins` (the turned-on ones, in order).
pub(crate) fn resolve(plugins: &[Arc<PluginEntry>]) -> Resolved {
    let mut resolved = Resolved::default();
    for entry in plugins {
        let mut configs = Vec::new();
        for language in &entry.manifest.languages {
            match language_config(entry, language) {
                Ok(config) => configs.push(config),
                Err(problem) => resolved.problems.push((entry.id().to_string(), problem)),
            }
            resolved
                .lsp_ids
                .insert(language.id.clone(), language.lsp_id().to_string());
        }
        resolved.languages.push((entry.id().to_string(), configs));
    }
    // Language servers: the files of the languages they name, of any plugin. A server's name is
    // its key (its folder among installed servers): a later plugin with the same server replaces
    // the earlier one's config, in its place.
    let languages: Vec<&LanguageSpec> = plugins
        .iter()
        .flat_map(|entry| &entry.manifest.languages)
        .collect();
    for entry in plugins {
        for server in &entry.manifest.language_servers {
            let config = server_config(server, &languages);
            if config.extensions.is_empty() && config.file_names.is_empty() {
                resolved.problems.push((
                    entry.id().to_string(),
                    format!(
                        "{}: serves no files — none of its languages ({}) is installed",
                        server.id,
                        server.languages.join(", ")
                    ),
                ));
            }
            resolved
                .server_plugins
                .insert(config.name.clone(), entry.id().to_string());
            match resolved
                .servers
                .iter_mut()
                .find(|known| known.name == config.name)
            {
                Some(known) => *known = config,
                None => resolved.servers.push(config),
            }
        }
    }
    resolved
}

/// The same plugin as before: same source, manifest and files (a reload makes a new entry).
fn same_entry(a: &PluginEntry, b: &PluginEntry) -> bool {
    a.source == b.source
        && a.manifest == b.manifest
        && a.files.dir() == b.files.dir()
        && a.source != PluginSource::Dev
}

/// Bumps on every change of the contributions: for those who keep what they derived from them and
/// compare it later (observers of the global are told anyway).
#[allow(dead_code)]
pub fn generation(cx: &App) -> u64 {
    cx.try_global::<Contributions>()
        .map_or(0, |contributions| contributions.generation)
}

/// The language servers of the turned-on plugins, in their order: for a file, the first one that
/// serves it is the main one.
pub fn servers(cx: &App) -> Vec<ServerConfig> {
    cx.try_global::<Contributions>()
        .map(|contributions| contributions.servers.clone())
        .unwrap_or_default()
}

/// The plugin that brings the language server `name` (its id).
pub fn server_plugin(name: &str, cx: &App) -> Option<String> {
    cx.try_global::<Contributions>()
        .and_then(|contributions| contributions.server_plugins.get(name).cloned())
}

/// The id language servers know a file's language by (`textDocument/didOpen`): the language's
/// `lsp-id`, or its id; "plaintext" for a file without a language.
pub fn lsp_language_id(path: &Path, cx: &App) -> String {
    let Some(language) = flux_syntax::language_for_path(path) else {
        return "plaintext".into();
    };
    cx.try_global::<Contributions>()
        .and_then(|contributions| contributions.lsp_ids.get(language.name()).cloned())
        .unwrap_or_else(|| language.name().to_string())
}

/// What went wrong reading a plugin's contributions (a missing query, a broken theme).
pub fn problems(plugin: &str, cx: &App) -> Vec<String> {
    cx.try_global::<Contributions>()
        .and_then(|contributions| contributions.problems.get(plugin).cloned())
        .unwrap_or_default()
}

/// A plugin's language, resolved: the query files read and joined, the grammar's bytes read.
fn language_config(entry: &PluginEntry, language: &LanguageSpec) -> Result<LanguageConfig, String> {
    let mut highlights = Vec::new();
    for file in &language.highlights {
        let bytes = entry
            .files
            .read(file)
            .ok_or_else(|| format!("{}: {file} is missing", language.id))?;
        let text = String::from_utf8(bytes.into_owned())
            .map_err(|_| format!("{}: {file} is not UTF-8", language.id))?;
        highlights.push(text);
    }
    let grammar = entry
        .manifest
        .grammar(&language.grammar)
        .ok_or_else(|| format!("{}: no grammar {}", language.id, language.grammar))?;
    let grammar = match &grammar.source {
        GrammarSourceSpec::Builtin(name) => {
            if !flux_syntax::builtin_grammar_names().contains(&name.as_str()) {
                return Err(format!("{}: Flux has no grammar \"{name}\"", language.id));
            }
            GrammarSource::Builtin(name.clone())
        }
        GrammarSourceSpec::Wasm { path, symbol } => {
            let bytes = entry
                .files
                .read(path)
                .ok_or_else(|| format!("{}: {path} is missing", language.id))?;
            GrammarSource::Wasm {
                name: symbol.clone(),
                bytes: Arc::from(bytes.into_owned()),
            }
        }
    };
    Ok(LanguageConfig {
        name: language.id.clone(),
        display_name: entry
            .translate(crate::i18n::lang_code(), &language.name)
            .to_string(),
        extensions: language.extensions.clone(),
        file_names: language.file_names.clone(),
        aliases: language.aliases.clone(),
        grammar,
        highlights: highlights.join("\n"),
        precedence: match language.precedence {
            QueryPrecedence::LastPattern => Precedence::LastPattern,
            QueryPrecedence::FirstPattern => Precedence::FirstPattern,
        },
    })
}

/// A plugin's language server as the window's language servers take it: the files it serves are
/// its own `extensions` and `file-names`, otherwise those of its languages.
fn server_config(server: &LanguageServerSpec, languages: &[&LanguageSpec]) -> ServerConfig {
    let (extensions, file_names) = if server.extensions.is_empty() && server.file_names.is_empty() {
        let served = languages
            .iter()
            .filter(|language| server.languages.contains(&language.id));
        let mut extensions = Vec::new();
        let mut file_names = Vec::new();
        for language in served {
            extensions.extend(language.extensions.iter().cloned());
            file_names.extend(language.file_names.iter().cloned());
        }
        (extensions, file_names)
    } else {
        (server.extensions.clone(), server.file_names.clone())
    };
    ServerConfig {
        name: server.id.clone(),
        command: server.command.clone(),
        args: server.args.clone(),
        extensions,
        file_names,
        initialization_options: server.initialization_options.clone(),
        settings: server.settings.clone(),
        install: server.install.as_ref().map(install),
    }
}

fn install(spec: &InstallSpec) -> Install {
    match spec {
        InstallSpec::Npm { packages, bin } => Install::Npm {
            packages: packages.clone(),
            bin: bin.clone(),
        },
        InstallSpec::GitHub { repo, asset, bin } => Install::GitHubRelease {
            repo: repo.clone(),
            asset: asset.clone(),
            bin: bin.clone(),
        },
        InstallSpec::Go { package, bin } => Install::GoInstall {
            package: package.clone(),
            bin: bin.clone(),
        },
        InstallSpec::Rustup {
            component,
            fallback,
        } => Install::Rustup {
            component: component.clone(),
            fallback: Box::new(install(fallback)),
        },
    }
}

#[cfg(test)]
mod tests {
    use flux_plugin::manifest::Manifest;
    use flux_plugin::registry::{PluginFiles, PluginSource};

    use super::*;

    const MANIFEST: &str = r#"
id = "test.lang"
name = "Lang"
version = "1.0.0"
api = "0.2"

[[languages]]
id = "zzlang"
name = "ZZ Lang"
extensions = ["zz"]
grammar = "javascript"
highlights = ["queries/a.scm", "queries/b.scm"]
lsp-id = "zz"

[[grammars]]
id = "javascript"
builtin = "javascript"

[[language-servers]]
id = "zz-ls"
command = "zz-ls"
languages = ["zzlang"]
install = { npm = ["zz-ls"], bin = "zz-ls" }
"#;

    static FILES: &[(&str, &[u8])] = &[
        ("queries/a.scm", b"(string) @string"),
        ("queries/b.scm", b"(number) @number"),
    ];

    fn entry() -> PluginEntry {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        PluginEntry {
            manifest,
            files: PluginFiles::Embedded(FILES),
            source: PluginSource::Bundled,
            locales: Default::default(),
            problem: None,
        }
    }

    #[test]
    fn a_language_reads_its_queries_and_grammar() {
        let entry = entry();
        let config = language_config(&entry, &entry.manifest.languages[0]).unwrap();
        assert_eq!(config.name, "zzlang");
        assert_eq!(config.highlights, "(string) @string\n(number) @number");
        assert_eq!(config.grammar, GrammarSource::Builtin("javascript".into()));
        let mut broken = entry.manifest.languages[0].clone();
        broken.highlights.push("queries/missing.scm".into());
        assert!(
            language_config(&entry, &broken)
                .unwrap_err()
                .contains("queries/missing.scm")
        );
    }

    #[test]
    fn a_server_serves_its_languages_files() {
        let entry = entry();
        let languages: Vec<&LanguageSpec> = entry.manifest.languages.iter().collect();
        let config = server_config(&entry.manifest.language_servers[0], &languages);
        assert_eq!(config.name, "zz-ls");
        assert_eq!(config.extensions, ["zz"]);
        assert_eq!(
            config.install,
            Some(Install::Npm {
                packages: vec!["zz-ls".into()],
                bin: "zz-ls".into()
            })
        );
    }

    /// A plugin of `manifest` with the query files of [`FILES`].
    fn plugin(manifest: &str, source: PluginSource) -> Arc<PluginEntry> {
        Arc::new(PluginEntry {
            manifest: Manifest::parse(manifest).unwrap(),
            files: PluginFiles::Embedded(FILES),
            source,
            locales: Default::default(),
            problem: None,
        })
    }

    #[test]
    fn plugins_resolve_into_languages_servers_and_owners() {
        // A second plugin brings its own zz-ls (a newer one): it replaces the first one's config,
        // in its place; another server serves files of the first plugin's language.
        let second = r#"
id = "test.more"
name = "More"
version = "1.0.0"
api = "0.2"

[[language-servers]]
id = "zz-ls"
command = "zz-ls-2"
languages = ["zzlang"]

[[language-servers]]
id = "zz-lint"
command = "zz-lint"
extensions = ["zz"]
"#;
        let plugins = [
            plugin(MANIFEST, PluginSource::Bundled),
            plugin(second, PluginSource::Installed),
        ];
        let resolved = resolve(&plugins);
        assert!(resolved.problems.is_empty(), "{:?}", resolved.problems);
        let owners: Vec<&str> = resolved.languages.iter().map(|(o, _)| o.as_str()).collect();
        assert_eq!(owners, ["test.lang", "test.more"]);
        assert_eq!(resolved.languages[0].1.len(), 1);
        assert!(resolved.languages[1].1.is_empty());
        let names: Vec<&str> = resolved.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["zz-ls", "zz-lint"]);
        assert_eq!(resolved.servers[0].command, "zz-ls-2");
        assert_eq!(resolved.servers[0].extensions, ["zz"]);
        assert_eq!(resolved.server_plugins["zz-ls"], "test.more");
        assert_eq!(resolved.server_plugins["zz-lint"], "test.more");
        assert_eq!(resolved.lsp_ids["zzlang"], "zz");
    }

    #[test]
    fn what_cant_be_read_is_a_problem_of_its_plugin() {
        let broken = MANIFEST
            .replace("\"queries/b.scm\"", "\"queries/missing.scm\"")
            .replace("builtin = \"javascript\"", "builtin = \"cobol\"");
        let resolved = resolve(&[plugin(&broken, PluginSource::Bundled)]);
        assert_eq!(resolved.problems.len(), 1, "{:?}", resolved.problems);
        assert_eq!(resolved.problems[0].0, "test.lang");
        assert!(resolved.problems[0].1.contains("queries/missing.scm"));
        // The language is left out; its server serves no files.
        assert!(resolved.languages[0].1.is_empty());
        let unknown = MANIFEST.replace("builtin = \"javascript\"", "builtin = \"cobol\"");
        let resolved = resolve(&[plugin(&unknown, PluginSource::Bundled)]);
        assert!(resolved.problems[0].1.contains("cobol"), "{:?}", resolved.problems);
        let lonely = r#"
id = "test.lonely"
name = "Lonely"
version = "1.0.0"
api = "0.2"

[[language-servers]]
id = "lonely-ls"
command = "lonely-ls"
languages = ["nothing"]
"#;
        let resolved = resolve(&[plugin(lonely, PluginSource::Installed)]);
        assert_eq!(resolved.servers.len(), 1);
        assert!(resolved.problems[0].1.contains("serves no files"));
    }

    #[test]
    fn a_reloaded_plugin_under_development_is_not_the_same() {
        let bundled = plugin(MANIFEST, PluginSource::Bundled);
        let again = plugin(MANIFEST, PluginSource::Bundled);
        assert!(same_entry(&bundled, &again));
        let dev = plugin(MANIFEST, PluginSource::Dev);
        let reloaded = plugin(MANIFEST, PluginSource::Dev);
        assert!(!same_entry(&dev, &reloaded), "its files may have changed");
    }
}
