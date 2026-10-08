//! Which language server serves which files, and how to start it.

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

/// The built-in servers: rust-analyzer, gopls, pyright and ruff (Python: types, then formatting
/// and linting), typescript-language-server, taplo, bash-language-server, yaml-language-server,
/// vscode-json-language-server, marksman. A server that is not on the machine is installed by Flux
/// ([`crate::install`]).
pub fn default_servers() -> Vec<ServerConfig> {
    vec![
        server("rust-analyzer", "rust-analyzer", &[], &["rs"], &[]).installed(Install::Rustup {
            component: "rust-analyzer".into(),
            fallback: Box::new(github(
                "rust-lang/rust-analyzer",
                "rust-analyzer-{arch}-apple-darwin.gz",
                "rust-analyzer",
            )),
        }),
        server("gopls", "gopls", &[], &["go"], &[]).installed(Install::GoInstall {
            package: "golang.org/x/tools/gopls".into(),
            bin: "gopls".into(),
        }),
        server(
            "pyright",
            "pyright-langserver",
            &["--stdio"],
            &["py", "pyi", "pyw"],
            &[],
        )
        .installed(npm(&["pyright"], "pyright-langserver")),
        // After pyright: it has no formatter; ruff formats and lints.
        server("ruff", "ruff", &["server"], &["py", "pyi"], &[]).installed(github(
            "astral-sh/ruff",
            "ruff-{arch}-apple-darwin.tar.gz",
            "ruff",
        )),
        server(
            "typescript-language-server",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx"],
            &[],
        )
        // TypeScript 6: the last one with `tsserver`, which typescript-language-server drives;
        // TypeScript 7 is native and has an LSP server of its own.
        .installed(npm(
            &["typescript-language-server", "typescript@6"],
            "typescript-language-server",
        )),
        server(
            "taplo",
            "taplo",
            &["lsp", "stdio"],
            &["toml"],
            &["Cargo.lock", "Pipfile", "poetry.lock", "uv.lock"],
        )
        .installed(github("tamasfe/taplo", "taplo-darwin-{arch}.gz", "taplo")),
        server(
            "bash-language-server",
            "bash-language-server",
            &["start"],
            &["sh", "bash"],
            &[
                ".bashrc",
                ".bash_profile",
                ".bash_aliases",
                ".bash_logout",
                ".profile",
                "PKGBUILD",
            ],
        )
        .installed(npm(&["bash-language-server"], "bash-language-server")),
        server(
            "yaml-language-server",
            "yaml-language-server",
            &["--stdio"],
            &["yaml", "yml"],
            &[".clang-format", ".clang-tidy"],
        )
        .installed(npm(&["yaml-language-server"], "yaml-language-server")),
        ServerConfig {
            // Formatting is off unless asked for.
            initialization_options: Some(serde_json::json!({ "provideFormatter": true })),
            ..server(
                "vscode-json-language-server",
                "vscode-json-language-server",
                &["--stdio"],
                &["json", "jsonc"],
                &["flake.lock"],
            )
        }
        .installed(npm(
            &["vscode-langservers-extracted"],
            "vscode-json-language-server",
        )),
        server(
            "marksman",
            "marksman",
            &["server"],
            &["md", "markdown"],
            &[],
        )
        .installed(github(
            "artempyanykh/marksman",
            "marksman-macos",
            "marksman",
        )),
    ]
}

fn npm(packages: &[&str], bin: &str) -> Install {
    Install::Npm {
        packages: packages.iter().map(|p| p.to_string()).collect(),
        bin: bin.to_string(),
    }
}

fn github(repo: &str, asset: &str, bin: &str) -> Install {
    Install::GitHubRelease {
        repo: repo.to_string(),
        asset: asset.to_string(),
        bin: bin.to_string(),
    }
}

fn server(
    name: &str,
    command: &str,
    args: &[&str],
    extensions: &[&str],
    file_names: &[&str],
) -> ServerConfig {
    let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
    ServerConfig {
        name: name.to_string(),
        command: command.to_string(),
        args: strings(args),
        extensions: strings(extensions),
        file_names: strings(file_names),
        initialization_options: None,
        settings: None,
        install: None,
    }
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

/// `languageId` for `textDocument/didOpen`: "rust", "typescriptreact" for `.tsx`, …; "plaintext"
/// for anything unknown.
pub fn language_id(path: &Path) -> &'static str {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    match file_name {
        "Cargo.lock" | "Pipfile" | "poetry.lock" | "uv.lock" => return "toml",
        "flake.lock" => return "json",
        ".clang-format" | ".clang-tidy" => return "yaml",
        ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".profile" | ".zshrc"
        | ".zshenv" | ".zprofile" | ".zlogin" | ".zlogout" | "PKGBUILD" => return "shellscript",
        _ => {}
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "toml" => "toml",
        "sh" | "bash" | "zsh" => "shellscript",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "jsonc" => "jsonc",
        "md" | "markdown" => "markdown",
        _ => "plaintext",
    }
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

    fn installed(self, install: Install) -> Self {
        Self {
            install: Some(install),
            ..self
        }
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
    let mut settings = match &config.settings {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => json!({}),
    };
    let mut initialization_options = config.initialization_options.clone();
    if config.name == "pyright"
        && let Some(python) = python_interpreter(root, env::var_os("VIRTUAL_ENV").as_deref())
    {
        settings["python"]["pythonPath"] = json!(python);
    }
    if config.name == "typescript-language-server"
        && let Some(lib) = typescript_beside(program)
    {
        let options = initialization_options.get_or_insert_with(|| json!({}));
        if options.is_object() && options["tsserver"]["fallbackPath"].is_null() {
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
    let home = env::var_os("HOME").map(PathBuf::from);
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
    fn every_config_has_a_language_id_for_its_files() {
        for config in default_servers() {
            for extension in &config.extensions {
                let path = PathBuf::from(format!("file.{extension}"));
                assert_ne!(language_id(&path), "plaintext", "{extension}");
            }
            for file_name in &config.file_names {
                assert_ne!(
                    language_id(Path::new(file_name)),
                    "plaintext",
                    "{file_name}"
                );
            }
        }
        assert_eq!(language_id(Path::new("a.tsx")), "typescriptreact");
        assert_eq!(language_id(Path::new("a.jsx")), "javascriptreact");
        assert_eq!(language_id(Path::new("a.mts")), "typescript");
        assert_eq!(language_id(Path::new("x.sh")), "shellscript");
        assert_eq!(language_id(Path::new("README")), "plaintext");
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
