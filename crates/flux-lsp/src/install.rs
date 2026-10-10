//! Installing language servers that are not on the machine, into Flux's own directory: npm
//! packages with a Node.js downloaded for them, GitHub release binaries, `go install`, rustup
//! components. Blocking — run off the UI thread.
//!
//! The directory ([`set_dir`]; the app uses `~/Library/Application Support/flux/servers`):
//!
//! ```text
//! node/                                  Node.js for the npm servers (official build, SHA-256 checked)
//! pyright/node_modules/.bin/…            an npm server: its own prefix
//! ruff/bin/ruff                          a binary server (GitHub release, go install)
//! shellcheck/bin/shellcheck              a tool a server runs (bash-language-server)
//! <name>/install.json                    what is installed there: version, source
//! .locks/<name>                          an install in progress (its pid): others wait for it
//! .tmp/                                  installs in progress, moved into place with one rename
//! .npm-cache/                            npm's download cache
//! ```
//!
//! Downloads go through the system `curl`, archives through `tar`, `gunzip` and `unzip`, checksums
//! through `shasum` — all part of macOS — so the crate needs no HTTP or TLS stack of its own.
//! Node.js is Flux's own even when the user has one: a version manager may switch the user's
//! Node.js per project, and servers installed by one npm may not run on another Node.js.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::config::{Install, ServerConfig, search_path, search_path_env};

/// `check` messages, short enough for the status bar; the app may translate them.
pub const NEEDS_GO: &str = "Needs Go";
pub const UNSUPPORTED: &str = "Can't be installed on this system";
pub const NO_INSTALLER: &str = "Flux can't install it";
/// `install` was canceled.
pub const CANCELED: &str = "Installation canceled";

/// An install lock older than this is taken over even if its process looks alive (a pid reused by
/// another process).
const LOCK_MAX_AGE: Duration = Duration::from_secs(30 * 60);
/// Leftovers of interrupted installs in `.tmp` older than this are removed.
const TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const POLL: Duration = Duration::from_millis(150);
const USER_AGENT: &str = concat!("Flux/", env!("CARGO_PKG_VERSION"));
/// Tools that servers run, installed by Flux next to them; added to every server's `PATH`.
const TOOLS: &[&str] = &["shellcheck"];

static DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Where installed servers live (`~/Library/Application Support/flux/servers`); set by the app at
/// start. Without it nothing is installed or found there.
pub fn set_dir(dir: PathBuf) {
    if let Ok(mut current) = DIR.write() {
        *current = Some(dir);
    }
}

fn dir() -> Option<PathBuf> {
    DIR.read().ok()?.clone()
}

/// Install progress for the status bar: "Downloading Node.js", 0.4.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub text: String,
    pub fraction: Option<f32>,
}

/// Whether Flux can install this server here: `Err` with a reason for the user ([`NEEDS_GO`],
/// [`UNSUPPORTED`], [`NO_INSTALLER`]) when it can't. Needs no network: whether the download works
/// shows only when installing.
pub fn check(config: &ServerConfig) -> Result<(), String> {
    match &config.install {
        Some(recipe) => check_recipe(recipe),
        None => Err(NO_INSTALLER.into()),
    }
}

fn check_recipe(recipe: &Install) -> Result<(), String> {
    match recipe {
        Install::Npm { .. } => node_platform().map(|_| ()).ok_or(UNSUPPORTED.into()),
        Install::GitHubRelease { .. } => {
            let supported = cfg!(target_os = "macos") && github_arch().is_some();
            supported.then_some(()).ok_or(UNSUPPORTED.into())
        }
        Install::GoInstall { .. } => find_tool("go").map(|_| ()).ok_or(NEEDS_GO.into()),
        Install::Rustup { fallback, .. } => match find_tool("rustup") {
            Some(_) => Ok(()),
            None => check_recipe(fallback),
        },
    }
}

/// Installs the server; afterwards [`ServerConfig::resolve_command`] finds it. Installs of the same
/// server from other windows or Flux processes wait for each other instead of racing. `cancel`
/// stops it between steps. `Err` — a message for the status bar.
pub fn install(
    config: &ServerConfig,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<(), String> {
    check(config)?;
    if config.resolve_command().is_some() {
        return Ok(());
    }
    let dir = dir().ok_or("No directory for language servers")?;
    Installer {
        dir,
        sources: Sources::default(),
        progress,
        cancel,
    }
    .install_with(config, false)
}

/// Where a server's command comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Installed on the machine (PATH, Homebrew, rustup…): Flux uses it and leaves it alone.
    System(PathBuf),
    /// Installed by Flux; the version from its `install.json`, if known.
    Flux {
        version: Option<String>,
    },
    Missing,
}

/// Where the server comes from now: the machine first, as [`ServerConfig::resolve_command`] does.
pub fn source(config: &ServerConfig) -> Source {
    if let Some(path) = config.resolve_system_command() {
        return Source::System(path);
    }
    match dir().and_then(|dir| installed_home(&dir, config)) {
        Some((home, _)) => Source::Flux {
            version: installed_version(&home, config),
        },
        None => Source::Missing,
    }
}

/// From the `install.json` of the server's folder: `version`, or for an npm server — the version of
/// its package.
fn installed_version(home: &Path, config: &ServerConfig) -> Option<String> {
    let manifest = fs::read_to_string(home.join("install.json")).ok()?;
    let manifest: Value = serde_json::from_str(&manifest).ok()?;
    let version = match (&manifest["version"], &config.install) {
        (Value::String(version), _) => version.as_str(),
        (_, Some(Install::Npm { packages, .. })) => {
            manifest["packages"][package_name(packages.first()?)].as_str()?
        }
        _ => return None,
    };
    Some(version.trim_start_matches('v').to_string())
}

/// Installs the latest version again, even if Flux has one: the old install keeps working until
/// the new one replaces it. Blocking, like [`install`].
pub fn update(
    config: &ServerConfig,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<(), String> {
    check(config)?;
    let dir = dir().ok_or("No directory for language servers")?;
    Installer {
        dir,
        sources: Sources::default(),
        progress,
        cancel,
    }
    .install_with(config, true)
}

/// Removes what Flux installed for the server (its own Node.js and tools stay: other servers use
/// them). Waits for an install of the same server in progress.
pub fn uninstall(config: &ServerConfig) -> Result<(), String> {
    let dir = dir().ok_or("No directory for language servers")?;
    let cancel = AtomicBool::new(false);
    let installer = Installer {
        dir: dir.clone(),
        sources: Sources::default(),
        progress: &|_| {},
        cancel: &cancel,
    };
    let _lock = installer.lock(&config.name)?;
    match fs::remove_dir_all(dir.join(&config.name)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

/// The command of a server Flux installed, if it is there (an npm server also needs Flux's
/// Node.js).
pub(crate) fn installed_command(config: &ServerConfig) -> Option<PathBuf> {
    installed_in(&dir()?, config)
}

/// The command, if `<dir>/<name>` has the server installed by the same recipe: when a new Flux
/// changes the recipe (another package version, another source), the old install is replaced.
fn installed_in(dir: &Path, config: &ServerConfig) -> Option<PathBuf> {
    installed_home(dir, config).map(|(_, command)| command)
}

/// Where the server is installed (its folder) and its command. An npm server may live in the folder
/// of another server installed from the same packages: the JSON, HTML and CSS servers all come from
/// `vscode-langservers-extracted`, and one copy (~90 MB) serves the three.
fn installed_home(dir: &Path, config: &ServerConfig) -> Option<(PathBuf, PathBuf)> {
    let recipe = config.install.as_ref()?;
    let node_ready = !matches!(recipe, Install::Npm { .. })
        || is_executable(&dir.join("node").join("bin").join("node"));
    if !node_ready {
        return None;
    }
    let recipe_of = |home: &Path| -> Option<String> {
        let manifest = fs::read_to_string(home.join("install.json")).ok()?;
        let manifest: Value = serde_json::from_str(&manifest).ok()?;
        manifest["recipe"].as_str().map(str::to_string)
    };
    let home = dir.join(&config.name);
    let path = command_path(dir, &config.name, recipe);
    if is_executable(&path) && recipe_of(&home).as_deref() == Some(recipe_key(recipe).as_str()) {
        return Some((home, path));
    }
    let Install::Npm { packages, bin } = recipe else {
        return None;
    };
    let same_packages = format!("npm:{}:", packages.join(","));
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|other| other.is_dir() && *other != home)
        .find_map(|other| {
            let command = other.join("node_modules").join(".bin").join(bin);
            let shared = recipe_of(&other).is_some_and(|key| key.starts_with(&same_packages));
            (shared && is_executable(&command)).then_some((other, command))
        })
}

/// What an install was made from, kept in its `install.json`.
fn recipe_key(recipe: &Install) -> String {
    match recipe {
        Install::Npm { packages, bin } => format!("npm:{}:{bin}", packages.join(",")),
        Install::GitHubRelease { repo, asset, bin } => format!("github:{repo}:{asset}:{bin}"),
        Install::GoInstall { package, bin } => format!("go:{package}:{bin}"),
        Install::Rustup { fallback, .. } => recipe_key(fallback),
    }
}

/// The package name of an npm spec: `typescript@6` → `typescript`, `@scope/x@1` → `@scope/x`.
fn package_name(spec: &str) -> &str {
    match spec.rfind('@') {
        Some(at) if at > 0 => &spec[..at],
        _ => spec,
    }
}

/// Where `recipe` puts the server's executable under `dir`.
fn command_path(dir: &Path, name: &str, recipe: &Install) -> PathBuf {
    let home = dir.join(name);
    match recipe {
        Install::Npm { bin, .. } => home.join("node_modules").join(".bin").join(bin),
        Install::GitHubRelease { bin, .. } | Install::GoInstall { bin, .. } => {
            home.join("bin").join(bin)
        }
        Install::Rustup { fallback, .. } => command_path(dir, name, fallback),
    }
}

/// Flux's Node.js `bin`, if `program` is an npm server Flux installed: it must run first in the
/// server's `PATH` (the scripts start with `#!/usr/bin/env node`).
pub(crate) fn node_bin_for(config: &ServerConfig, program: &Path) -> Option<PathBuf> {
    let dir = dir()?;
    if !matches!(config.install, Some(Install::Npm { .. })) || !program.starts_with(&dir) {
        return None;
    }
    Some(dir.join("node").join("bin"))
}

/// `bin` directories of the tools Flux installed for servers.
pub(crate) fn tool_dirs() -> Vec<PathBuf> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    TOOLS
        .iter()
        .map(|tool| dir.join(tool).join("bin"))
        .filter(|bin| bin.is_dir())
        .collect()
}

/// Where downloads come from; tests point them at local files.
struct Sources {
    github_api: String,
    github: String,
    node_dist: String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            github_api: "https://api.github.com".into(),
            github: "https://github.com".into(),
            node_dist: "https://nodejs.org/dist".into(),
        }
    }
}

struct Installer<'a> {
    dir: PathBuf,
    sources: Sources,
    progress: &'a (dyn Fn(Progress) + Sync),
    cancel: &'a AtomicBool,
}

impl Installer<'_> {
    /// `force` — install even if this recipe is already installed (update).
    fn install_with(&self, config: &ServerConfig, force: bool) -> Result<(), String> {
        let recipe = config.install.as_ref().ok_or(NO_INSTALLER)?;
        fs::create_dir_all(&self.dir)
            .map_err(|err| format!("Can't create {}: {err}", self.dir.display()))?;
        self.clean_temp();
        let _lock = self.lock(&config.name)?;
        // Another window or Flux process may have installed it while we waited.
        if force || installed_in(&self.dir, config).is_none() {
            self.run_recipe(&config.name, recipe)?;
        }
        if config.name == "bash-language-server" {
            // Without shellcheck the server still works, only without diagnostics.
            self.install_shellcheck().ok();
        }
        Ok(())
    }

    fn run_recipe(&self, name: &str, recipe: &Install) -> Result<(), String> {
        let key = recipe_key(recipe);
        match recipe {
            Install::Npm { packages, bin } => self.npm(name, packages, bin, &key),
            Install::GitHubRelease { repo, asset, bin } => {
                self.github(name, repo, asset, bin, &key)
            }
            Install::GoInstall { package, bin } => self.go_install(name, package, bin, &key),
            Install::Rustup {
                component,
                fallback,
            } => {
                if self.rustup(component)? {
                    return Ok(());
                }
                self.run_recipe(name, fallback)
            }
        }
    }

    fn report(&self, text: impl Into<String>, fraction: Option<f32>) {
        (self.progress)(Progress {
            text: text.into(),
            fraction,
        });
    }

    fn check_cancel(&self) -> Result<(), String> {
        match self.cancel.load(Ordering::Relaxed) {
            true => Err(CANCELED.into()),
            false => Ok(()),
        }
    }

    // --- Recipes ---

    /// npm packages into `<dir>/<name>` with Flux's Node.js.
    fn npm(&self, name: &str, packages: &[String], bin: &str, key: &str) -> Result<(), String> {
        let node = self.ensure_node()?;
        let staging = self.staging(name)?;
        self.report(format!("Installing {name}"), None);
        let npm = node
            .join("lib")
            .join("node_modules")
            .join("npm")
            .join("bin");
        let mut command = Command::new(node.join("bin").join("node"));
        command
            .arg(npm.join("npm-cli.js"))
            .args(["install", "--no-audit", "--no-fund", "--no-update-notifier"])
            .args(["--loglevel=error", "--prefix"])
            .arg(staging.path())
            .arg("--cache")
            .arg(self.dir.join(".npm-cache"))
            .args(packages)
            .current_dir(staging.path())
            .env("PATH", prepend_path(&node.join("bin")));
        self.run(&mut command, "npm install")?;
        let command = staging.path().join("node_modules").join(".bin").join(bin);
        if !is_executable(&command) {
            return Err(format!("npm installed no {bin}"));
        }
        let versions: serde_json::Map<String, Value> = packages
            .iter()
            .map(|spec| {
                let package = package_name(spec);
                let manifest = staging
                    .path()
                    .join("node_modules")
                    .join(package)
                    .join("package.json");
                let version = fs::read_to_string(manifest)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .and_then(|json| json["version"].as_str().map(str::to_string));
                (package.to_string(), json!(version))
            })
            .collect();
        write_manifest(
            staging.path(),
            json!({ "recipe": key, "packages": versions }),
        )?;
        self.commit(staging, name)
    }

    /// A binary from the latest GitHub release of `repo` into `<dir>/<name>/bin`.
    fn github(
        &self,
        name: &str,
        repo: &str,
        asset: &str,
        bin: &str,
        key: &str,
    ) -> Result<(), String> {
        let arch = github_arch().ok_or(UNSUPPORTED)?;
        self.report(format!("Looking up the latest {name}"), None);
        let release = self.fetch(&format!(
            "{}/repos/{repo}/releases/latest",
            self.sources.github_api
        ));
        let (file, url, size, checksum, version) = match release {
            Ok(text) => {
                let release: Value = serde_json::from_str(&text)
                    .map_err(|err| format!("Unexpected answer from GitHub: {err}"))?;
                let tag = release["tag_name"].as_str().unwrap_or_default().to_string();
                let file = expand_asset(asset, arch, &tag);
                let found = find_asset(&release, &file)
                    .ok_or_else(|| format!("No {file} in the latest {name} release"))?;
                let checksum = find_asset(&release, &format!("{file}.sha256"));
                (file, found.0, found.1, checksum.map(|c| c.0), tag)
            }
            // The API allows 60 requests an hour without a token; the "latest" link has no limit,
            // but works only for file names without the version in them.
            Err(_) if !asset.contains("{tag}") => {
                let file = expand_asset(asset, arch, "");
                let url = format!(
                    "{}/{repo}/releases/latest/download/{file}",
                    self.sources.github
                );
                (file, url, None, None, "latest".to_string())
            }
            Err(err) => return Err(err),
        };
        let staging = self.staging(name)?;
        let archive = staging.path().join(&file);
        self.download(&url, &archive, &format!("Downloading {name}"), size)?;
        if let Some(checksum_url) = checksum {
            let text = self.fetch(&checksum_url)?;
            let expected = text.split_whitespace().next().unwrap_or_default();
            verify_sha256(&archive, expected)?;
        }
        self.report(format!("Unpacking {name}"), None);
        let unpacked = staging.path().join("unpacked");
        unpack(&archive, &unpacked)?;
        let binary = find_binary(&unpacked, bin).ok_or_else(|| format!("No {bin} in {file}"))?;
        let bin_dir = staging.path().join("bin");
        fs::create_dir_all(&bin_dir).map_err(|err| err.to_string())?;
        let target = bin_dir.join(bin);
        fs::rename(&binary, &target).map_err(|err| err.to_string())?;
        make_executable(&target)?;
        fs::remove_dir_all(&unpacked).ok();
        fs::remove_file(&archive).ok();
        write_manifest(
            staging.path(),
            json!({ "recipe": key, "version": version, "asset": file }),
        )?;
        self.commit(staging, name)
    }

    /// `go install package@latest` into `<dir>/<name>/bin`.
    fn go_install(&self, name: &str, package: &str, bin: &str, key: &str) -> Result<(), String> {
        let go = find_tool("go").ok_or(NEEDS_GO)?;
        let staging = self.staging(name)?;
        let bin_dir = staging.path().join("bin");
        self.report(format!("Building {name} with Go"), None);
        let mut command = Command::new(&go);
        command
            .args(["install", &format!("{package}@latest")])
            .current_dir(staging.path())
            .env("GOBIN", &bin_dir)
            .env("PATH", search_path_env());
        self.run(&mut command, "go install")?;
        let binary = bin_dir.join(bin);
        if !is_executable(&binary) {
            return Err(format!("go install built no {bin}"));
        }
        let version = Command::new(&go)
            .args(["version", "-m"])
            .arg(&binary)
            .env("PATH", search_path_env())
            .output()
            .ok()
            .and_then(|output| module_version(&String::from_utf8_lossy(&output.stdout)));
        write_manifest(staging.path(), json!({ "recipe": key, "version": version }))?;
        self.commit(staging, name)
    }

    /// `rustup component add`; `false` — there is no rustup, or it couldn't add the component.
    fn rustup(&self, component: &str) -> Result<bool, String> {
        let Some(rustup) = find_tool("rustup") else {
            return Ok(false);
        };
        self.report(format!("Installing {component} with rustup"), None);
        let mut add = Command::new(&rustup);
        add.args(["component", "add", component])
            .env("PATH", search_path_env());
        if self.run(&mut add, "rustup").is_err() {
            return Ok(false);
        }
        let found = Command::new(&rustup)
            .args(["which", component])
            .env("PATH", search_path_env())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        Ok(found)
    }

    /// shellcheck for bash-language-server, unless the machine has one.
    fn install_shellcheck(&self) -> Result<(), String> {
        let name = "shellcheck";
        if find_tool(name).is_some() || is_executable(&self.dir.join(name).join("bin").join(name)) {
            return Ok(());
        }
        let _lock = self.lock(name)?;
        let recipe = Install::GitHubRelease {
            repo: "koalaman/shellcheck".into(),
            asset: "shellcheck-{tag}.darwin.{arch}.tar.xz".into(),
            bin: name.into(),
        };
        self.run_recipe(name, &recipe)
    }

    /// Flux's Node.js (`<dir>/node`): the latest LTS, downloaded and checked against the release's
    /// `SHASUMS256.txt` the first time an npm server is installed.
    fn ensure_node(&self) -> Result<PathBuf, String> {
        let home = self.dir.join("node");
        if is_executable(&home.join("bin").join("node")) {
            return Ok(home);
        }
        let _lock = self.lock("node")?;
        if is_executable(&home.join("bin").join("node")) {
            return Ok(home);
        }
        let (os, arch) = node_platform().ok_or(UNSUPPORTED)?;
        self.report("Looking up Node.js", None);
        let index = self.fetch(&format!("{}/index.json", self.sources.node_dist))?;
        let version = latest_lts(&index, os, arch).ok_or("No Node.js LTS for this system")?;
        let file = format!("node-{version}-{os}-{arch}.tar.xz");
        let shasums = self.fetch(&format!(
            "{}/{version}/SHASUMS256.txt",
            self.sources.node_dist
        ))?;
        let expected = shasum_for(&shasums, &file)
            .ok_or_else(|| format!("No checksum for {file}"))?
            .to_string();
        let staging = self.staging("node")?;
        let archive = staging.path().join(&file);
        let url = format!("{}/{version}/{file}", self.sources.node_dist);
        self.download(
            &url,
            &archive,
            &format!("Downloading Node.js {version}"),
            None,
        )?;
        verify_sha256(&archive, &expected)?;
        self.report("Unpacking Node.js", None);
        let unpacked = staging.path().join("unpacked");
        unpack(&archive, &unpacked)?;
        let root = unpacked.join(format!("node-{version}-{os}-{arch}"));
        if !is_executable(&root.join("bin").join("node")) {
            return Err(format!("No node in {file}"));
        }
        write_manifest(&root, json!({ "source": "nodejs.org", "version": version }))?;
        self.commit_dir(&root, "node")?;
        Ok(home)
    }

    // --- Steps ---

    /// Downloads `url` to `to`, reporting the share done (by the file's size so far) and stopping
    /// on cancel.
    fn download(&self, url: &str, to: &Path, label: &str, size: Option<u64>) -> Result<(), String> {
        self.check_cancel()?;
        self.report(label, Some(0.));
        let size = size.or_else(|| self.content_length(url));
        let mut command = Command::new("curl");
        command
            .args([
                "-fL",
                "-sS",
                "--retry",
                "2",
                "--connect-timeout",
                "20",
                "-A",
                USER_AGENT,
            ])
            .arg("-o")
            .arg(to)
            .arg(url);
        let report = || {
            let done = fs::metadata(to).map_or(0, |m| m.len());
            if let Some(size) = size.filter(|&size| size > 0) {
                self.report(label, Some((done as f32 / size as f32).min(1.)));
            }
        };
        self.run_with(&mut command, "Download", report)
    }

    /// The size of what `url` serves (after redirects), if the server says.
    fn content_length(&self, url: &str) -> Option<u64> {
        let output = Command::new("curl")
            .args(["-sIL", "--connect-timeout", "20", "-A", USER_AGENT, url])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        // The last one: the headers of every redirect come first.
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .rev()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
    }

    /// A small text resource (release info, checksums, Node.js index).
    fn fetch(&self, url: &str) -> Result<String, String> {
        self.check_cancel()?;
        let output = Command::new("curl")
            .args(["-fL", "-sS", "--retry", "2", "--connect-timeout", "20"])
            .args(["--max-time", "60", "-A", USER_AGENT])
            .args(["-H", "Accept: application/vnd.github+json", url])
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("Can't run curl: {err}"))?;
        if !output.status.success() {
            let message = last_line(&String::from_utf8_lossy(&output.stderr));
            return Err(format!("Download failed: {message}"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run(&self, command: &mut Command, what: &str) -> Result<(), String> {
        self.run_with(command, what, || {})
    }

    /// Runs `command`, calling `tick` while it works, killing it on cancel. Its output is collected
    /// on threads (a chatty npm would otherwise fill the pipe and stall); the last line goes into
    /// the error.
    fn run_with(&self, command: &mut Command, what: &str, tick: impl Fn()) -> Result<(), String> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| format!("Can't run {what}: {err}"))?;
        let output = Arc::new(Mutex::new(String::new()));
        let readers: Vec<_> = [
            child
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|err| Box::new(err) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        .map(|stream| {
            let output = output.clone();
            thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if let Ok(mut output) = output.lock()
                        && !line.trim().is_empty()
                    {
                        output.push_str(&line);
                        output.push('\n');
                    }
                }
            })
        })
        .collect();
        let status = loop {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CANCELED.into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    tick();
                    thread::sleep(POLL);
                }
                Err(err) => return Err(format!("{what}: {err}")),
            }
        };
        for reader in readers {
            let _ = reader.join();
        }
        if status.success() {
            return Ok(());
        }
        let output = output.lock().map(|o| o.clone()).unwrap_or_default();
        let message = last_line(&output);
        Err(match message.is_empty() {
            true => format!("{what} failed ({status})"),
            false => format!("{what} failed: {message}"),
        })
    }

    /// Waits for other installs of `name` (this or another Flux process), then holds the lock until
    /// dropped. A lock whose process is gone, or that is very old, is taken over.
    fn lock(&self, name: &str) -> Result<Lock, String> {
        let locks = self.dir.join(".locks");
        fs::create_dir_all(&locks).map_err(|err| err.to_string())?;
        let path = locks.join(name);
        let mut waiting = false;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(Lock { path });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        fs::remove_file(&path).ok();
                        continue;
                    }
                    if !waiting {
                        self.report(format!("Waiting for another install of {name}"), None);
                        waiting = true;
                    }
                    self.check_cancel()?;
                    thread::sleep(POLL * 2);
                }
                Err(err) => return Err(format!("Can't lock {name}: {err}")),
            }
        }
    }

    /// A fresh directory in `.tmp` for an install of `name`; removed unless committed.
    fn staging(&self, name: &str) -> Result<Staging, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = self
            .dir
            .join(".tmp")
            .join(format!("{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)
            .map_err(|err| format!("Can't create {}: {err}", path.display()))?;
        Ok(Staging { path })
    }

    fn commit(&self, staging: Staging, name: &str) -> Result<(), String> {
        let result = self.commit_dir(staging.path(), name);
        drop(staging);
        result
    }

    /// Puts `from` in place of `<dir>/<name>` with renames: a reader sees the old install or the
    /// new one, never half of either.
    fn commit_dir(&self, from: &Path, name: &str) -> Result<(), String> {
        self.check_cancel()?;
        let target = self.dir.join(name);
        let old = self.dir.join(".tmp").join(format!(
            "{name}-old-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let replaced = target.exists() && fs::rename(&target, &old).is_ok();
        if let Err(err) = fs::rename(from, &target) {
            if replaced {
                fs::rename(&old, &target).ok();
            }
            return Err(format!("Can't install {name}: {err}"));
        }
        if replaced {
            fs::remove_dir_all(&old).ok();
        }
        Ok(())
    }

    /// Removes what interrupted installs left in `.tmp` long ago.
    fn clean_temp(&self) {
        let Ok(entries) = fs::read_dir(self.dir.join(".tmp")) else {
            return;
        };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > TEMP_MAX_AGE);
            if old {
                fs::remove_dir_all(entry.path()).ok();
            }
        }
    }
}

struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        fs::remove_file(&self.path).ok();
    }
}

/// A directory of an install in progress; removed when dropped (after a commit it is gone already).
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

fn lock_is_stale(path: &Path) -> bool {
    let age = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .unwrap_or_default();
    if age > LOCK_MAX_AGE {
        return true;
    }
    match fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    {
        Some(pid) => !process_alive(pid),
        // Being written right now, unless it has been empty for a while (a crash between create
        // and write).
        None => age > Duration::from_secs(5),
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

// --- Pure helpers ---

/// `{arch}` in GitHub asset names: `aarch64`, `x86_64`.
fn github_arch() -> Option<&'static str> {
    match env::consts::ARCH {
        arch @ ("aarch64" | "x86_64") => Some(arch),
        _ => None,
    }
}

/// Node.js's names for this system: (`darwin`, `arm64`).
fn node_platform() -> Option<(&'static str, &'static str)> {
    let os = match env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    Some((os, arch))
}

fn expand_asset(pattern: &str, arch: &str, tag: &str) -> String {
    pattern.replace("{arch}", arch).replace("{tag}", tag)
}

/// The download URL and size of the asset named `name` in a GitHub release.
fn find_asset(release: &Value, name: &str) -> Option<(String, Option<u64>)> {
    release["assets"].as_array()?.iter().find_map(|asset| {
        (asset["name"].as_str()? == name).then(|| {
            let url = asset["browser_download_url"].as_str()?.to_string();
            Some((url, asset["size"].as_u64()))
        })?
    })
}

/// The newest LTS in Node.js's `index.json` with a tarball for `os`-`arch` (`darwin`, `arm64`).
fn latest_lts(index: &str, os: &str, arch: &str) -> Option<String> {
    let releases: Vec<Value> = serde_json::from_str(index).ok()?;
    // index.json names macOS tarballs "osx-arm64-tar", Linux ones "linux-arm64".
    let file = match os {
        "darwin" => format!("osx-{arch}-tar"),
        os => format!("{os}-{arch}"),
    };
    releases.iter().find_map(|release| {
        let lts = !matches!(release["lts"], Value::Bool(false) | Value::Null);
        let has_file = release["files"]
            .as_array()?
            .iter()
            .any(|f| f.as_str() == Some(file.as_str()));
        (lts && has_file).then(|| release["version"].as_str().map(str::to_string))?
    })
}

/// The checksum of `file` in a `SHASUMS256.txt` (`<hex>  <file>` lines).
fn shasum_for<'a>(shasums: &'a str, file: &str) -> Option<&'a str> {
    shasums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == file).then_some(hash)
    })
}

/// The module version in `go version -m` output (`\tmod\tgolang.org/x/tools/gopls\tv0.20.0\t…`).
fn module_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next()? == "mod").then(|| parts.nth(1).map(str::to_string))?
    })
}

fn verify_sha256(file: &Path, expected: &str) -> Result<(), String> {
    let actual = sha256(file)?;
    match actual.eq_ignore_ascii_case(expected.trim()) {
        true => Ok(()),
        false => Err(format!(
            "Checksum mismatch for {}",
            file.file_name().unwrap_or_default().to_string_lossy()
        )),
    }
}

/// SHA-256 with `shasum` (macOS) or `sha256sum` (Linux).
fn sha256(file: &Path) -> Result<String, String> {
    let attempts: [(&str, &[&str]); 2] = [("shasum", &["-a", "256"]), ("sha256sum", &[])];
    for (tool, args) in attempts {
        if let Ok(output) = Command::new(tool).args(args).arg(file).output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(hash) = text.split_whitespace().next() {
                return Ok(hash.to_string());
            }
        }
    }
    Err("Can't compute a checksum".into())
}

/// Unpacks an archive into `into` by its name: tarballs, zip, a single gzipped file, or a plain file
/// (copied as is).
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    fs::create_dir_all(into).map_err(|err| err.to_string())?;
    let name = archive
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let tool_failed = |tool: &str, output: std::process::Output| {
        let message = last_line(&String::from_utf8_lossy(&output.stderr));
        Err(format!("{tool} failed: {message}"))
    };
    let tarball = [".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".tar"]
        .iter()
        .any(|ext| name.ends_with(ext));
    if tarball {
        let output = Command::new("tar")
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(into)
            .output()
            .map_err(|err| err.to_string())?;
        return match output.status.success() {
            true => Ok(()),
            false => tool_failed("tar", output),
        };
    }
    if name.ends_with(".zip") {
        let output = Command::new("unzip")
            .arg("-q")
            .arg(archive)
            .arg("-d")
            .arg(into)
            .output()
            .map_err(|err| err.to_string())?;
        return match output.status.success() {
            true => Ok(()),
            false => tool_failed("unzip", output),
        };
    }
    if let Some(stem) = name.strip_suffix(".gz") {
        let target = fs::File::create(into.join(stem)).map_err(|err| err.to_string())?;
        let output = Command::new("gunzip")
            .arg("-c")
            .arg(archive)
            .stdout(target)
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| err.to_string())?;
        return match output.status.success() {
            true => Ok(()),
            false => tool_failed("gunzip", output),
        };
    }
    fs::copy(archive, into.join(&name))
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// The file named `bin` anywhere under `dir`; otherwise the only file there (a single binary
/// downloaded as `taplo-darwin-aarch64.gz` or `marksman-macos`).
fn find_binary(dir: &Path, bin: &str) -> Option<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).ok()?.flatten() {
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                if path.file_name().is_some_and(|name| name == bin) {
                    return Some(path);
                }
                files.push(path);
            }
        }
    }
    (files.len() == 1).then(|| files.remove(0))
}

fn write_manifest(dir: &Path, manifest: Value) -> Result<(), String> {
    let mut manifest = manifest;
    let installed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    manifest["installed_at"] = json!(installed);
    let text = serde_json::to_string_pretty(&manifest).map_err(|err| err.to_string())?;
    fs::write(dir.join("install.json"), text).map_err(|err| err.to_string())
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|err| err.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// A tool (`go`, `rustup`, `shellcheck`) in the servers' search path.
fn find_tool(name: &str) -> Option<PathBuf> {
    search_path()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path))
}

/// `dir` and then the servers' search path, as a `PATH` value.
fn prepend_path(dir: &Path) -> OsString {
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(env::split_paths(&search_path_env()));
    env::join_paths(dirs).unwrap_or_default()
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

fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> impl Fn(Progress) + Sync {
        |_| {}
    }

    #[test]
    fn asset_names_and_release_lookup() {
        assert_eq!(
            expand_asset("ruff-{arch}-apple-darwin.tar.gz", "aarch64", "0.16.10"),
            "ruff-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            expand_asset("shellcheck-{tag}.darwin.{arch}.tar.xz", "x86_64", "v0.11.0"),
            "shellcheck-v0.11.0.darwin.x86_64.tar.xz"
        );
        let release = json!({
            "tag_name": "0.10.0",
            "assets": [
                { "name": "taplo-darwin-aarch64.gz", "size": 10,
                  "browser_download_url": "https://example.com/a" },
                { "name": "taplo-darwin-x86_64.gz", "size": 20,
                  "browser_download_url": "https://example.com/b" },
            ],
        });
        assert_eq!(
            find_asset(&release, "taplo-darwin-x86_64.gz"),
            Some(("https://example.com/b".to_string(), Some(20)))
        );
        assert_eq!(find_asset(&release, "taplo-linux.gz"), None);
    }

    #[test]
    fn node_lts_and_checksums() {
        let index = json!([
            { "version": "v26.1.0", "lts": false, "files": ["osx-arm64-tar", "linux-x64"] },
            { "version": "v24.21.0", "lts": "Krypton", "files": ["osx-x64-tar", "linux-x64"] },
            { "version": "v24.20.0", "lts": "Krypton", "files": ["osx-arm64-tar"] },
        ])
        .to_string();
        // The newest LTS that has a build for the system.
        assert_eq!(
            latest_lts(&index, "darwin", "x64").as_deref(),
            Some("v24.21.0")
        );
        assert_eq!(
            latest_lts(&index, "darwin", "arm64").as_deref(),
            Some("v24.20.0")
        );
        assert_eq!(
            latest_lts(&index, "linux", "x64").as_deref(),
            Some("v24.21.0")
        );
        assert_eq!(latest_lts(&index, "linux", "arm64"), None);
        assert_eq!(latest_lts("not json", "darwin", "arm64"), None);

        let shasums =
            "aaa  node-v24.21.0-darwin-arm64.tar.gz\nbbb  node-v24.21.0-darwin-arm64.tar.xz\n";
        assert_eq!(
            shasum_for(shasums, "node-v24.21.0-darwin-arm64.tar.xz"),
            Some("bbb")
        );
        assert_eq!(shasum_for(shasums, "node-v24.21.0-darwin-x64.tar.xz"), None);
    }

    #[test]
    fn go_module_version() {
        let output = "/x/gopls: go1.27.1\n\tpath\tgolang.org/x/tools/gopls\n\tmod\tgolang.org/x/tools/gopls\tv0.20.0\th1:abc=\n\tdep\tgithub.com/x\tv1.0.0\n";
        assert_eq!(module_version(output).as_deref(), Some("v0.20.0"));
        assert_eq!(module_version("nothing"), None);
    }

    #[test]
    fn binaries_are_found_by_name_or_as_the_only_file() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("ruff-aarch64-apple-darwin");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("ruff"), "x").unwrap();
        fs::write(nested.join("README"), "x").unwrap();
        assert_eq!(find_binary(root.path(), "ruff"), Some(nested.join("ruff")));

        let single = tempfile::tempdir().unwrap();
        fs::write(single.path().join("marksman-macos"), "x").unwrap();
        assert_eq!(
            find_binary(single.path(), "marksman"),
            Some(single.path().join("marksman-macos"))
        );
        fs::write(single.path().join("other"), "x").unwrap();
        assert_eq!(find_binary(single.path(), "marksman"), None);
    }

    #[test]
    fn locks_of_dead_processes_are_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let progress = quiet();
        let installer = Installer {
            dir: dir.path().to_path_buf(),
            sources: Sources::default(),
            progress: &progress,
            cancel: &cancel,
        };
        let lock = installer.lock("x").unwrap();
        assert!(dir.path().join(".locks/x").is_file());
        drop(lock);
        assert!(!dir.path().join(".locks/x").exists());

        // A lock left by a process that is gone (a pid far above the usual range).
        fs::write(dir.path().join(".locks/x"), "99999999").unwrap();
        let lock = installer.lock("x").unwrap();
        drop(lock);

        // Our own live lock: another install in this process waits, and cancel ends the wait.
        let held = installer.lock("y").unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(installer.lock("y").err().as_deref(), Some(CANCELED));
        drop(held);
    }

    /// A fake GitHub and nodejs.org on disk: `file://` URLs, which curl serves like any other.
    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().unwrap(),
            }
        }

        fn url(&self, relative: &str) -> String {
            format!("file://{}/{relative}", self.root.path().display())
        }

        fn sources(&self) -> Sources {
            Sources {
                github_api: self.url("api"),
                github: self.url("github"),
                node_dist: self.url("node"),
            }
        }

        fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
            let path = self.root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }

        /// A tarball at `relative` with `entries` (path, contents, executable).
        fn tarball(&self, relative: &str, entries: &[(&str, &str)]) -> PathBuf {
            let source = tempfile::tempdir().unwrap();
            for (path, contents) in entries {
                let file = source.path().join(path);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(&file, contents).unwrap();
                make_executable(&file).unwrap();
            }
            let archive = self.root.path().join(relative);
            fs::create_dir_all(archive.parent().unwrap()).unwrap();
            let status = Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(source.path())
                .arg(".")
                .status()
                .unwrap();
            assert!(status.success());
            archive
        }
    }

    fn config(name: &str, install: Install) -> ServerConfig {
        ServerConfig {
            name: name.into(),
            command: format!("flux-test-{name}"),
            args: vec![],
            extensions: vec![],
            file_names: vec![],
            initialization_options: None,
            settings: None,
            install: Some(install),
        }
    }

    #[test]
    fn github_release_install_with_checksum_and_progress() {
        let fixture = Fixture::new();
        let arch = github_arch().unwrap();
        let file = format!("tool-{arch}.tar.gz");
        let archive = fixture.tarball(
            &format!("dl/{file}"),
            &[
                ("tool-1.0/tool", "#!/bin/sh\necho tool\n"),
                ("tool-1.0/README", "x"),
            ],
        );
        let hash = sha256(&archive).unwrap();
        fixture.write(&format!("dl/{file}.sha256"), format!("{hash}  {file}\n"));
        fixture.write(
            "api/repos/acme/tool/releases/latest",
            json!({
                "tag_name": "v1.0",
                "assets": [
                    { "name": file, "size": fs::metadata(&archive).unwrap().len(),
                      "browser_download_url": fixture.url(&format!("dl/{file}")) },
                    { "name": format!("{file}.sha256"), "size": 99,
                      "browser_download_url": fixture.url(&format!("dl/{file}.sha256")) },
                ],
            })
            .to_string(),
        );
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let reports = Mutex::new(Vec::new());
        let progress = |p: Progress| reports.lock().unwrap().push(p);
        let installer = Installer {
            dir: dir.path().to_path_buf(),
            sources: fixture.sources(),
            progress: &progress,
            cancel: &cancel,
        };
        let config = config(
            "tool",
            Install::GitHubRelease {
                repo: "acme/tool".into(),
                asset: "tool-{arch}.tar.gz".into(),
                bin: "tool".into(),
            },
        );
        installer.install_with(&config, false).unwrap();

        let command = installed_in(dir.path(), &config).unwrap();
        assert_eq!(command, dir.path().join("tool/bin/tool"));
        let output = Command::new(&command).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "tool\n");
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(dir.path().join("tool/install.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["version"], "v1.0");
        // Nothing left behind: no staging, no lock.
        assert_eq!(fs::read_dir(dir.path().join(".tmp")).unwrap().count(), 0);
        assert!(!dir.path().join(".locks/tool").exists());
        let seen = reports.lock().unwrap().clone();
        assert!(seen.iter().any(|p| p.text == "Downloading tool"));

        // A second install finds it in place and does nothing.
        installer.install_with(&config, false).unwrap();
    }

    #[test]
    fn bad_checksums_and_missing_assets_fail_cleanly() {
        let fixture = Fixture::new();
        let arch = github_arch().unwrap();
        let file = format!("tool-{arch}.tar.gz");
        fixture.tarball(&format!("dl/{file}"), &[("tool", "#!/bin/sh\n")]);
        fixture.write(
            &format!("dl/{file}.sha256"),
            format!("{}  {file}\n", "0".repeat(64)),
        );
        fixture.write(
            "api/repos/acme/tool/releases/latest",
            json!({
                "tag_name": "v1",
                "assets": [
                    { "name": file, "browser_download_url": fixture.url(&format!("dl/{file}")) },
                    { "name": format!("{file}.sha256"),
                      "browser_download_url": fixture.url(&format!("dl/{file}.sha256")) },
                ],
            })
            .to_string(),
        );
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let progress = quiet();
        let installer = Installer {
            dir: dir.path().to_path_buf(),
            sources: fixture.sources(),
            progress: &progress,
            cancel: &cancel,
        };
        let tool = |asset: &str| {
            config(
                "tool",
                Install::GitHubRelease {
                    repo: "acme/tool".into(),
                    asset: asset.into(),
                    bin: "tool".into(),
                },
            )
        };
        let err = installer
            .install_with(&tool("tool-{arch}.tar.gz"), false)
            .unwrap_err();
        assert!(err.starts_with("Checksum mismatch"), "{err}");
        let err = installer
            .install_with(&tool("other-{arch}.zip"), false)
            .unwrap_err();
        assert!(err.contains("No other-"), "{err}");
        assert!(installed_in(dir.path(), &tool("tool-{arch}.tar.gz")).is_none());
        assert_eq!(fs::read_dir(dir.path().join(".tmp")).unwrap().count(), 0);

        // No network (here: no such file) and a pattern with the version: the API error is shown.
        let missing = config(
            "gone",
            Install::GitHubRelease {
                repo: "acme/gone".into(),
                asset: "gone-{tag}.tar.gz".into(),
                bin: "gone".into(),
            },
        );
        let err = installer.install_with(&missing, false).unwrap_err();
        assert!(err.starts_with("Download failed"), "{err}");
    }

    #[test]
    fn node_downloads_from_its_dist_with_checksum() {
        // Node.js itself, from a fake dist: index, checksums, tarball with bin/node.
        let fixture = Fixture::new();
        let (os, arch) = node_platform().unwrap();
        let version = "v24.0.0";
        let name = format!("node-{version}-{os}-{arch}");
        let tar_name = format!("{name}.tar.xz");
        let source = tempfile::tempdir().unwrap();
        let node = source.path().join(&name).join("bin").join("node");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::write(&node, "#!/bin/sh\necho v24\n").unwrap();
        make_executable(&node).unwrap();
        let archive = fixture
            .root
            .path()
            .join("node")
            .join(version)
            .join(&tar_name);
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let status = Command::new("tar")
            .arg("-cJf")
            .arg(&archive)
            .arg("-C")
            .arg(source.path())
            .arg(&name)
            .status()
            .unwrap();
        assert!(status.success());
        let hash = sha256(&archive).unwrap();
        fixture.write(
            &format!("node/{version}/SHASUMS256.txt"),
            format!("{hash}  {tar_name}\n"),
        );
        let file_key = match os {
            "darwin" => format!("osx-{arch}-tar"),
            os => format!("{os}-{arch}"),
        };
        fixture.write(
            "node/index.json",
            json!([{ "version": version, "lts": "Test", "files": [file_key] }]).to_string(),
        );
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let progress = quiet();
        let installer = Installer {
            dir: dir.path().to_path_buf(),
            sources: fixture.sources(),
            progress: &progress,
            cancel: &cancel,
        };
        let home = installer.ensure_node().unwrap();
        assert!(is_executable(&home.join("bin/node")));
        let manifest = fs::read_to_string(home.join("install.json")).unwrap();
        assert!(manifest.contains(version));
        // Already there: no second download.
        fs::remove_file(&archive).unwrap();
        installer.ensure_node().unwrap();
    }

    #[test]
    fn cancel_stops_before_downloading() {
        let fixture = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let progress = quiet();
        let installer = Installer {
            dir: dir.path().to_path_buf(),
            sources: fixture.sources(),
            progress: &progress,
            cancel: &cancel,
        };
        let config = config(
            "tool",
            Install::GitHubRelease {
                repo: "acme/tool".into(),
                asset: "tool".into(),
                bin: "tool".into(),
            },
        );
        assert_eq!(
            installer.install_with(&config, false).err().as_deref(),
            Some(CANCELED)
        );
    }

    #[test]
    fn check_explains_what_is_missing() {
        let npm = config(
            "x",
            Install::Npm {
                packages: vec!["x".into()],
                bin: "x".into(),
            },
        );
        assert_eq!(check(&npm), Ok(()));
        let mut none = npm.clone();
        none.install = None;
        assert_eq!(check(&none), Err(NO_INSTALLER.to_string()));
        let go = config(
            "gopls",
            Install::GoInstall {
                package: "golang.org/x/tools/gopls".into(),
                bin: "gopls".into(),
            },
        );
        let expected = match find_tool("go") {
            Some(_) => Ok(()),
            None => Err(NEEDS_GO.to_string()),
        };
        assert_eq!(check(&go), expected);
    }

    /// The recipes that the author's machine doesn't need (it has rust-analyzer and gopls), for
    /// real, into a temporary directory.
    #[test]
    #[ignore]
    fn real_rust_analyzer_from_github_and_gopls_from_go() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let progress = |p: Progress| eprintln!("{} {:?}", p.text, p.fraction);
        let installer = Installer {
            dir: dir.path().to_path_buf(),
            sources: Sources::default(),
            progress: &progress,
            cancel: &cancel,
        };
        let recipe = Install::GitHubRelease {
            repo: "rust-lang/rust-analyzer".into(),
            asset: "rust-analyzer-{arch}-apple-darwin.gz".into(),
            bin: "rust-analyzer".into(),
        };
        installer.run_recipe("rust-analyzer", &recipe).unwrap();
        let output = Command::new(dir.path().join("rust-analyzer/bin/rust-analyzer"))
            .arg("--version")
            .output()
            .unwrap();
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(output.status.success());

        if find_tool("go").is_some() {
            let recipe = Install::GoInstall {
                package: "golang.org/x/tools/gopls".into(),
                bin: "gopls".into(),
            };
            installer.run_recipe("gopls", &recipe).unwrap();
            let output = Command::new(dir.path().join("gopls/bin/gopls"))
                .arg("version")
                .output()
                .unwrap();
            eprintln!("{}", String::from_utf8_lossy(&output.stdout));
            assert!(output.status.success());
            let manifest = fs::read_to_string(dir.path().join("gopls/install.json")).unwrap();
            assert!(manifest.contains("\"version\": \"v"), "{manifest}");
        }
    }

    /// Two installs of one server at once (two windows): one downloads, the other waits and finds
    /// it installed.
    #[test]
    #[ignore]
    fn real_concurrent_installs_wait_for_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::fixtures::default_servers()
            .into_iter()
            .find(|c| c.name == "taplo")
            .unwrap();
        let downloads = Mutex::new(0);
        thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    let cancel = AtomicBool::new(false);
                    let downloaded = AtomicBool::new(false);
                    let progress = |p: Progress| {
                        if p.text == "Downloading taplo" {
                            downloaded.store(true, Ordering::Relaxed);
                        }
                    };
                    let installer = Installer {
                        dir: dir.path().to_path_buf(),
                        sources: Sources::default(),
                        progress: &progress,
                        cancel: &cancel,
                    };
                    installer.install_with(&config, false).unwrap();
                    if downloaded.load(Ordering::Relaxed) {
                        *downloads.lock().unwrap() += 1;
                    }
                });
            }
        });
        assert_eq!(*downloads.lock().unwrap(), 1);
        assert!(installed_in(dir.path(), &config).is_some());
    }

    #[test]
    fn npm_package_names_and_recipe_changes() {
        assert_eq!(package_name("typescript@6"), "typescript");
        assert_eq!(package_name("typescript"), "typescript");
        assert_eq!(package_name("@scope/pkg@1.2"), "@scope/pkg");
        assert_eq!(package_name("@scope/pkg"), "@scope/pkg");

        // An install made by another recipe doesn't count: a new Flux replaces it.
        let dir = tempfile::tempdir().unwrap();
        let tool = |asset: &str| {
            config(
                "tool",
                Install::GitHubRelease {
                    repo: "acme/tool".into(),
                    asset: asset.into(),
                    bin: "tool".into(),
                },
            )
        };
        let old = tool("tool-1.gz");
        let bin = dir.path().join("tool/bin/tool");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "").unwrap();
        make_executable(&bin).unwrap();
        assert_eq!(installed_in(dir.path(), &old), None, "no manifest");
        write_manifest(
            &dir.path().join("tool"),
            json!({ "recipe": recipe_key(old.install.as_ref().unwrap()) }),
        )
        .unwrap();
        assert_eq!(installed_in(dir.path(), &old), Some(bin));
        assert_eq!(installed_in(dir.path(), &tool("tool-2.gz")), None);
    }

    #[test]
    fn npm_servers_of_the_same_packages_share_an_install() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("node/bin/node");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::write(&node, "").unwrap();
        make_executable(&node).unwrap();
        let server = |name: &str, bin: &str| {
            config(
                name,
                Install::Npm {
                    packages: vec!["vscode-langservers-extracted".into()],
                    bin: bin.into(),
                },
            )
        };
        let json = server("vscode-json-language-server", "vscode-json-language-server");
        let html = server("vscode-html-language-server", "vscode-html-language-server");
        let home = dir.path().join("vscode-json-language-server");
        for bin in ["vscode-json-language-server", "vscode-html-language-server"] {
            let path = home.join("node_modules/.bin").join(bin);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
            make_executable(&path).unwrap();
        }
        assert_eq!(installed_in(dir.path(), &html), None, "no manifest yet");
        write_manifest(
            &home,
            json!({
                "recipe": recipe_key(json.install.as_ref().unwrap()),
                "packages": { "vscode-langservers-extracted": "4.10.0" },
            }),
        )
        .unwrap();
        assert_eq!(
            installed_in(dir.path(), &html),
            Some(home.join("node_modules/.bin/vscode-html-language-server"))
        );
        assert_eq!(
            installed_version(&installed_home(dir.path(), &html).unwrap().0, &html).as_deref(),
            Some("4.10.0")
        );
        // Another package set doesn't count.
        let other = config(
            "x-server",
            Install::Npm {
                packages: vec!["other".into()],
                bin: "vscode-html-language-server".into(),
            },
        );
        assert_eq!(installed_in(dir.path(), &other), None);
    }

    #[test]
    fn install_layout() {
        let dir = Path::new("/servers");
        let npm = Install::Npm {
            packages: vec!["pyright".into()],
            bin: "pyright-langserver".into(),
        };
        assert_eq!(
            command_path(dir, "pyright", &npm),
            Path::new("/servers/pyright/node_modules/.bin/pyright-langserver")
        );
        let rustup = Install::Rustup {
            component: "rust-analyzer".into(),
            fallback: Box::new(Install::GitHubRelease {
                repo: "rust-lang/rust-analyzer".into(),
                asset: "x".into(),
                bin: "rust-analyzer".into(),
            }),
        };
        assert_eq!(
            command_path(dir, "rust-analyzer", &rustup),
            Path::new("/servers/rust-analyzer/bin/rust-analyzer")
        );
    }
}
