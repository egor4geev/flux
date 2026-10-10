//! The plugins' items in the context menus (part 8.2): the editor's, the project tree's and a
//! tab's, from the manifests' `[[menus]]`. Under the menu's own items, after a separator, come the
//! commands of the running plugins that fit what the menu is opened on (`when`: a selection, a
//! file, a folder) and that the plugin hasn't grayed out — the plugins by name, a plugin's single
//! item as it is, several in a submenu named after the plugin (as Claude ›, Git ›). A command run
//! from a menu gets its context ([`CommandOrigin`]): the menu's source and the files it was
//! opened on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use flux_plugin::api::types::CommandSource;
use flux_plugin::manifest::{MenuLocation, MenuWhen};
use flux_plugin::registry::PluginEntry;
use gpui::{App, Entity};

use crate::context_menu::ContextMenu;
use crate::i18n::lang_code;
use crate::plugins::{CommandOrigin, PluginStore, RunCommand};

/// What a context menu is opened on.
pub(crate) struct MenuTarget {
    pub location: MenuLocation,
    /// The editor has a selection (the editor's menu).
    pub selection: bool,
    /// The files and folders: the rows selected in the tree, a tab's file, the document's file.
    pub paths: Vec<PathBuf>,
}

/// Adds the plugins' items for `target` to the end of `menu`; without any, the menu stays as it is.
pub(crate) fn append(
    menu: ContextMenu,
    plugins: &Entity<PluginStore>,
    target: &MenuTarget,
    cx: &App,
) -> ContextMenu {
    let store = plugins.read(cx);
    let groups = groups(
        store.plugins().iter().map(|state| &*state.entry),
        target,
        lang_code(),
        |plugin, command| store.command_enabled(plugin, command),
        Path::is_dir,
    );
    add_groups(menu, groups)
}

/// One plugin's items in a menu.
#[derive(Debug, Clone, PartialEq)]
struct Group {
    /// The plugin's name in the interface language: the title of its submenu.
    name: String,
    items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
struct Item {
    /// The command's title in the interface language.
    label: String,
    action: RunCommand,
}

fn add_groups(mut menu: ContextMenu, groups: Vec<Group>) -> ContextMenu {
    if groups.is_empty() {
        return menu;
    }
    menu = menu.separator();
    for group in groups {
        menu = match group.items.as_slice() {
            [item] => menu.entry(item.label.clone(), item.action.clone()),
            items => menu.submenu(group.name, |mut submenu| {
                for item in items {
                    submenu = submenu.entry(item.label.clone(), item.action.clone());
                }
                submenu
            }),
        };
    }
    menu
}

/// The plugins' items for `target`, a group per plugin with any, ordered by the plugins' names.
/// `enabled` — the plugin runs and hasn't grayed the command out; `is_dir` — a path is a folder.
fn groups<'a>(
    plugins: impl IntoIterator<Item = &'a PluginEntry>,
    target: &MenuTarget,
    language: &str,
    enabled: impl Fn(&str, &str) -> bool,
    is_dir: impl Fn(&Path) -> bool,
) -> Vec<Group> {
    let source = source_of(target.location);
    let mut groups: Vec<Group> = plugins
        .into_iter()
        .filter_map(|entry| {
            let manifest = &entry.manifest;
            // A command listed twice for the same menu (a `when` each) shows once.
            let mut seen = HashSet::new();
            let items: Vec<Item> = manifest
                .menus
                .iter()
                .filter(|menu| menu.location == target.location)
                .filter(|menu| fits(menu.when, target, &is_dir))
                .filter(|menu| seen.insert(menu.command.as_str()))
                .filter(|menu| enabled(entry.id(), &menu.command))
                .filter_map(|menu| {
                    let command = manifest.command(&menu.command)?;
                    Some(Item {
                        label: entry.translate(language, &command.title).to_string(),
                        action: RunCommand {
                            plugin: entry.id().to_string().into(),
                            command: command.id.clone().into(),
                            origin: Some(CommandOrigin::with_paths(source, target.paths.clone())),
                        },
                    })
                })
                .collect();
            (!items.is_empty()).then(|| Group {
                name: entry.translate(language, &manifest.name).to_string(),
                items,
            })
        })
        .collect();
    groups.sort_by_cached_key(|group| group.name.to_lowercase());
    groups
}

/// Whether an item's `when` fits what the menu is opened on.
fn fits(when: Option<MenuWhen>, target: &MenuTarget, is_dir: impl Fn(&Path) -> bool) -> bool {
    let all = |folder: bool| {
        !target.paths.is_empty() && target.paths.iter().all(|path| is_dir(path) == folder)
    };
    match when {
        None => true,
        Some(MenuWhen::Selection) => target.selection,
        Some(MenuWhen::File) => all(false),
        Some(MenuWhen::Folder) => all(true),
    }
}

/// Where a command of a menu was run from, for its context.
fn source_of(location: MenuLocation) -> CommandSource {
    match location {
        MenuLocation::Editor => CommandSource::EditorMenu,
        MenuLocation::Tree => CommandSource::TreeMenu,
        MenuLocation::Tab => CommandSource::TabMenu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_plugin::locales::Locales;
    use flux_plugin::manifest::Manifest;
    use flux_plugin::registry::{PluginFiles, PluginSource};

    /// A plugin with commands `a`, `b`, `c` and the given `[[menus]]`.
    fn plugin(id: &str, name: &str, menus: &str) -> PluginEntry {
        let manifest = format!(
            "id = \"{id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi = \"0.2\"\n\n\
             [[commands]]\nid = \"a\"\ntitle = \"Alpha\"\n\n\
             [[commands]]\nid = \"b\"\ntitle = \"Beta\"\n\n\
             [[commands]]\nid = \"c\"\ntitle = \"Gamma\"\n\n{menus}"
        );
        PluginEntry {
            manifest: Manifest::parse(&manifest).unwrap(),
            files: PluginFiles::Embedded(&[]),
            source: PluginSource::Dev,
            locales: Locales::default(),
            problem: None,
        }
    }

    fn menu(command: &str, location: &str, when: Option<&str>) -> String {
        let when = when.map_or(String::new(), |when| format!("when = \"{when}\"\n"));
        format!("[[menus]]\ncommand = \"{command}\"\nlocation = \"{location}\"\n{when}\n")
    }

    fn target(location: MenuLocation, selection: bool, paths: &[&str]) -> MenuTarget {
        MenuTarget {
            location,
            selection,
            paths: paths.iter().map(PathBuf::from).collect(),
        }
    }

    /// Paths ending with `/` are folders.
    fn is_dir(path: &Path) -> bool {
        path.to_string_lossy().ends_with('/')
    }

    /// The labels per group: "Plugin: Label, Label".
    fn labels(
        plugins: &[PluginEntry],
        target: &MenuTarget,
        enabled: impl Fn(&str, &str) -> bool,
    ) -> Vec<String> {
        groups(plugins, target, "en", enabled, is_dir)
            .into_iter()
            .map(|group| {
                let items: Vec<&str> = group.items.iter().map(|item| item.label.as_str()).collect();
                format!("{}: {}", group.name, items.join(", "))
            })
            .collect()
    }

    fn all(_: &str, _: &str) -> bool {
        true
    }

    #[test]
    fn items_go_to_their_menu_when_they_fit() {
        let menus = [
            menu("a", "editor", None),
            menu("b", "editor", Some("selection")),
            menu("c", "tree", Some("folder")),
            menu("a", "tree", Some("file")),
            menu("b", "tab", Some("file")),
        ]
        .concat();
        let plugins = [plugin("x.tools", "Tools", &menus)];
        let editor = |selection| target(MenuLocation::Editor, selection, &["/p/main.rs"]);
        assert_eq!(labels(&plugins, &editor(false), all), ["Tools: Alpha"]);
        assert_eq!(labels(&plugins, &editor(true), all), ["Tools: Alpha, Beta"]);
        let tree = |path| target(MenuLocation::Tree, false, &[path]);
        assert_eq!(labels(&plugins, &tree("/p/src/"), all), ["Tools: Gamma"]);
        assert_eq!(labels(&plugins, &tree("/p/main.rs"), all), ["Tools: Alpha"]);
        assert_eq!(
            labels(
                &plugins,
                &target(MenuLocation::Tab, false, &["/p/main.rs"]),
                all
            ),
            ["Tools: Beta"]
        );
        // A new document's tab has no file.
        assert!(labels(&plugins, &target(MenuLocation::Tab, false, &[]), all).is_empty());
    }

    #[test]
    fn grayed_out_commands_and_stopped_plugins_add_nothing() {
        let plugins = [plugin(
            "x.tools",
            "Tools",
            &[menu("a", "editor", None), menu("b", "editor", None)].concat(),
        )];
        let target = target(MenuLocation::Editor, false, &["/p/main.rs"]);
        assert_eq!(
            labels(&plugins, &target, |_, command| command != "a"),
            ["Tools: Beta"]
        );
        // A plugin that doesn't run has every command off: no group, so no separator either.
        assert!(groups(&plugins, &target, "en", |_, _| false, is_dir).is_empty());
    }

    #[test]
    fn plugins_go_by_name_and_a_command_shows_once() {
        let plugins = [
            plugin("z.one", "zeta", &menu("a", "editor", None)),
            plugin(
                "a.two",
                "Alpha Tools",
                &[
                    menu("a", "editor", None),
                    menu("a", "editor", Some("selection")),
                    menu("b", "editor", None),
                ]
                .concat(),
            ),
        ];
        let target = target(MenuLocation::Editor, true, &["/p/main.rs"]);
        assert_eq!(
            labels(&plugins, &target, all),
            ["Alpha Tools: Alpha, Beta", "zeta: Alpha"]
        );
    }

    #[test]
    fn an_item_runs_its_command_with_the_menus_context() {
        let plugins = [plugin(
            "x.tools",
            "Tools",
            &menu("c", "tree", Some("folder")),
        )];
        let target = target(MenuLocation::Tree, false, &["/p/src/"]);
        let groups = groups(&plugins, &target, "en", all, is_dir);
        assert_eq!(
            groups[0].items[0].action,
            RunCommand {
                plugin: "x.tools".into(),
                command: "c".into(),
                origin: Some(CommandOrigin::with_paths(
                    CommandSource::TreeMenu,
                    vec![PathBuf::from("/p/src/")]
                )),
            }
        );
        assert_eq!(source_of(MenuLocation::Editor), CommandSource::EditorMenu);
        assert_eq!(source_of(MenuLocation::Tab), CommandSource::TabMenu);
    }

    #[test]
    fn labels_are_translated() {
        static FILES: &[(&str, &[u8])] = &[(
            "locales/ru.toml",
            "\"Tools\" = \"Инструменты\"\n\"Alpha\" = \"Альфа\"\n".as_bytes(),
        )];
        let mut entry = plugin("x.tools", "Tools", &menu("a", "editor", None));
        entry.files = PluginFiles::Embedded(FILES);
        entry.locales = Locales::load(&entry.files);
        let target = target(MenuLocation::Editor, false, &["/p/main.rs"]);
        let groups = groups([&entry], &target, "ru", all, is_dir);
        assert_eq!(groups[0].name, "Инструменты");
        assert_eq!(groups[0].items[0].label, "Альфа");
    }

    #[test]
    fn several_paths_fit_only_when_all_do() {
        let plugins = [plugin("x.tools", "Tools", &menu("a", "tree", Some("file")))];
        let mixed = target(MenuLocation::Tree, false, &["/p/a.rs", "/p/src/"]);
        assert!(labels(&plugins, &mixed, all).is_empty());
        let files = target(MenuLocation::Tree, false, &["/p/a.rs", "/p/b.rs"]);
        assert_eq!(labels(&plugins, &files, all), ["Tools: Alpha"]);
    }
}
