//! Which language server serves which files, and how to start it. The configs come from the
//! language plugins (stage 8.3: `[[language-servers]]` of their manifests, turned into a
//! [`ServerConfig`] by the application), with the user's settings over them (part 10.5:
//! [`with_user_settings`]); this module picks the servers of a file and finds their executables.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::install;

/// How to start a language server and which files it serves.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// Display name, also the key: "rust-analyzer".
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// File extensions (without the dot) and exact file names this server serves.
    pub extensions: Vec<String>,
    pub file_names: Vec<String>,
    /// `initializationOptions` for `initialize`.
    pub initialization_options: Option<serde_json::Value>,
    /// Answers to `workspace/configuration`, by section (`{"python": {"analysis": …}}`); the client
    /// adds what it finds in the project at start (the Python virtual environment). `None` — the
    /// server's defaults.
    pub settings: Option<serde_json::Value>,
    /// How Flux installs the server when it is not on the machine; `None` — it can't.
    pub install: Option<Install>,
    /// More environment variables for the server's process (the user's settings, part 10.5:
    /// `CARGO_TARGET_DIR`, `NODE_OPTIONS`); they win over Flux's own `PATH`.
    pub env: BTreeMap<String, String>,
}

/// The plugin's `config` with the user's settings of the server over it (part 10.5: `lsp.<server>`
/// in Flux's `settings.json`); `None` — the user turned the server off (`"enabled": false`).
/// - `settings` (the answers to `workspace/configuration`, by section) and
///   `initialization_options` merge into the plugin's, object by object; any other value replaces;
///   empty objects add nothing;
/// - `command` (a name looked up in `PATH`, or a path — `~` is the home directory) and `args`
///   replace the plugin's; a server started with the user's own command is not installed by Flux;
/// - `env` adds environment variables.
///
/// Values of the wrong type are ignored, so a typo doesn't stop the server.
pub fn with_user_settings(mut config: ServerConfig, user: &Value) -> Option<ServerConfig> {
    let Some(user) = user.as_object() else {
        return Some(config);
    };
    if user.get("enabled") == Some(&Value::Bool(false)) {
        return None;
    }
    let object = |key: &str| {
        user.get(key)
            .filter(|value| value.is_object())
            .and_then(pruned)
    };
    if let Some(settings) = object("settings") {
        merge_into(&mut config.settings, settings);
    }
    if let Some(options) = object("initialization_options") {
        merge_into(&mut config.initialization_options, options);
    }
    if let Some(command) = user
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        config.command = expand_home(
            command,
            flux_platform::paths::home()
                .map(std::ffi::OsString::from)
                .as_deref(),
        );
        config.install = None;
    }
    if let Some(args) = user.get("args").and_then(Value::as_array) {
        config.args = args
            .iter()
            .filter_map(|arg| Some(arg.as_str()?.to_string()))
            .collect();
    }
    if let Some(env) = user.get("env").and_then(Value::as_object) {
        for (key, value) in env {
            let value = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(on) => on.to_string(),
                _ => continue,
            };
            config.env.insert(key.clone(), value);
        }
    }
    Some(config)
}

/// Whether `a` and `b` differ in nothing but `settings`: a running server takes new settings
/// without a restart (`workspace/didChangeConfiguration`).
pub fn differs_only_in_settings(a: &ServerConfig, b: &ServerConfig) -> bool {
    a.settings != b.settings
        && ServerConfig {
            settings: None,
            ..a.clone()
        } == ServerConfig {
            settings: None,
            ..b.clone()
        }
}

/// `over` merged into `base`: objects key by key (deeper objects the same way), anything else
/// replaces.
pub fn deep_merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(existing) => deep_merge(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

fn merge_into(base: &mut Option<Value>, over: Value) {
    match base {
        Some(base) => deep_merge(base, &over),
        None => *base = Some(over),
    }
}

/// `value` without empty objects (`{"rust-analyzer": {}}` — a key "Configure…" added and nobody
/// filled in); `None` if nothing is left.
fn pruned(value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            let map: serde_json::Map<String, Value> = map
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), pruned(value)?)))
                .collect();
            (!map.is_empty()).then_some(Value::Object(map))
        }
        other => Some(other.clone()),
    }
}

/// `~/bin/x` → `<home>/bin/x`; other commands as they are.
fn expand_home(command: &str, home: Option<&std::ffi::OsStr>) -> String {
    match (command.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(home).join(rest).to_string_lossy().into_owned(),
        _ => command.to_string(),
    }
}

/// Where a server comes from when Flux installs it ([`crate::install`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Install {
    /// npm packages (the server first, then what it needs, e.g. `typescript`), installed with a
    /// Node.js that Flux downloads for itself; `bin` — the executable in `node_modules/.bin`.
    Npm { packages: Vec<String>, bin: String },
    /// A binary from the latest GitHub release of `repo`. `asset` — the file name for this machine,
    /// with `{arch}` (`aarch64`, `x86_64`) and `{tag}` (the release tag) filled in; an archive
    /// (`.tar.gz`, `.tar.xz`, `.zip`, `.gz`) or the binary itself. `bin` — its name inside.
    GitHubRelease {
        repo: String,
        asset: String,
        bin: String,
    },
    /// `go install package@latest` (needs Go).
    GoInstall { package: String, bin: String },
    /// `rustup component add component` (needs rustup), otherwise `fallback`.
    Rustup {
        component: String,
        fallback: Box<Install>,
    },
}

/// Every config that serves `path`, the main server first (Python: pyright, then ruff for formatting
/// and linting): those that name the file exactly if any, otherwise those for its extension (any
/// case), in the order of `configs`.
pub fn servers_for_path<'a>(configs: &'a [ServerConfig], path: &Path) -> Vec<&'a ServerConfig> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let by_name: Vec<&ServerConfig> = configs
        .iter()
        .filter(|config| config.file_names.iter().any(|name| name == file_name))
        .collect();
    if !by_name.is_empty() {
        return by_name;
    }
    let Some(extension) = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return Vec::new();
    };
    configs
        .iter()
        .filter(|config| config.extensions.contains(&extension))
        .collect()
}

/// The main config that serves `path` — the first of [`servers_for_path`].
pub fn server_for_path<'a>(configs: &'a [ServerConfig], path: &Path) -> Option<&'a ServerConfig> {
    servers_for_path(configs, path).into_iter().next()
}

impl ServerConfig {
    /// The executable: `command` looked up in `PATH` and, because an app started from Finder gets
    /// a minimal `PATH`, also in the usual install locations (`~/.cargo/bin`, `~/go/bin`,
    /// `/opt/homebrew/bin`, `/usr/local/bin`, rustup's bin), then among the servers Flux installed
    /// ([`crate::install`]). `None` if it is not installed.
    pub fn resolve_command(&self) -> Option<PathBuf> {
        self.resolve_system_command()
            .or_else(|| install::installed_command(self))
    }

    /// The executable installed on the machine itself (`PATH` and the usual install locations),
    /// not by Flux.
    pub fn resolve_system_command(&self) -> Option<PathBuf> {
        let command = Path::new(&self.command);
        if command.components().count() > 1 {
            return is_executable(command).then(|| command.to_path_buf());
        }
        search_path()
            .into_iter()
            .map(|dir| dir.join(command))
            .find(|path| is_executable(path))
            .filter(|path| !self.is_bare_rustup_proxy(path))
    }

    /// rustup puts a proxy for `rust-analyzer` next to itself whether or not the toolchain has the
    /// component; without it the proxy only prints an error. `rustup which` tells them apart.
    fn is_bare_rustup_proxy(&self, path: &Path) -> bool {
        let Some(Install::Rustup { component, .. }) = &self.install else {
            return false;
        };
        let Some(rustup) = path.parent().map(|dir| dir.join("rustup")) else {
            return false;
        };
        if !is_executable(&rustup) {
            return false;
        }
        let has_component = Command::new(rustup)
            .args(["which", component])
            .env("PATH", search_path_env())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        !has_component
    }
}

/// What a server gets at start beyond its config: its `PATH`, and settings and options that depend
/// on the project and on what Flux installed.
pub(crate) struct Launch {
    pub path: OsString,
    /// Answers to `workspace/configuration` (an object; sections are looked up in it).
    pub settings: Value,
    pub initialization_options: Option<Value>,
}

/// [`Launch`] for `config` started as `program` in `root`:
/// - pyright gets the project's virtual environment as `python.pythonPath` — otherwise it resolves
///   imports against whatever `python3` is first in `PATH`, and installed packages are "missing";
/// - typescript-language-server gets the TypeScript installed next to it as the fallback, used when
///   the project has no `node_modules/typescript` of its own;
/// - an npm server installed by Flux runs with Flux's Node.js first in `PATH`, and the tools Flux
///   installed for servers (shellcheck) are at the end of it.
pub(crate) fn launch(config: &ServerConfig, root: &Path, program: &Path) -> Launch {
    let settings = workspace_settings(config, root);
    let mut initialization_options = config.initialization_options.clone();
    if config.name == "typescript-language-server"
        && let Some(lib) = typescript_beside(program)
    {
        let options = initialization_options.get_or_insert_with(|| json!({}));
        // The user's own options may hold anything: only an object takes the key.
        if options.is_object()
            && matches!(options.get("tsserver"), None | Some(Value::Object(_)))
            && options["tsserver"]["fallbackPath"].is_null()
        {
            options["tsserver"]["fallbackPath"] = json!(lib);
        }
    }
    let node = install::node_bin_for(config, program);
    let dirs = node
        .iter()
        .cloned()
        .chain(
            search_path()
                .into_iter()
                .filter(|dir| dir.is_dir() && Some(dir) != node.as_ref()),
        )
        .chain(install::tool_dirs());
    Launch {
        path: env::join_paths(dirs).unwrap_or_default(),
        settings,
        initialization_options,
    }
}

/// The answers to `workspace/configuration` of `config` started in `root` (an object; sections
/// are looked up in it): its settings, and pyright gets the project's virtual environment as
/// `python.pythonPath` when the settings don't name an interpreter. Also what a running server
/// takes when the user changes its settings.
pub fn workspace_settings(config: &ServerConfig, root: &Path) -> Value {
    let mut settings = match &config.settings {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => json!({}),
    };
    if config.name == "pyright"
        && matches!(settings.get("python"), None | Some(Value::Object(_)))
        && settings["python"]["pythonPath"].is_null()
        && let Some(python) = python_interpreter(root, env::var_os("VIRTUAL_ENV").as_deref())
    {
        settings["python"]["pythonPath"] = json!(python);
    }
    settings
}

/// The Python interpreter of the project's virtual environment: `.venv`, `venv` or `env` in the
/// root (a directory with `pyvenv.cfg`), otherwise the activated one (`VIRTUAL_ENV`).
fn python_interpreter(root: &Path, activated: Option<&std::ffi::OsStr>) -> Option<String> {
    let interpreter = |venv: &Path| {
        let python = venv.join("bin").join("python");
        (venv.join("pyvenv.cfg").is_file() && python.exists())
            .then(|| python.to_string_lossy().into_owned())
    };
    [".venv", "venv", "env"]
        .iter()
        .find_map(|name| interpreter(&root.join(name)))
        .or_else(|| activated.and_then(|venv| interpreter(Path::new(venv))))
}

/// `node_modules/typescript/lib` next to the typescript-language-server that runs as `program`
/// (`…/node_modules/.bin/typescript-language-server`), if TypeScript was installed with it.
fn typescript_beside(program: &Path) -> Option<String> {
    let node_modules = program.parent()?.parent()?;
    let lib = node_modules.join("typescript").join("lib");
    lib.join("tsserver.js")
        .is_file()
        .then(|| lib.to_string_lossy().into_owned())
}

/// The part of `settings` a `workspace/configuration` item asks for: `section` is a dotted path
/// (`python.analysis`); no section — all of them; nothing there — `null`.
pub(crate) fn settings_section(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|section| !section.is_empty()) else {
        return settings.clone();
    };
    section
        .split('.')
        .try_fold(settings, |value, key| value.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Where servers and the tools they run (`cargo`, `go`, `node`) are looked for: `PATH` first, then
/// the usual install locations that a GUI app's minimal `PATH` lacks — cargo and rustup, Homebrew,
/// Go, user-local bins (pipx), Node managers (Volta, nvm — newest version first), MacPorts, and
/// macOS's `/etc/paths` and `/etc/paths.d`. Also passed to servers as their `PATH`.
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        dirs.extend(env::split_paths(&path));
    }
    let home = flux_platform::paths::home();
    let in_home = |relative: &str| home.as_ref().map(|home| home.join(relative));

    if let Some(cargo_home) = env::var_os("CARGO_HOME") {
        dirs.push(PathBuf::from(cargo_home).join("bin"));
    }
    dirs.extend(in_home(".cargo/bin"));
    // Homebrew's rustup is keg-only: its proxies (cargo, rust-analyzer) are not linked into bin.
    dirs.push("/opt/homebrew/opt/rustup/bin".into());
    dirs.push("/usr/local/opt/rustup/bin".into());
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/opt/homebrew/sbin".into());
    dirs.push("/usr/local/bin".into());

    if let Some(gobin) = env::var_os("GOBIN") {
        dirs.push(gobin.into());
    }
    if let Some(gopath) = env::var_os("GOPATH") {
        dirs.extend(env::split_paths(&gopath).map(|dir| dir.join("bin")));
    }
    dirs.extend(in_home("go/bin"));
    dirs.extend(in_home(".local/bin"));

    dirs.extend(in_home(".volta/bin"));
    dirs.extend(in_home(".npm-global/bin"));
    dirs.extend(in_home(".bun/bin"));
    if let Some(nvm) = in_home(".nvm/versions/node") {
        dirs.extend(nvm_versions(&nvm));
    }
    dirs.push("/opt/local/bin".into());
    dirs.extend(system_paths());

    let mut unique = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !dir.as_os_str().is_empty() && !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// [`search_path`] as a `PATH` value: only the directories that exist.
pub(crate) fn search_path_env() -> OsString {
    let dirs = search_path().into_iter().filter(|dir| dir.is_dir());
    env::join_paths(dirs).unwrap_or_default()
}

/// `bin` directories of the Node versions installed by nvm, newest first.
fn nvm_versions(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u32>, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name
                .trim_start_matches('v')
                .split('.')
                .map(|part| part.parse().unwrap_or(0))
                .collect();
            (version, entry.path().join("bin"))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, bin)| bin).collect()
}

/// `/etc/paths` and the files in `/etc/paths.d`: what macOS's `path_helper` adds to a login
/// shell's `PATH`.
fn system_paths() -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from("/etc/paths")];
    if let Ok(entries) = fs::read_dir("/etc/paths.d") {
        let mut more: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        more.sort();
        files.extend(more);
    }
    files
        .iter()
        .filter_map(|file| fs::read_to_string(file).ok())
        .flat_map(|contents| {
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{default_servers, server};

    #[test]
    fn servers_by_file_name_and_extension() {
        let configs = default_servers();
        let name = |path: &str| server_for_path(&configs, Path::new(path)).map(|c| c.name.as_str());
        assert_eq!(name("src/main.rs"), Some("rust-analyzer"));
        assert_eq!(name("MAIN.RS"), Some("rust-analyzer"));
        assert_eq!(name("app.tsx"), Some("typescript-language-server"));
        assert_eq!(name("Cargo.lock"), Some("taplo"));
        assert_eq!(name("Cargo.toml"), Some("taplo"));
        assert_eq!(name("dir/.bashrc"), Some("bash-language-server"));
        assert_eq!(name("notes.md"), Some("marksman"));
        assert_eq!(name("Makefile"), None);
        assert_eq!(name("image.png"), None);
    }

    #[test]
    fn python_has_two_servers_and_the_main_one_comes_first() {
        let configs = default_servers();
        let names = |path: &str| -> Vec<&str> {
            servers_for_path(&configs, Path::new(path))
                .iter()
                .map(|c| c.name.as_str())
                .collect()
        };
        assert_eq!(names("app/main.py"), ["pyright", "ruff"]);
        assert_eq!(names("types.pyi"), ["pyright", "ruff"]);
        assert_eq!(names("script.pyw"), ["pyright"]);
        assert_eq!(names("lib.rs"), ["rust-analyzer"]);
        assert_eq!(names("Cargo.lock"), ["taplo"]);
        assert!(names("Makefile").is_empty());
        assert!(names("/").is_empty());
        let main = server_for_path(&configs, Path::new("main.py")).map(|c| c.name.as_str());
        assert_eq!(main, Some("pyright"));
    }

    #[test]
    fn every_built_in_server_can_be_installed() {
        for config in default_servers() {
            assert!(config.install.is_some(), "{}", config.name);
        }
    }

    #[test]
    fn settings_sections_by_dotted_path() {
        let settings = json!({ "python": { "pythonPath": "/p", "analysis": { "mode": "x" } } });
        assert_eq!(
            settings_section(&settings, Some("python.analysis")),
            json!({ "mode": "x" })
        );
        assert_eq!(
            settings_section(&settings, Some("python.pythonPath")),
            json!("/p")
        );
        assert_eq!(settings_section(&settings, Some("yaml")), Value::Null);
        assert_eq!(
            settings_section(&settings, Some("python.nope.deeper")),
            Value::Null
        );
        assert_eq!(settings_section(&settings, None), settings);
        assert_eq!(settings_section(&settings, Some("")), settings);
    }

    #[test]
    fn python_virtual_environment_of_the_project() {
        let root = tempfile::tempdir().unwrap();
        let venv = |dir: &Path| {
            fs::create_dir_all(dir.join("bin")).unwrap();
            fs::write(dir.join("bin/python"), "").unwrap();
            fs::write(dir.join("pyvenv.cfg"), "home = /usr/bin").unwrap();
        };
        assert_eq!(python_interpreter(root.path(), None), None);
        // A directory named `env` without pyvenv.cfg is not an environment.
        fs::create_dir_all(root.path().join("env/bin")).unwrap();
        fs::write(root.path().join("env/bin/python"), "").unwrap();
        assert_eq!(python_interpreter(root.path(), None), None);

        let activated = tempfile::tempdir().unwrap();
        venv(activated.path());
        let expected = activated.path().join("bin/python");
        assert_eq!(
            python_interpreter(root.path(), Some(activated.path().as_os_str())),
            Some(expected.to_string_lossy().into_owned())
        );
        // The project's own environment wins over the activated one.
        venv(&root.path().join(".venv"));
        let expected = root.path().join(".venv/bin/python");
        assert_eq!(
            python_interpreter(root.path(), Some(activated.path().as_os_str())),
            Some(expected.to_string_lossy().into_owned())
        );

        let configs = default_servers();
        let pyright = configs.iter().find(|c| c.name == "pyright").unwrap();
        let launch = launch(pyright, root.path(), Path::new("/bin/pyright-langserver"));
        assert_eq!(
            settings_section(&launch.settings, Some("python.pythonPath")),
            json!(expected.to_string_lossy())
        );
        let ruff = configs.iter().find(|c| c.name == "ruff").unwrap();
        let launch = super::launch(ruff, root.path(), Path::new("/bin/ruff"));
        assert_eq!(launch.settings, json!({}));
    }

    #[test]
    fn typescript_next_to_the_server_is_its_fallback() {
        let prefix = tempfile::tempdir().unwrap();
        let bin = prefix.path().join("node_modules/.bin");
        fs::create_dir_all(&bin).unwrap();
        let program = bin.join("typescript-language-server");
        fs::write(&program, "").unwrap();
        let configs = default_servers();
        let config = configs
            .iter()
            .find(|c| c.name == "typescript-language-server")
            .unwrap();
        // Without TypeScript next to it: no options.
        assert_eq!(
            launch(config, prefix.path(), &program).initialization_options,
            None
        );
        let lib = prefix.path().join("node_modules/typescript/lib");
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("tsserver.js"), "").unwrap();
        let options = launch(config, prefix.path(), &program).initialization_options;
        assert_eq!(
            options,
            Some(json!({ "tsserver": { "fallbackPath": lib.to_string_lossy() } }))
        );
    }

    #[test]
    fn user_settings_merge_over_the_plugins() {
        let plugin = ServerConfig {
            settings: Some(json!({ "rust-analyzer": { "cargo": { "features": "all" } } })),
            initialization_options: Some(json!({ "a": 1, "nested": { "keep": true } })),
            ..server("rust-analyzer", "rust-analyzer", &["--x"], &["rs"], &[])
        };
        let user = json!({
            "settings": { "rust-analyzer": { "check": { "command": "clippy" } } },
            "initialization_options": { "nested": { "added": [1, 2] }, "a": "replaced" },
            "args": ["--y", 3, "--z"],
            "env": { "CARGO_TARGET_DIR": "/tmp/ra", "N": 2, "B": true, "X": null },
        });
        let merged = with_user_settings(plugin.clone(), &user).unwrap();
        assert_eq!(
            merged.settings,
            Some(json!({
                "rust-analyzer": { "cargo": { "features": "all" }, "check": { "command": "clippy" } }
            }))
        );
        assert_eq!(
            merged.initialization_options,
            Some(json!({ "a": "replaced", "nested": { "keep": true, "added": [1, 2] } }))
        );
        assert_eq!(merged.args, ["--y", "--z"]);
        assert_eq!(
            merged.env,
            BTreeMap::from([
                ("B".to_string(), "true".to_string()),
                ("CARGO_TARGET_DIR".to_string(), "/tmp/ra".to_string()),
                ("N".to_string(), "2".to_string()),
            ])
        );
        // Untouched: the command, the files, the install recipe.
        assert_eq!(merged.command, "rust-analyzer");
        assert_eq!(merged.extensions, plugin.extensions);
        // Nothing, an empty object, empty sections or the wrong types change nothing.
        for user in [
            json!(null),
            json!({}),
            json!({ "settings": { "rust-analyzer": {} }, "initialization_options": {} }),
            json!({ "settings": 5, "args": "x", "env": [1], "command": "  ", "enabled": "no" }),
        ] {
            assert_eq!(
                with_user_settings(plugin.clone(), &user),
                Some(plugin.clone()),
                "{user}"
            );
        }
        // Settings where the plugin has none.
        let bare = server("gopls", "gopls", &[], &["go"], &[]);
        let merged = with_user_settings(bare, &json!({ "settings": { "gopls": { "a": 1 } } }));
        assert_eq!(
            merged.unwrap().settings,
            Some(json!({ "gopls": { "a": 1 } }))
        );
    }

    #[test]
    fn a_server_can_be_turned_off_or_given_another_command() {
        let plugin = ServerConfig {
            install: Some(Install::GoInstall {
                package: "golang.org/x/tools/gopls".into(),
                bin: "gopls".into(),
            }),
            ..server("gopls", "gopls", &[], &["go"], &[])
        };
        assert_eq!(
            with_user_settings(plugin.clone(), &json!({ "enabled": false })),
            None
        );
        assert!(with_user_settings(plugin.clone(), &json!({ "enabled": true })).is_some());
        let custom = with_user_settings(plugin.clone(), &json!({ "command": "/opt/gopls" }));
        let custom = custom.unwrap();
        assert_eq!(custom.command, "/opt/gopls");
        // Flux doesn't install the plugin's server in place of the user's own.
        assert_eq!(custom.install, None);
        assert_eq!(
            expand_home("~/bin/gopls", Some(std::ffi::OsStr::new("/Users/me"))),
            "/Users/me/bin/gopls"
        );
        assert_eq!(
            expand_home("gopls", Some(std::ffi::OsStr::new("/Users/me"))),
            "gopls"
        );
    }

    #[test]
    fn only_a_change_of_settings_is_applied_without_a_restart() {
        let a = server("x", "x", &[], &["x"], &[]);
        let b = ServerConfig {
            settings: Some(json!({ "x": { "a": 1 } })),
            ..a.clone()
        };
        assert!(differs_only_in_settings(&a, &b));
        assert!(!differs_only_in_settings(&a, &a));
        let c = ServerConfig {
            args: vec!["--other".into()],
            ..b.clone()
        };
        assert!(!differs_only_in_settings(&a, &c));
        let d = ServerConfig {
            env: BTreeMap::from([("A".to_string(), "1".to_string())]),
            ..b
        };
        assert!(!differs_only_in_settings(&a, &d));
    }

    #[test]
    fn deep_merge_goes_into_objects_and_replaces_the_rest() {
        let mut base = json!({ "a": { "b": 1, "c": [1] }, "d": "x" });
        deep_merge(
            &mut base,
            &json!({ "a": { "c": [2], "e": null }, "d": { "f": 1 } }),
        );
        assert_eq!(
            base,
            json!({ "a": { "b": 1, "c": [2], "e": null }, "d": { "f": 1 } })
        );
        let mut scalar = json!(1);
        deep_merge(&mut scalar, &json!({ "a": 1 }));
        assert_eq!(scalar, json!({ "a": 1 }));
    }

    #[test]
    fn the_users_python_interpreter_wins_over_the_projects() {
        let root = tempfile::tempdir().unwrap();
        let venv = root.path().join(".venv");
        fs::create_dir_all(venv.join("bin")).unwrap();
        fs::write(venv.join("bin/python"), "").unwrap();
        fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin").unwrap();
        let pyright = server("pyright", "pyright-langserver", &[], &["py"], &[]);
        let found = workspace_settings(&pyright, root.path());
        assert!(
            found["python"]["pythonPath"]
                .as_str()
                .unwrap()
                .ends_with(".venv/bin/python")
        );
        let own = with_user_settings(
            pyright.clone(),
            &json!({ "settings": { "python": { "pythonPath": "/usr/bin/python3" } } }),
        )
        .unwrap();
        assert_eq!(
            workspace_settings(&own, root.path())["python"]["pythonPath"],
            json!("/usr/bin/python3")
        );
        // A section that isn't an object is left alone (no panic).
        let odd = with_user_settings(pyright, &json!({ "settings": { "python": "x" } })).unwrap();
        assert_eq!(workspace_settings(&odd, root.path())["python"], json!("x"));
    }

    #[test]
    fn commands_resolve_by_path_and_by_name() {
        let config = |command: &str| ServerConfig {
            command: command.to_string(),
            ..server("x", "x", &[], &[], &[])
        };
        assert_eq!(
            config("/bin/sh").resolve_command(),
            Some(PathBuf::from("/bin/sh"))
        );
        assert!(config("sh").resolve_command().is_some());
        assert_eq!(config("/bin/no-such-server").resolve_command(), None);
        assert_eq!(config("no-such-server-flux").resolve_command(), None);
        // A directory is not an executable.
        assert_eq!(config("/bin").resolve_command(), None);
    }

    #[test]
    fn search_path_keeps_path_first_and_has_no_duplicates() {
        let dirs = search_path();
        if let Some(path) = env::var_os("PATH")
            && let Some(first) = env::split_paths(&path).find(|d| !d.as_os_str().is_empty())
        {
            assert_eq!(dirs[0], first);
        }
        for (i, dir) in dirs.iter().enumerate() {
            assert!(!dirs[..i].contains(dir), "{dir:?} twice");
        }
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
    }

    #[test]
    fn nvm_versions_newest_first() {
        let root = tempfile::tempdir().unwrap();
        for version in ["v18.19.0", "v20.11.1", "v9.0.0", "v20.2.0"] {
            fs::create_dir_all(root.path().join(version).join("bin")).unwrap();
        }
        let bins: Vec<String> = nvm_versions(root.path())
            .iter()
            .map(|bin| {
                let version = bin.parent().unwrap().file_name().unwrap();
                version.to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(bins, ["v20.11.1", "v20.2.0", "v18.19.0", "v9.0.0"]);
    }
}
