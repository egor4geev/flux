//! The `claude` executable: where it is, its version, whether the user is signed in, and the
//! arguments of a session.

use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use flux_platform::process::Background;
use serde_json::Value;

use crate::types::{Effort, PermissionMode};

/// The oldest version the host protocol was checked with (wiki: ADR-030, the recordings of
/// `PROTOCOL.md`). Older ones may work; the chat warns.
pub const TESTED_VERSION: &str = "2.1.285";

/// A `claude` executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub path: PathBuf,
}

/// `claude auth status`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthStatus {
    pub logged_in: bool,
    /// "claude.ai" (a subscription), "console" (API billing)…
    pub method: Option<String>,
    pub email: Option<String>,
}

impl Cli {
    /// The executable to run: `FLUX_CLAUDE_PATH` (tests and scenarios — the fake CLI), the path the
    /// user chose, then `claude` on the `PATH` and where installers put it. Flux started from the
    /// Finder has a short `PATH`, hence the list. On Windows the `PATH` is searched by `PATHEXT`
    /// (`claude.exe`, npm's `claude.cmd`), then the installers' folders ([`windows_install_paths`]).
    pub fn locate(preferred: Option<&Path>) -> Option<Cli> {
        if let Some(path) = env::var_os("FLUX_CLAUDE_PATH").filter(|path| !path.is_empty()) {
            return Some(Cli {
                path: PathBuf::from(path),
            });
        }
        if let Some(path) = preferred.filter(|path| path.is_file()) {
            return Some(Cli {
                path: path.to_path_buf(),
            });
        }
        if cfg!(windows) {
            let path = env::var_os("PATH").unwrap_or_default();
            return flux_platform::process::find_in_path("claude", &path)
                .or_else(|| {
                    let app_data = env::var_os("APPDATA").map(PathBuf::from);
                    windows_install_paths(
                        flux_platform::paths::home().as_deref(),
                        app_data.as_deref(),
                    )
                    .into_iter()
                    .find(|path| path.is_file())
                })
                .map(|path| Cli { path });
        }
        search_path()
            .into_iter()
            .map(|dir| dir.join("claude"))
            .find(|path| path.is_file())
            .map(|path| Cli { path })
    }

    /// `claude --version`: "2.1.285".
    pub fn version(&self) -> Result<String, String> {
        let output = self
            .command()
            .arg("--version")
            .output()
            .map_err(|err| format!("Can't run {}: {err}", self.path.display()))?;
        let text = String::from_utf8_lossy(&output.stdout);
        text.split_whitespace()
            .next()
            .filter(|version| version.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(str::to_string)
            .ok_or_else(|| format!("Unexpected output of claude --version: {}", text.trim()))
    }

    /// `claude auth status` (JSON).
    pub fn auth_status(&self) -> Result<AuthStatus, String> {
        let output = self
            .command()
            .args(["auth", "status"])
            .output()
            .map_err(|err| format!("Can't run {}: {err}", self.path.display()))?;
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            format!(
                "Unexpected output of claude auth status: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            )
        })?;
        Ok(AuthStatus {
            logged_in: value["loggedIn"].as_bool().unwrap_or(false),
            method: value["authMethod"].as_str().map(str::to_string),
            email: value["email"].as_str().map(str::to_string),
        })
    }

    /// The command that signs in (it opens the browser): run in a terminal, where the user sees
    /// what it asks.
    pub fn login_command(&self) -> (PathBuf, Vec<String>) {
        (self.path.clone(), vec!["auth".into(), "login".into()])
    }

    /// One question without a session (part 9.2: a commit message): `claude -p` in `cwd` with no
    /// tools and no MCP servers, nothing saved to the history; the prompt goes through stdin.
    /// Blocks — run it on a background thread. `model` — an alias ("haiku") or an id; `None` — the
    /// account's default. `stop` set — the process is killed at once (the country check blocked
    /// Claude, part «Флаг страны»): [`ASK_STOPPED`]. The answer's text, or why there is none.
    pub fn ask(
        &self,
        cwd: &Path,
        prompt: &str,
        model: Option<&str>,
        stop: &AtomicBool,
    ) -> Result<String, String> {
        let mut child = self
            .command()
            .args(ask_args(model))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("{}: {err}", self.path.display()))?;
        // A long prompt (a diff) is written while the CLI reads; its output is read meanwhile.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let prompt = prompt.to_string();
        let writer = thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let read = |mut pipe: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut text = String::new();
                pipe.read_to_string(&mut text).ok();
                text
            })
        };
        let stdout = read(Box::new(child.stdout.take().expect("stdout is piped")));
        let stderr = read(Box::new(child.stderr.take().expect("stderr is piped")));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if stop.load(Ordering::Relaxed) => {
                    flux_platform::process::kill_tree(&mut child).ok();
                    child.wait().ok();
                    return Err(ASK_STOPPED.to_string());
                }
                Ok(None) if started.elapsed() > ASK_TIMEOUT => {
                    flux_platform::process::kill_tree(&mut child).ok();
                    child.wait().ok();
                    return Err(format!(
                        "Claude didn't answer in {} s.",
                        ASK_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(err) => return Err(err.to_string()),
            }
        };
        writer.join().ok();
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        ask_answer(&stdout, &stderr, status.code())
    }

    /// A command for this executable with the environment a session needs.
    /// Started out of sight: on Windows a console program would open a console window.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        command
            .stdin(Stdio::null())
            .env("PATH", search_path_env())
            // The SDK drops these from the child's environment: a debugger flag of Node would
            // break the CLI's own runtime.
            .env_remove("NODE_OPTIONS")
            .env_remove("DEBUG")
            .background();
        // The CLI on Windows looks for Git Bash only by the variable and the `git` on the `PATH`;
        // Flux also knows Git's usual folders and tells it where Git Bash is.
        if cfg!(windows)
            && let Some(bash) = git_bash()
        {
            command.env(GIT_BASH_VARIABLE, bash);
        }
        command
    }
}

/// How long [`Cli::ask`] waits for the answer.
const ASK_TIMEOUT: Duration = Duration::from_secs(90);
/// Why [`Cli::ask`] gave no answer when told to stop.
pub const ASK_STOPPED: &str = "Stopped.";

/// The arguments of [`Cli::ask`]: print mode with the result as JSON, no tools, no MCP servers
/// of the user's configuration, no transcript.
fn ask_args(model: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--no-session-persistence",
        "--tools=",
        "--strict-mcp-config",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if let Some(model) = model {
        args.push(format!("--model={model}"));
    }
    args
}

/// The answer in the CLI's JSON result (`{"type":"result","is_error":false,"result":"…"}`), or
/// the error it reports, or the last line of its stderr.
fn ask_answer(stdout: &str, stderr: &str, code: Option<i32>) -> Result<String, String> {
    let result = stdout
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| value["type"] == "result");
    match result {
        Some(result) if result["is_error"] != true => match result["result"].as_str() {
            Some(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
            _ => Err("Claude gave an empty answer.".to_string()),
        },
        Some(result) => Err(result["result"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(|text| text.trim().to_string())
            .or_else(|| {
                result["errors"]
                    .as_array()
                    .and_then(|errors| errors.first())
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Claude couldn't answer.".to_string())),
        None => Err(stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| match code {
                Some(code) => format!("claude exited with code {code}."),
                None => "claude was stopped.".to_string(),
            })),
    }
}

/// How a session starts.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchOptions {
    /// The project root: Claude works there.
    pub cwd: PathBuf,
    /// An alias ("opus") or a model id; `None` — the account's default.
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    pub effort: Option<Effort>,
    /// Continue a saved session by its id.
    pub resume: Option<String>,
    /// With `resume`: only the conversation up to this entry (`--resume-session-at`, a uuid of the
    /// transcript's chain) — a message edited and sent again continues from just before the old
    /// one (part 10.2).
    pub resume_at: Option<String>,
    /// The id of a new session (a UUID); `None` — the CLI makes one.
    pub session_id: Option<String>,
    /// More arguments the user gave in the settings.
    pub extra_args: Vec<String>,
}

impl LaunchOptions {
    /// The arguments of `claude` in host mode.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            // Readable (summarized) thinking instead of empty blocks.
            "--thinking-display",
            "summarized",
            // A subagent's text and thinking, not only its tool calls.
            "--forward-subagent-text",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        if let Some(model) = &self.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(mode) = self.permission_mode {
            args.extend(["--permission-mode".into(), mode.wire().into()]);
        }
        if let Some(effort) = self.effort {
            args.extend(["--effort".into(), effort.wire().into()]);
        }
        if let Some(resume) = &self.resume {
            args.extend(["--resume".into(), resume.clone()]);
            if let Some(at) = &self.resume_at {
                args.extend(["--resume-session-at".into(), at.clone()]);
            }
        }
        if let Some(id) = &self.session_id {
            args.extend(["--session-id".into(), id.clone()]);
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Where the Windows installers put `claude`, besides the `PATH`: the native installer's
/// `%USERPROFILE%\.local\bin\claude.exe`, npm's `%APPDATA%\npm\claude.cmd`, Bun's
/// `%USERPROFILE%\.bun\bin\claude.exe`.
pub fn windows_install_paths(home: Option<&Path>, app_data: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home {
        paths.push(home.join(".local").join("bin").join("claude.exe"));
    }
    if let Some(app_data) = app_data {
        paths.push(app_data.join("npm").join("claude.cmd"));
    }
    if let Some(home) = home {
        paths.push(home.join(".bun").join("bin").join("claude.exe"));
    }
    paths
}

/// The variable that tells the Claude CLI on Windows where Git Bash is.
pub const GIT_BASH_VARIABLE: &str = "CLAUDE_CODE_GIT_BASH_PATH";

/// The Git Bash the Claude CLI runs its commands in on Windows (it doesn't start without one):
/// see [`git_bash_in`]. `None` elsewhere and when Git for Windows isn't installed.
pub fn git_bash() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let configured = env::var_os(GIT_BASH_VARIABLE).map(PathBuf::from);
    let path = env::var_os("PATH").unwrap_or_default();
    let git = flux_platform::process::find_in_path("git", &path);
    let program_dirs = flux_platform::process::program_dirs();
    git_bash_in(
        configured.as_deref(),
        git.as_deref(),
        &program_dirs,
        |file| file.is_file(),
    )
}

/// What a terminal that runs `claude` adds to its environment: on Windows where Git Bash is (as
/// [`Cli::command`] tells the CLI), elsewhere nothing.
pub fn terminal_env() -> Vec<(String, String)> {
    git_bash()
        .map(|bash| {
            (
                GIT_BASH_VARIABLE.to_string(),
                bash.to_string_lossy().into_owned(),
            )
        })
        .into_iter()
        .collect()
}

/// Where Git Bash is: `configured` (`CLAUDE_CODE_GIT_BASH_PATH`) when the file is there; else
/// `bin\bash.exe` two folders above `git` (`…\Git\cmd\git.exe` → `…\Git\bin\bash.exe`, as the
/// CLI looks); else `Git\bin\bash.exe` in one of `program_dirs` (Program Files, the per-user
/// `%LOCALAPPDATA%\Programs`). `exists` says which files are there.
pub fn git_bash_in(
    configured: Option<&Path>,
    git: Option<&Path>,
    program_dirs: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let next_to_git = git
        .and_then(|git| git.parent()?.parent())
        .map(|dir| dir.join("bin").join("bash.exe"));
    let installed = program_dirs
        .iter()
        .map(|dir| dir.join("Git").join("bin").join("bash.exe"));
    configured
        .map(Path::to_path_buf)
        .into_iter()
        .chain(next_to_git)
        .chain(installed)
        .find(|bash| exists(bash))
}

/// Where to look for `claude`: the `PATH`, then where the native installer, Homebrew, npm and nvm
/// put it. On Windows the `PATH` and the installers' folders of [`windows_install_paths`].
fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    if cfg!(windows) {
        let app_data = env::var_os("APPDATA").map(PathBuf::from);
        let installers =
            windows_install_paths(flux_platform::paths::home().as_deref(), app_data.as_deref());
        for dir in installers.iter().filter_map(|path| path.parent()) {
            if !dirs
                .iter()
                .any(|known| flux_platform::same_path(known, dir))
            {
                dirs.push(dir.to_path_buf());
            }
        }
        return dirs;
    }
    if let Some(home) = flux_platform::paths::home() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".bun/bin"));
        dirs.extend(nvm_bins(&home.join(".nvm/versions/node")));
    }
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
            .into_iter()
            .map(PathBuf::from),
    );
    let mut unique = Vec::new();
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// The search path as a `PATH` value for the CLI: the commands Claude runs (git, cargo, node) are
/// found as in the user's terminal.
fn search_path_env() -> OsString {
    env::join_paths(search_path().into_iter().filter(|dir| dir.is_dir())).unwrap_or_default()
}

/// `bin` directories of the Node versions installed by nvm, newest first.
fn nvm_bins(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
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

/// Compares dotted versions: "2.1.285" < "2.1.300".
pub fn version_at_least(version: &str, minimum: &str) -> bool {
    let parts = |text: &str| -> Vec<u32> {
        text.split(['.', '-'])
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    parts(version) >= parts(minimum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_mode_arguments() {
        let options = LaunchOptions {
            cwd: PathBuf::from("/tmp/p"),
            model: Some("opus".into()),
            permission_mode: Some(PermissionMode::AcceptEdits),
            effort: Some(Effort::High),
            resume: Some("abc".into()),
            ..LaunchOptions::default()
        };
        let args = options.args();
        assert_eq!(
            &args[..5],
            [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json"
            ]
        );
        let joined = args.join(" ");
        assert!(joined.contains("--permission-prompt-tool stdio"));
        assert!(joined.contains("--model opus"));
        assert!(joined.contains("--permission-mode acceptEdits"));
        assert!(joined.contains("--effort high"));
        assert!(joined.contains("--resume abc"));
        assert!(!joined.contains("--resume-session-at"));
        let at = LaunchOptions {
            resume_at: Some("e1".into()),
            ..options.clone()
        };
        assert!(
            at.args()
                .join(" ")
                .contains("--resume abc --resume-session-at e1")
        );
        // Without a session to resume there is nothing to cut.
        let fresh = LaunchOptions { resume: None, ..at };
        assert!(!fresh.args().join(" ").contains("--resume-session-at"));
    }

    #[test]
    fn questions_without_a_session() {
        assert_eq!(
            ask_args(Some("haiku")),
            [
                "-p",
                "--output-format",
                "json",
                "--no-session-persistence",
                "--tools=",
                "--strict-mcp-config",
                "--model=haiku",
            ]
        );
        assert!(!ask_args(None).iter().any(|arg| arg.starts_with("--model")));

        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"  Fix the parser\n"}"#;
        assert_eq!(
            ask_answer(ok, "", Some(0)),
            Ok("Fix the parser".to_string())
        );
        let failed = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#;
        assert_eq!(
            ask_answer(failed, "", Some(1)),
            Err("Not logged in · Please run /login".to_string())
        );
        assert_eq!(
            ask_answer(
                "",
                "warming up\nerror: unknown option '--tools='\n",
                Some(1)
            ),
            Err("error: unknown option '--tools='".to_string())
        );
        assert_eq!(
            ask_answer("", "", Some(2)),
            Err("claude exited with code 2.".to_string())
        );
    }

    #[test]
    fn windows_installs_and_git_bash() {
        let home = Path::new(r"C:\Users\me");
        let app_data = Path::new(r"C:\Users\me\AppData\Roaming");
        let paths = windows_install_paths(Some(home), Some(app_data));
        let shown: Vec<String> = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let separator = std::path::MAIN_SEPARATOR;
        assert_eq!(
            shown[0],
            format!(r"C:\Users\me{separator}.local{separator}bin{separator}claude.exe")
        );
        assert_eq!(
            shown[1],
            format!(r"C:\Users\me\AppData\Roaming{separator}npm{separator}claude.cmd")
        );
        assert!(windows_install_paths(None, None).is_empty());

        let program_files = PathBuf::from(r"C:\Program Files");
        let bash = program_files.join("Git").join("bin").join("bash.exe");
        let only = |file: PathBuf| move |path: &Path| path == file;
        // Git's own folder, found without git on the PATH.
        assert_eq!(
            git_bash_in(
                None,
                None,
                std::slice::from_ref(&program_files),
                only(bash.clone())
            ),
            Some(bash.clone())
        );
        // Next to the git on the PATH, as the CLI looks.
        let scoop = Path::new("D:").join("scoop").join("git");
        let git = scoop.join("cmd").join("git.exe");
        let scoop_bash = scoop.join("bin").join("bash.exe");
        assert_eq!(
            git_bash_in(None, Some(&git), &[], only(scoop_bash.clone())),
            Some(scoop_bash)
        );
        // The user's variable first; a missing file is passed over.
        let custom = Path::new("E:").join("bash.exe");
        assert_eq!(
            git_bash_in(Some(&custom), None, &[], only(custom.clone())),
            Some(custom.clone())
        );
        assert_eq!(
            git_bash_in(Some(&custom), None, &[program_files], only(bash.clone())),
            Some(bash)
        );
        assert_eq!(git_bash_in(None, None, &[], |_| true), None);
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(version_at_least("2.1.285", "2.1.285"));
        assert!(version_at_least("2.1.300", "2.1.285"));
        assert!(version_at_least("2.2.0", "2.1.285"));
        assert!(!version_at_least("2.1.99", "2.1.285"));
        assert!(!version_at_least("1.9.999", "2.1.285"));
    }
}
