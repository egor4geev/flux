//! The API Playground: every capability of the Flux plugin API 0.2 behind a button, with what it
//! returned under the button — the manual check of the API, and an example to copy from. Each
//! section of the tool window is one interface; background work (a started request, the server, a
//! started program, a terminal, a proposal, a timer) reports into its section as its events come.
//! The context menus' items print the context their command gets.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flux_plugin_api::diagnostics::{self, Diagnostic};
use flux_plugin_api::dialog::AskText;
use flux_plugin_api::git::{self, ChangeKind};
use flux_plugin_api::host::{editors, project, ui};
use flux_plugin_api::http::{self, EventStream, HttpError};
use flux_plugin_api::process::{self, OutputChannel};
use flux_plugin_api::review::{self, ProposalOutcome};
use flux_plugin_api::server::{self, ServerRequest, WsMessage};
use flux_plugin_api::view::*;
use flux_plugin_api::{
    CommandContext, Event, Plugin, Range, UiEvent, editor, notify, register_plugin, secrets,
    status, system, terminal, timers, tr, trf,
};

/// The tool window of the manifest.
const WINDOW: &str = "playground";
/// The status bar item of the clock.
const CLOCK: &str = "clock";
/// The key of the secrets' section.
const SECRET: &str = "demo";
/// Events a stream from the plugin's own server sends before it ends.
const TICKS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Network,
    Server,
    Programs,
    Terminal,
    Review,
    Git,
    Problems,
    Secrets,
    Timers,
    System,
    Context,
}

impl Section {
    fn id(self) -> &'static str {
        match self {
            Section::Network => "network",
            Section::Server => "server",
            Section::Programs => "programs",
            Section::Terminal => "terminal",
            Section::Review => "review",
            Section::Git => "git",
            Section::Problems => "problems",
            Section::Secrets => "secrets",
            Section::Timers => "timers",
            Section::System => "system",
            Section::Context => "context",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::Network => "Network",
            Section::Server => "Local Server",
            Section::Programs => "Programs",
            Section::Terminal => "Terminal",
            Section::Review => "Proposed Edits",
            Section::Git => "Git",
            Section::Problems => "Problems",
            Section::Secrets => "Secrets",
            Section::Timers => "Timers",
            Section::System => "System",
            Section::Context => "Context Menus",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Section::Network => {
                "Requests to the hosts the manifest allows: api.github.com and localhost."
            }
            Section::Server => {
                "The plugin's server on 127.0.0.1: a page, server-sent events, a WebSocket echo."
            }
            Section::Programs => "Programs the manifest lists: git.",
            Section::Terminal => {
                "Tabs with a command or the shell; FLUX_PLAYGROUND is in their environment."
            }
            Section::Review => "A proposed edit of the active file in a diff: accept or reject it.",
            Section::Git => "The project's repositories, read-only.",
            Section::Problems => "Read the problems; publish the plugin's own.",
            Section::Secrets => "A value in the macOS keychain.",
            Section::Timers => "A clock in the status bar, every second.",
            Section::System => "The clipboard, the home folder, the browser.",
            Section::Context => {
                "Right-click a selection in the editor, a file or a folder in the tree, a tab: the \
                 Playground's items print what their command gets."
            }
        }
    }
}

/// A request started in the background (`http.start`): what has come of it.
struct Stream {
    id: u64,
    status: Option<u16>,
    chunks: u32,
    bytes: usize,
    events: EventStream,
    messages: Vec<String>,
    done: Option<Result<(), String>>,
}

/// A program started in the background.
struct Spawned {
    id: u64,
    output: String,
    exit: Option<Option<i32>>,
}

struct Playground {
    /// The tool window is on screen: only then the view is sent.
    shown: bool,
    /// What each section's buttons returned (Markdown).
    results: HashMap<Section, String>,
    /// Counts of events that come by themselves: `git-changed`, `diagnostics-changed`.
    notes: HashMap<Section, String>,
    git_events: u32,
    diagnostics_events: u32,
    /// The plugin's server: its port, the requests it answered, the WebSockets open.
    port: Option<u16>,
    served: u32,
    last_request: Option<String>,
    sockets: HashSet<u64>,
    last_message: Option<String>,
    /// Streams of server-sent events the server sends: the request and the events sent; a timer
    /// sends the next one.
    streams: HashMap<u64, u32>,
    stream_timer: Option<u64>,
    stream: Option<Stream>,
    spawned: Option<Spawned>,
    /// The terminal with the command, and the shell text is sent to.
    command_terminal: Option<u64>,
    shell: Option<u64>,
    proposal: Option<u64>,
    secret_question: Option<u64>,
    /// The clock: its timer and the ticks so far.
    clock: Option<(u64, u32)>,
}

impl Plugin for Playground {
    fn new() -> Self {
        Playground {
            shown: false,
            results: HashMap::new(),
            notes: HashMap::new(),
            git_events: 0,
            diagnostics_events: 0,
            port: None,
            served: 0,
            last_request: None,
            sockets: HashSet::new(),
            last_message: None,
            streams: HashMap::new(),
            stream_timer: None,
            stream: None,
            spawned: None,
            command_terminal: None,
            shell: None,
            proposal: None,
            secret_question: None,
            clock: None,
        }
    }

    fn run_command(&mut self, command: &str, context: &CommandContext) {
        match command {
            "open" => ui::show(WINDOW),
            "show-context" | "show-file-context" | "show-folder-context" | "show-tab-context" => {
                self.show_context(command, context)
            }
            "stop-clock" if self.clock.is_some() => self.toggle_clock(),
            _ => {}
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::ToolWindowShown(window) if window == WINDOW => {
                self.shown = true;
                self.render();
            }
            Event::ToolWindowHidden(window) if window == WINDOW => self.shown = false,
            Event::Ui(input) if input.window == WINDOW && input.event == UiEvent::Clicked => {
                self.clicked(&input.element)
            }
            Event::TextAnswered((id, answer)) if Some(id) == self.secret_question => {
                self.secret_question = None;
                self.save_secret(answer);
            }
            Event::Timer(id) if Some(id) == self.clock.map(|(timer, _)| timer) => self.tick_clock(),
            Event::Timer(id) if Some(id) == self.stream_timer => self.tick_streams(),
            Event::HttpResponse(head) => {
                if let Some(stream) = self.stream.as_mut().filter(|s| s.id == head.request) {
                    stream.status = Some(head.status);
                    self.show_stream();
                }
            }
            Event::HttpBody((id, chunk)) => {
                if let Some(stream) = self.stream.as_mut().filter(|s| s.id == id) {
                    stream.chunks += 1;
                    stream.bytes += chunk.len();
                    for event in stream.events.feed(&chunk) {
                        stream.messages.push(event.data);
                    }
                    self.show_stream();
                }
            }
            Event::HttpDone((id, result)) => {
                if let Some(stream) = self.stream.as_mut().filter(|s| s.id == id) {
                    stream.done = Some(result.map_err(|err| err.message()));
                    self.show_stream();
                }
            }
            Event::ServerRequest(request) => self.serve(request),
            Event::StreamClosed(request) => {
                self.streams.remove(&request);
            }
            Event::WsOpened(open) => {
                if open.route() == "/ws" {
                    self.sockets.insert(open.connection);
                } else {
                    server::ws_close(open.connection);
                }
                self.show_server();
            }
            Event::WsMessage((connection, message)) => {
                let text = match message {
                    WsMessage::Text(text) => text,
                    WsMessage::Binary(bytes) => format!("{} bytes", bytes.len()),
                };
                let reply = format!("{text} (echoed by the plugin)");
                if let Err(err) = server::ws_send_text(connection, &reply) {
                    notify::warning(&tr("Couldn't answer the WebSocket"), Some(&err));
                }
                self.last_message = Some(text);
                self.show_server();
            }
            Event::WsClosed(connection) => {
                self.sockets.remove(&connection);
                self.show_server();
            }
            Event::ProcessOutput(output) => {
                if let Some(spawned) = self.spawned.as_mut().filter(|s| s.id == output.process) {
                    let text = String::from_utf8_lossy(&output.bytes);
                    match output.channel {
                        OutputChannel::Stdout => spawned.output.push_str(&text),
                        OutputChannel::Stderr => {
                            for line in text.lines() {
                                spawned.output.push_str(&format!("! {line}\n"));
                            }
                        }
                    }
                    self.show_spawned();
                }
            }
            Event::ProcessExited((id, code)) => {
                if let Some(spawned) = self.spawned.as_mut().filter(|s| s.id == id) {
                    spawned.exit = Some(code);
                    self.show_spawned();
                }
            }
            Event::TerminalExited((id, code)) => self.set(
                Section::Terminal,
                trf(
                    "Terminal {0}: the command ended with exit code {1}.",
                    &[&id, &exit_code(code)],
                ),
            ),
            Event::TerminalClosed(id) => {
                if self.shell == Some(id) {
                    self.shell = None;
                }
                if self.command_terminal == Some(id) {
                    self.command_terminal = None;
                }
                self.set(Section::Terminal, trf("Terminal {0} was closed.", &[&id]));
            }
            Event::ProposalAnswered((id, outcome)) => {
                if self.proposal == Some(id) {
                    self.proposal = None;
                }
                let text = match outcome {
                    ProposalOutcome::Accepted(text) => trf(
                        "Proposal {0} accepted: {1} lines, written by Flux.",
                        &[&id, &text.lines().count()],
                    ),
                    ProposalOutcome::Rejected => trf("Proposal {0} rejected.", &[&id]),
                    ProposalOutcome::Closed => {
                        trf("Proposal {0} closed without an answer.", &[&id])
                    }
                };
                self.set(Section::Review, text);
            }
            Event::GitChanged => {
                self.git_events += 1;
                self.notes.insert(
                    Section::Git,
                    trf("git-changed events: {0}", &[&self.git_events]),
                );
                self.render();
            }
            Event::DiagnosticsChanged(paths) => {
                self.diagnostics_events += 1;
                let last = paths.first().map(|path| relative(path)).unwrap_or_default();
                self.notes.insert(
                    Section::Problems,
                    trf(
                        "diagnostics-changed events: {0} (last: {1})",
                        &[&self.diagnostics_events, &last],
                    ),
                );
                self.render();
            }
            _ => {}
        }
    }
}

impl Playground {
    fn clicked(&mut self, element: &str) {
        match element {
            "clear" => {
                self.results.clear();
                self.render();
            }
            "network-fetch" => self.fetch_github(),
            "network-denied" => self.fetch_denied(),
            "network-stream" => self.stream_from_server(),
            "server-toggle" => self.toggle_server(),
            "server-open" => self.open_page(),
            "programs-run" => self.git_version(),
            "programs-spawn" => self.spawn_git_log(),
            "programs-denied" => self.run_denied(),
            "terminal-command" => self.terminal_command(),
            "terminal-shell" => self.terminal_shell(),
            "terminal-send" => self.terminal_send(),
            "review-propose" => self.propose_header(),
            "review-withdraw" => self.withdraw(),
            "git-repositories" => self.repositories(),
            "git-status" => self.git_status(),
            "git-diff" => self.git_diff(),
            "problems-list" => self.list_problems(),
            "problems-warn" => self.warn_here(),
            "problems-clear" => {
                diagnostics::clear();
                self.set(Section::Problems, tr("The playground's problems are cleared."));
            }
            "secrets-save" => {
                self.secret_question = Some(
                    AskText::new(&tr("Save a Secret"), &tr("Save"))
                        .message(&tr("The value goes to the macOS keychain under \"demo\"."))
                        .placeholder(&tr("A token, a password…"))
                        .send(),
                );
            }
            "secrets-read" => {
                let text = match secrets::get(SECRET) {
                    Some(value) => format!("`{SECRET}` = `{value}`"),
                    None => tr("No secret is saved."),
                };
                self.set(Section::Secrets, text);
            }
            "secrets-delete" => {
                let text = match secrets::remove(SECRET) {
                    Ok(()) => tr("The secret is deleted."),
                    Err(err) => failed(&err),
                };
                self.set(Section::Secrets, text);
            }
            "timers-clock" => self.toggle_clock(),
            "system-copy" => {
                system::copy_text("Copied by the Flux API Playground");
                self.set(Section::System, tr("Copied: paste it anywhere."));
            }
            "system-home" => self.home_folder(),
            "system-link" => {
                let text = match system::open_url("https://github.com/egor4geev/flux") {
                    Ok(()) => tr("Opened in the browser."),
                    Err(err) => failed(&err),
                };
                self.set(Section::System, text);
            }
            _ => {}
        }
    }

    // --- Network ---

    fn fetch_github(&mut self) {
        let started = Instant::now();
        let result = http::get("https://api.github.com/repos/egor4geev/flux")
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "flux-api-playground")
            .timeout(Duration::from_secs(20))
            .fetch();
        let took = started.elapsed().as_millis();
        let text = match result {
            Ok(response) if response.is_success() => {
                let repo: serde_json::Value = response.json().unwrap_or_default();
                format!(
                    "`GET api.github.com/repos/egor4geev/flux` → **{}** · {took} ms\n\n**{}** · ★ {} · \
                     {}: `{}`",
                    response.status,
                    repo["full_name"].as_str().unwrap_or("?"),
                    repo["stargazers_count"],
                    tr("default branch"),
                    repo["default_branch"].as_str().unwrap_or("?"),
                )
            }
            Ok(response) => format!(
                "`GET api.github.com` → **{}**\n\n```\n{}\n```",
                response.status,
                clip(&response.text(), 600)
            ),
            Err(err) => failed(&err.message()),
        };
        self.set(Section::Network, text);
    }

    fn fetch_denied(&mut self) {
        let text = match http::get("https://example.com/").fetch() {
            Err(HttpError::Denied(message)) => {
                format!("{} ✓\n\n> {message}", tr("Denied, as the manifest says."))
            }
            Err(err) => format!("{}: {}", tr("Failed, but not as denied"), err.message()),
            Ok(response) => trf(
                "Unexpected: the request went through ({0}).",
                &[&response.status],
            ),
        };
        self.set(Section::Network, text);
    }

    /// Streams the server's `/events` (a stream of server-sent events) through `http.start`.
    fn stream_from_server(&mut self) {
        let Some(port) = self.port else {
            self.set(Section::Network, tr("Start the local server first."));
            return;
        };
        let id = http::get(&server::url(port, "/events")).start();
        self.stream = Some(Stream {
            id,
            status: None,
            chunks: 0,
            bytes: 0,
            events: EventStream::new(),
            messages: Vec::new(),
            done: None,
        });
        self.show_stream();
    }

    fn show_stream(&mut self) {
        let Some(stream) = &self.stream else {
            return;
        };
        let status = stream
            .status
            .map_or_else(|| "…".to_string(), |status| status.to_string());
        let mut text = format!(
            "`GET /events` ({} {}) → **{status}** · {}: {} · {} {}",
            tr("request"),
            stream.id,
            tr("chunks"),
            stream.chunks,
            stream.bytes,
            tr("bytes"),
        );
        if !stream.messages.is_empty() {
            text.push_str(&format!("\n\n{}: {}", tr("events"), stream.messages.join(", ")));
        }
        match &stream.done {
            Some(Ok(())) => text.push_str(&format!("\n\n**{}**", tr("Done."))),
            Some(Err(err)) => text.push_str(&format!("\n\n{}", failed(err))),
            None => {}
        }
        self.set(Section::Network, text);
    }

    // --- The local server ---

    fn toggle_server(&mut self) {
        if self.port.take().is_some() {
            server::stop();
            self.sockets.clear();
            self.streams.clear();
            if let Some(timer) = self.stream_timer.take() {
                timers::cancel(timer);
            }
            self.set(Section::Server, tr("Stopped."));
            return;
        }
        match server::listen() {
            Ok(port) => {
                self.port = Some(port);
                self.show_server();
            }
            Err(err) => self.set(Section::Server, failed(&err)),
        }
    }

    fn open_page(&mut self) {
        let Some(port) = self.port else {
            self.set(Section::Server, tr("Start the server first."));
            return;
        };
        if let Err(err) = system::open_url(&server::url(port, "/")) {
            self.set(Section::Server, failed(&err));
        }
    }

    fn serve(&mut self, request: ServerRequest) {
        self.served += 1;
        self.last_request = Some(format!("{} {}", request.method, request.path));
        let port = self.port.unwrap_or_default();
        match request.route() {
            "/" => server::respond_html(
                request.id,
                200,
                &PAGE
                    .replace("{PORT}", &port.to_string())
                    .replace("{SERVED}", &self.served.to_string()),
            ),
            "/events" => {
                server::start_events(request.id);
                self.streams.insert(request.id, 0);
                if self.stream_timer.is_none() {
                    self.stream_timer = Some(timers::every(Duration::from_millis(400)));
                }
            }
            "/favicon.ico" => server::respond(request.id, 204, "image/x-icon", Vec::new()),
            _ => server::not_found(request.id),
        }
        self.show_server();
    }

    /// The next event of every stream the server sends; a stream ends after [`TICKS`].
    fn tick_streams(&mut self) {
        let mut ended = Vec::new();
        for (&request, sent) in self.streams.iter_mut() {
            *sent += 1;
            let sent_ok = server::send_event(request, Some("tick"), &format!("tick {sent}"));
            if *sent >= TICKS || sent_ok.is_err() {
                server::end_events(request);
                ended.push(request);
            }
        }
        for request in ended {
            self.streams.remove(&request);
        }
        if self.streams.is_empty()
            && let Some(timer) = self.stream_timer.take()
        {
            timers::cancel(timer);
        }
    }

    fn show_server(&mut self) {
        let Some(port) = self.port else {
            return;
        };
        let url = server::url(port, "/");
        let mut text = format!(
            "{} [{url}]({url}) · {}: {} · WebSockets: {}",
            tr("Listening on"),
            tr("requests"),
            self.served,
            self.sockets.len()
        );
        if let Some(request) = &self.last_request {
            text.push_str(&format!("\n\n{}: `{request}`", tr("last request")));
        }
        if let Some(message) = &self.last_message {
            text.push_str(&format!("\n\n{}: «{message}»", tr("WebSocket message")));
        }
        self.set(Section::Server, text);
    }

    // --- Programs ---

    fn git_version(&mut self) {
        let text = match process::run("git", &["--version"]) {
            Ok(output) => format!(
                "`git --version` → {} {}\n\n```\n{}\n```",
                tr("exit code"),
                exit_code(output.exit_code),
                output.stdout_text().trim()
            ),
            Err(err) => failed(&err),
        };
        self.set(Section::Programs, text);
    }

    fn run_denied(&mut self) {
        let text = match process::run("ls", &["-la"]) {
            Err(err) => format!("{} ✓\n\n> {err}", tr("Denied, as the manifest says.")),
            Ok(_) => tr("Unexpected: ls ran."),
        };
        self.set(Section::Programs, text);
    }

    fn spawn_git_log(&mut self) {
        match process::command("git")
            .args(["log", "--oneline", "-12"])
            .spawn()
        {
            Ok(id) => {
                self.spawned = Some(Spawned {
                    id,
                    output: String::new(),
                    exit: None,
                });
                self.show_spawned();
            }
            Err(err) => self.set(Section::Programs, failed(&err)),
        }
    }

    fn show_spawned(&mut self) {
        let Some(spawned) = &self.spawned else {
            return;
        };
        let state = match spawned.exit {
            None => tr("running…"),
            Some(code) => trf("exited with code {0}", &[&exit_code(code)]),
        };
        let text = format!(
            "`git log --oneline -12` ({} {}) — {state}\n\n```\n{}\n```",
            tr("process"),
            spawned.id,
            clip(spawned.output.trim_end(), 1500)
        );
        self.set(Section::Programs, text);
    }

    // --- Terminals ---

    fn terminal_command(&mut self) {
        let opened = terminal::command(["git", "log", "--oneline", "-5"])
            .title("Git Log")
            .env("FLUX_PLAYGROUND", "1")
            .open();
        let text = match opened {
            Ok(id) => {
                self.command_terminal = Some(id);
                trf(
                    "Terminal {0}: `git log --oneline -5` in the panel; its end comes as an event.",
                    &[&id],
                )
            }
            Err(err) => failed(&err),
        };
        self.set(Section::Terminal, text);
    }

    fn terminal_shell(&mut self) {
        let opened = terminal::shell()
            .title(&tr("Playground Shell"))
            .env("FLUX_PLAYGROUND", "hello from the API Playground")
            .in_editor()
            .focus()
            .open();
        let text = match opened {
            Ok(id) => {
                self.shell = Some(id);
                trf(
                    "Terminal {0}: the shell among the editor's tabs. Send echo types into it.",
                    &[&id],
                )
            }
            Err(err) => failed(&err),
        };
        self.set(Section::Terminal, text);
    }

    fn terminal_send(&mut self) {
        let Some(shell) = self.shell else {
            self.set(Section::Terminal, tr("Open the shell first."));
            return;
        };
        let text = match terminal::run_line(shell, "echo $FLUX_PLAYGROUND") {
            Ok(()) => trf("Typed `echo $FLUX_PLAYGROUND` into terminal {0}.", &[&shell]),
            Err(err) => failed(&err),
        };
        self.set(Section::Terminal, text);
    }

    // --- Proposed edits ---

    fn propose_header(&mut self) {
        let Some((document, text)) = editor::active_text() else {
            self.set(Section::Review, tr("Open a file first."));
            return;
        };
        let Some(path) = document.path else {
            self.set(Section::Review, tr("Save the document first."));
            return;
        };
        let proposed = format!("{}\n{text}", header_comment(&path));
        let sent = review::proposal(&path, &proposed)
            .title(&tr("Playground: a header comment"))
            .apply()
            .focus()
            .send();
        let text = match sent {
            Ok(id) => {
                self.proposal = Some(id);
                trf(
                    "Proposal {0} for {1}: accept (⌘↵) or reject it in its tab.",
                    &[&id, &relative(&path)],
                )
            }
            Err(err) => failed(&err),
        };
        self.set(Section::Review, text);
    }

    fn withdraw(&mut self) {
        match self.proposal.take() {
            Some(id) => {
                review::withdraw(id);
                self.set(Section::Review, trf("Proposal {0} withdrawn.", &[&id]));
            }
            None => self.set(Section::Review, tr("No proposal is open.")),
        }
    }

    // --- Git ---

    fn repositories(&mut self) {
        let repositories = git::repositories();
        let text = if repositories.is_empty() {
            tr("No repositories (or no `project` permission).")
        } else {
            repositories
                .iter()
                .map(|repo| {
                    let head = repo.head.as_deref().map_or("—", |head| &head[..head.len().min(8)]);
                    let mut line = format!(
                        "- `{}` · {}: **{}** · HEAD `{head}`",
                        repo.root,
                        tr("branch"),
                        repo.branch.as_deref().unwrap_or("(detached)"),
                    );
                    if let Some(operation) = &repo.operation {
                        line.push_str(&format!(" · {operation}"));
                    }
                    line
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        self.set(Section::Git, text);
    }

    fn git_status(&mut self) {
        let changes = git::status();
        let mut lines: Vec<String> = changes
            .iter()
            .take(20)
            .map(|change| {
                let kind = match change.kind {
                    ChangeKind::Added => "A",
                    ChangeKind::Modified => "M",
                    ChangeKind::Deleted => "D",
                    ChangeKind::Renamed => "R",
                    ChangeKind::Untracked => "?",
                    ChangeKind::Conflicted => "U",
                };
                format!("- `{kind}` {}", relative(&change.path))
            })
            .collect();
        if changes.len() > 20 {
            lines.push(trf("- … {0} more", &[&(changes.len() - 20)]));
        }
        let text = if changes.is_empty() {
            tr("No changes.")
        } else {
            format!("{}: {}\n\n{}", tr("Changes"), changes.len(), lines.join("\n"))
        };
        self.set(Section::Git, text);
    }

    fn git_diff(&mut self) {
        let Some(path) = editor::active().and_then(|document| document.path) else {
            self.set(Section::Git, tr("Open a file first."));
            return;
        };
        let text = match git::diff(&path) {
            Ok(diff) if diff.trim().is_empty() => trf("{0}: no changes against HEAD.", &[&relative(&path)]),
            Ok(diff) => format!(
                "{}\n\n```diff\n{}\n```",
                relative(&path),
                diff.lines().take(60).collect::<Vec<_>>().join("\n")
            ),
            Err(err) => failed(&err),
        };
        self.set(Section::Git, text);
    }

    // --- Problems ---

    fn list_problems(&mut self) {
        let files = diagnostics::all();
        let count = |severity: diagnostics::Severity, problems: &[Diagnostic]| {
            problems.iter().filter(|problem| problem.severity == severity).count()
        };
        let mut errors = 0;
        let mut warnings = 0;
        let mut lines = Vec::new();
        for file in &files {
            let (e, w) = (
                count(diagnostics::Severity::Error, &file.diagnostics),
                count(diagnostics::Severity::Warning, &file.diagnostics),
            );
            errors += e;
            warnings += w;
            if lines.len() < 10 {
                lines.push(format!(
                    "- {}: {} {e}, {} {w}, {} {}",
                    relative(&file.path),
                    tr("errors"),
                    tr("warnings"),
                    tr("all"),
                    file.diagnostics.len()
                ));
            }
        }
        let text = if files.is_empty() {
            tr("No problems (or no `project` permission).")
        } else {
            format!(
                "{}: {} · {}: {errors} · {}: {warnings}\n\n{}",
                tr("Files"),
                files.len(),
                tr("errors"),
                tr("warnings"),
                lines.join("\n")
            )
        };
        self.set(Section::Problems, text);
    }

    /// A warning of the plugin's own on the caret's line of the active document.
    fn warn_here(&mut self) {
        let Some((document, text)) = editor::active_text() else {
            self.set(Section::Problems, tr("Open a file first."));
            return;
        };
        let Some(path) = document.path else {
            self.set(Section::Problems, tr("Save the document first."));
            return;
        };
        let line = editors::selections(document.id)
            .first()
            .map_or(0, |selection| selection.end.line);
        let length = text
            .lines()
            .nth(line as usize)
            .map_or(0, |line| line.chars().count() as u32);
        let warning = Diagnostic::warning(
            Range::on_line(line, 0, length),
            &tr("A demo warning from the API Playground"),
        )
        .code("PG001");
        diagnostics::publish(&path, &[warning]);
        self.set(
            Section::Problems,
            trf(
                "A warning on line {0} of {1}: hover it, F2 goes to it.",
                &[&(line + 1), &relative(&path)],
            ),
        );
    }

    // --- Secrets ---

    fn save_secret(&mut self, answer: Option<String>) {
        let text = match answer {
            Some(value) => match secrets::set(SECRET, &value) {
                Ok(()) => tr("Saved in the keychain. Read reads it back — after a restart too."),
                Err(err) => failed(&err),
            },
            None => tr("Cancelled."),
        };
        self.set(Section::Secrets, text);
    }

    // --- Timers ---

    fn toggle_clock(&mut self) {
        if let Some((timer, ticks)) = self.clock.take() {
            timers::cancel(timer);
            status::hide(CLOCK);
            self.set(Section::Timers, trf("Stopped after {0} ticks.", &[&ticks]));
            return;
        }
        let timer = timers::every(Duration::from_secs(1));
        self.clock = Some((timer, 0));
        self.show_clock();
        self.set(
            Section::Timers,
            tr("Ticking every second: see the status bar (a click on it stops the clock)."),
        );
    }

    fn tick_clock(&mut self) {
        if let Some((_, ticks)) = self.clock.as_mut() {
            *ticks += 1;
        }
        self.show_clock();
    }

    fn show_clock(&self) {
        let Some((_, ticks)) = self.clock else {
            return;
        };
        status::item(&format!("⏱ {ticks} · {} UTC", utc_now()))
            .tooltip(&tr("API Playground: a timer every second; click to stop it"))
            .command("stop-clock")
            .show(CLOCK);
    }

    // --- System ---

    fn home_folder(&mut self) {
        let home = system::home_dir();
        let allowed = match std::fs::read_dir(format!("{home}/.config")) {
            Ok(entries) => trf(
                "`~/.config`: {0} entries — the `folders` permission",
                &[&entries.count()],
            ),
            Err(err) => format!("`~/.config`: {err}"),
        };
        let denied = match std::fs::read_dir(format!("{home}/Documents")) {
            Ok(_) => tr("`~/Documents`: readable — unexpected, it isn't in the permissions"),
            Err(err) => format!("`~/Documents`: {err} ✓"),
        };
        self.set(
            Section::System,
            format!("{}: `{home}`\n\n- {allowed}\n- {denied}", tr("Home folder")),
        );
    }

    // --- Context menus ---

    fn show_context(&mut self, command: &str, context: &CommandContext) {
        let document = match &context.editor {
            Some(document) => format!(
                "{} ({}, id {})",
                document.path.as_deref().map_or_else(|| tr("Untitled"), relative),
                document.language,
                document.id
            ),
            None => "—".to_string(),
        };
        let selection = match context.selected_text() {
            Some(text) if !text.is_empty() => format!("«{}»", clip(&text, 80)),
            Some(_) => tr("a cursor"),
            None => "—".to_string(),
        };
        let paths = if context.paths.is_empty() {
            "—".to_string()
        } else {
            context
                .paths
                .iter()
                .map(|path| format!("`{}`", relative(path)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let text = format!(
            "**{command}** · `{}`\n\n- {}: {document}\n- {}: {} · {selection}\n- {}: {paths}",
            context.source.name(),
            tr("document"),
            tr("selections"),
            context.selections.len(),
            tr("paths"),
        );
        notify::info(&trf("Context: {0}", &[&context.source.name()]));
        self.set(Section::Context, text);
        ui::show(WINDOW);
    }

    // --- The view ---

    /// Keeps a section's result and redraws.
    fn set(&mut self, section: Section, text: String) {
        self.results.insert(section, text);
        self.render();
    }

    fn render(&self) {
        if !self.shown {
            return;
        }
        let b = |id: &str, label: &str| -> Element { button(id, &tr(label)).into() };
        let server_label = if self.port.is_some() { "Stop" } else { "Start" };
        let clock_label = if self.clock.is_some() { "Stop Clock" } else { "Start Clock" };
        let sections = [
            (
                Section::Network,
                vec![
                    b("network-fetch", "Fetch"),
                    b("network-denied", "Denied Host"),
                    b("network-stream", "Stream"),
                ],
            ),
            (
                Section::Server,
                vec![b("server-toggle", server_label), b("server-open", "Open in Browser")],
            ),
            (
                Section::Programs,
                vec![
                    b("programs-run", "git --version"),
                    b("programs-spawn", "Spawn git log"),
                    b("programs-denied", "ls (denied)"),
                ],
            ),
            (
                Section::Terminal,
                vec![
                    b("terminal-command", "Run git log"),
                    b("terminal-shell", "Shell in Editor"),
                    b("terminal-send", "Send echo"),
                ],
            ),
            (
                Section::Review,
                vec![b("review-propose", "Propose Header"), b("review-withdraw", "Withdraw")],
            ),
            (
                Section::Git,
                vec![
                    b("git-repositories", "Repositories"),
                    b("git-status", "Status"),
                    b("git-diff", "Diff"),
                ],
            ),
            (
                Section::Problems,
                vec![
                    b("problems-list", "List"),
                    b("problems-warn", "Warn Here"),
                    b("problems-clear", "Clear"),
                ],
            ),
            (
                Section::Secrets,
                vec![
                    b("secrets-save", "Save…"),
                    b("secrets-read", "Read"),
                    b("secrets-delete", "Delete"),
                ],
            ),
            (Section::Timers, vec![b("timers-clock", clock_label)]),
            (
                Section::System,
                vec![
                    b("system-copy", "Copy Text"),
                    b("system-home", "Home Folder"),
                    b("system-link", "Open Link"),
                ],
            ),
            (Section::Context, Vec::new()),
        ];
        let mut children = vec![toolbar(
            "toolbar",
            [icon_button("clear", "trash", &tr("Clear Results"))],
        )];
        for (index, (section, buttons)) in sections.into_iter().enumerate() {
            if index > 0 {
                children.push(divider(&format!("divider-{index}")));
            }
            children.push(self.section(section, buttons));
        }
        set_view(WINDOW, column("root", children));
    }

    fn section(&self, section: Section, buttons: Vec<Element>) -> Element {
        let id = section.id();
        let mut children = vec![
            text(&format!("{id}-title"), [span(&tr(section.title())).bold()]),
            text(
                &format!("{id}-about"),
                [span(&tr(section.about())).tone(Tone::Dim)],
            ),
        ];
        if !buttons.is_empty() {
            children.push(row(&format!("{id}-buttons"), buttons));
        }
        if let Some(note) = self.notes.get(&section) {
            children.push(text(&format!("{id}-note"), [span(note).tone(Tone::Muted)]));
        }
        if let Some(result) = self.results.get(&section) {
            children.push(markdown(&format!("{id}-result"), result));
        }
        column(&format!("{id}-section"), children)
    }
}

/// The page the server answers `/` with: the requests it answered, a WebSocket that the plugin
/// echoes, a link to the stream of events.
const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>Flux API Playground</title>
<style>
  body { font: 15px/1.5 -apple-system, system-ui, sans-serif; margin: 48px; color: #1d1d1f; }
  code { background: #f2f2f4; padding: 1px 5px; border-radius: 4px; }
  #ws { font-weight: 600; }
</style>
<h1>Hello from the API Playground</h1>
<p>This page comes from the plugin's own server on <code>127.0.0.1:{PORT}</code>. Requests answered: {SERVED}.</p>
<p>WebSocket: <span id="ws">connecting…</span></p>
<p><a href="/events">/events</a> — server-sent events: five ticks, one every 0.4 s.</p>
<script>
  const ws = new WebSocket(`ws://${location.host}/ws`);
  const out = document.getElementById("ws");
  ws.onopen = () => ws.send("Hello from the browser");
  ws.onmessage = (event) => { out.textContent = event.data; };
  ws.onerror = () => { out.textContent = "failed"; };
</script>
"#;

/// A line that says who proposed the edit, as a comment of the file's language.
fn header_comment(path: &str) -> String {
    let extension = path.rsplit('.').next().unwrap_or_default();
    let text = "Proposed by the Flux API Playground.";
    match extension {
        "py" | "toml" | "yaml" | "yml" | "sh" | "bash" | "zsh" | "rb" | "conf" => format!("# {text}"),
        "md" | "html" | "xml" | "svg" => format!("<!-- {text} -->"),
        "css" => format!("/* {text} */"),
        _ => format!("// {text}"),
    }
}

/// A path from the project root, when it is in the project.
fn relative(path: &str) -> String {
    match project::root() {
        Some(root) => path
            .strip_prefix(&root)
            .map(|rest| rest.trim_start_matches('/'))
            .filter(|rest| !rest.is_empty())
            .unwrap_or(path)
            .to_string(),
        None => path.to_string(),
    }
}

fn exit_code(code: Option<i32>) -> String {
    code.map_or_else(|| tr("(a signal)"), |code| code.to_string())
}

fn failed(error: &str) -> String {
    format!("{}: {error}", tr("Failed"))
}

/// At most `max` characters of `text`, with "…" when cut.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// The time of day in UTC, "HH:MM:SS" (a plugin has the clock, not the time zone).
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
        % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

register_plugin!(Playground);
