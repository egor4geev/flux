# Writing plugins for Flux

A Flux plugin adds commands, tool windows, context menu items, status bar items and settings to the
editor, and reacts to what happens in the window: documents opened, edited and saved, the active tab
changing, the project changing. With the user's permission it goes further: it talks to web
services, runs a server on the user's Mac for them to call back, runs programs and terminals,
proposes edits the user reviews in a diff, reads Git and the problems of the code and publishes its
own. It asks the user with forms and popup lists, makes a task's branch the way the Branches popup
does, turns keys like `TRACK-142` into its links wherever they appear, opens Claude Code sessions
with its context and keeps a task's tabs. Plugins look like Flux itself — they describe their windows
with elements, and Flux draws them in its own design system.

A plugin without code can add languages, their grammars and language servers: [Language plugins](languages.md);
color themes: [Themes](themes.md); sets of file icons: [File icon sets](icon-themes.md).

> [!NOTE]
> The plugin API is version **0.3**. A manifest names the version it is built for; Flux keeps
> running plugins built for **0.2**, the version the [catalog](catalog.md) opened with
> ([From 0.2 to 0.3](#from-02-to-03)).

## How plugins run

A plugin is a **WebAssembly component** of the `flux:plugin` world, run by
[wasmtime](https://wasmtime.dev) inside Flux. The API is described in WIT —
[`crates/flux-plugin/wit/v0.3/flux-plugin.wit`](../crates/flux-plugin/wit/v0.3/flux-plugin.wit) is
the reference (`wit/v0.2` — the previous version, which Flux still runs); this guide summarizes it.
Plugins are written in Rust with the SDK ([`crates/flux-plugin-api`](../crates/flux-plugin-api)) and
built for the `wasm32-wasip2` target.

- **Isolated.** A plugin can't crash or freeze Flux. Each one runs on its own thread; a panic, a call
  longer than **10 seconds** of its own work or more than **512 MB** of memory stops the plugin, and
  Flux tells the user (with the details) and offers to start it again.
- **Sandboxed.** A plugin sees only what its [permissions](#permissions) give it. Without any it has
  its own data folder, its settings, notifications, questions, tool windows, menu items, the open
  documents, timers, secrets, the clipboard and links in the browser. The project's files, the
  network, a server, programs, terminals, folders outside the project and Claude Code sessions need
  a permission each, which the user sees before installing. No environment variables.
- **One file for every platform.** The component is the same on macOS, Linux and Windows. Flux
  compiles it when it first loads it (tens of milliseconds) and keeps the result in a cache
  (`~/Library/Caches/flux/plugins`, the 64 most recently used).
- **Calls are cheap** — a fraction of a microsecond. Flux calls the plugin one call at a time and
  never waits for it; a call that needs the window (the text of a document, a terminal, Git) waits on
  the plugin's thread for Flux's answer — 5 seconds at most for documents, 10 for the rest — then it
  gets an empty result or an error.
- **Waiting isn't working.** Waiting inside a call — for a response, a program, the window — doesn't
  count toward the 10 seconds; long work goes to the background and reports back as events
  ([Background work](#background-work-and-the-time-limit)).
- **Declared up front.** The commands, tool windows, menu items and status bar items a plugin uses are
  those of its manifest: a call naming another one is ignored (the plugin's log says so), and a
  notification's action naming an unknown command is dropped.

## Quick start

1. Install the target once: `rustup target add wasm32-wasip2`.
2. Copy [`templates/plugin`](../templates/plugin) somewhere and rename it. In its `Cargo.toml`, point
   the `flux-plugin-api` dependency at your checkout of Flux (or use the `git` form in the comment).
3. Set the plugin's `id` and `name` in `flux-plugin.toml`.
4. In Flux: **Settings → Plugins → ⚙ → Install Plugin from Disk…** and pick the folder. A folder
   becomes a **plugin under development**: Flux builds it with cargo, loads it, and reloads it whenever
   its component changes. Rebuild it yourself (`cargo build --release --target wasm32-wasip2`), or
   run **Reload Dev Plugins** from the command palette (⇧⌘P), which rebuilds and reloads.
5. Run **Hello: Say Hello** from the palette or from the editor's context menu, open the
   **Documents** window from its icon in the launchpad.

What the plugin logs and prints is in **Settings → Plugins → the plugin → Log**.

[`examples/api-playground`](../examples/api-playground) is a plugin with every capability of the API
behind a button — install it the same way to see each one work, and copy from it.

## A plugin in Rust

```rust
use flux_plugin_api::notify::Notice;
use flux_plugin_api::view::*;
use flux_plugin_api::{CommandContext, Event, Plugin, UiEvent, register_plugin, tr, trf};

struct Counter {
    count: u32,
}

impl Plugin for Counter {
    fn new() -> Self {
        Counter { count: 0 }
    }

    // `[[commands]] id = "count"` in the manifest.
    fn run_command(&mut self, command: &str, _context: &CommandContext) {
        if command == "count" {
            self.add();
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            // `[[tool-windows]] id = "counter"`.
            Event::ToolWindowShown(window) if window == "counter" => self.render(),
            Event::Ui(input) if input.element == "add" && input.event == UiEvent::Clicked => {
                self.add()
            }
            _ => {}
        }
    }
}

impl Counter {
    fn add(&mut self) {
        self.count += 1;
        if self.count % 10 == 0 {
            Notice::success(&trf("{0} already!", &[&self.count]))
                .action(&tr("One More"), "count")
                .send();
        }
        self.render();
    }

    fn render(&self) {
        set_view(
            "counter",
            column("root", [
                label("count", &trf("Count: {0}", &[&self.count])),
                button("add", &tr("Add One")).primary().into(),
            ]),
        );
    }
}

register_plugin!(Counter);
```

The crate is a `cdylib` with its own `[workspace]`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
flux-plugin-api = { git = "https://github.com/egor4geev/flux" }

[profile.release]
opt-level = "s"
lto = true
strip = true

[workspace]
```

[`Plugin`](../crates/flux-plugin-api/src/lib.rs) has four methods, all optional but `new`:

| Method | Called when |
|--------|-------------|
| `activate` | The plugin was loaded: the window opened, or the user turned the plugin on. |
| `deactivate` | It is about to be unloaded: turned off, reloaded, the window is closing. |
| `run_command(id, context)` | One of the manifest's commands ran: the palette, its keys, a context menu, a notification's action, a status bar item. The [context](#commands-and-their-context) says from where and on what. |
| `on_event(event)` | Something happened in the window, an answer to a question came, or background work reported ([Events](#events)). |

The SDK's shortcuts:

| | |
|-|-|
| `tr(text)`, `trf(template, &[&a, &b])` | The text in the interface language ([Localization](#localization)). |
| `setting::<T>(key)` | A value of the plugin's settings. |
| `log::{debug, info, warn, error}` | Lines of the plugin's log. |
| `notify::{info, success, warning, error}`, `notify::Notice` | Notifications, with a body, actions, a sticky card. |
| `dialog::Ask`, `dialog::AskText` | Questions with buttons or a text field. |
| `editor::{active, active_text, selected_text, open}` | The documents. |
| `storage::{get, set, remove, dir}` | The plugin's data, kept between launches. |
| `status::{set, hide, item}` | The plugin's status bar items. |
| `view::*` | A tool window's content ([Tool windows](#tool-windows)). |
| `CommandContext` — `path`, `selection`, `selected_text`, `from_menu` | What a command acts on. |
| `http::{get, post, request…}`, `http::EventStream` | Requests to web services ([The network](#the-network)). |
| `server::{listen, respond_html, start_events, ws_send_text…}` | The plugin's server on 127.0.0.1 ([A local server](#a-local-server)). |
| `process::{run, command}` | Programs ([Programs](#programs)). |
| `terminal::{shell, command, run_line}` | Terminal tabs ([Terminals](#terminals)). |
| `review::proposal` | Edits the user reviews in a diff ([Proposed edits](#proposed-edits)). |
| `git::{repositories, status, diff, repository_of}` | The project's repositories ([Git](#git)). |
| `diagnostics::{all, of, publish, clear}`, `Diagnostic::warning…` | Problems in files ([Problems](#problems)). |
| `secrets::{get, set, remove}`, `timers::{after, every}`, `system::{open_url, copy_text, home_dir}` | [Secrets, timers and the system](#secrets-timers-and-the-system). |
| `system::{now, local_time, utc_offset, time_zone, open_settings}` | The clock in the user's time zone; Settings on the plugin's page. |
| `dialog::Form` | A dialog with fields ([Forms](#forms)). |
| `picker::{Picker, item, update, close}` | Popup lists: a search, a menu ([Pickers](#pickers)). |
| `settings::{set, reset, status, busy}` | Writing settings, the settings page's lines ([The settings page](#the-settings-page)). |
| `git::{branches, new_branch, checkout, commit_template}` | Branches the way Flux makes them, the commit message's template ([Git](#git)). |
| `links::{set, pattern, key_regex, tooltip}` | Links on keys like `TRACK-142` ([Links](#links)). |
| `claude::session` | A Claude Code session with the plugin's context ([Claude sessions](#claude-sessions)). |
| `contexts::{switch, remove}` | A task's tabs ([Contexts](#contexts)). |
| `offset`, `slice`, `Position::new`, `Range::on_line`… | Positions in a document's text. |
| `host::*` | Every interface of the API, as generated from the WIT. |

## The manifest

`flux-plugin.toml` in the plugin's folder says who the plugin is, what it may do and what it adds.
Flux reads it before running any code: the manager shows it, the install question lists its
permissions, the palette its commands. Unknown keys are errors — a typo doesn't go unnoticed.

```toml
id = "someone.hello"
name = "Hello"
version = "0.1.0"
api = "0.3"
authors = ["Someone"]
description = "Says hello."
repository = "https://github.com/someone/hello"
icon = "icons/hello.svg"
wasm = "target/wasm32-wasip2/release/hello_plugin.wasm"

[permissions]
project = "read"
network = ["api.example.com"]

[notifications]
display = "balloon"

[[commands]]
id = "hello"
title = "Say Hello"
category = "Hello"
keys = "alt-cmd-shift-h"

[[menus]]
command = "hello"
location = "editor"

[[tool-windows]]
id = "hello"
title = "Hello"
icon = "icons/hello.svg"

[[status-items]]
id = "count"

[[settings]]
key = "greeting"
title = "Greeting"
type = "string"
default = "Hello!"
```

| Key | | |
|-----|-|-|
| `id` | required | Lowercase letters and digits, words joined by `.` or `-`: `someone.hello-world`. The plugin's key everywhere: its folder, settings, notification group, secrets. |
| `name` | required | Shown in the manager, the palette's chips, Settings. |
| `version` | required | The plugin's version. |
| `api` | required | The plugin API it is built for: `"0.3"`. Flux runs 0.3 and 0.2 ([From 0.2 to 0.3](#from-02-to-03)); a plugin with code names the version of the SDK it was built with — Flux checks it against the component. |
| `authors` | `[]` | |
| `description` | `""` | Shown in the manager. |
| `repository` | — | A link for the manager. |
| `icon` | — | A monochrome SVG in the folder, for the manager. |
| `wasm` | — | The component, relative to the folder. A plugin without it has no code: it starts at once and only declares. |

### Permissions

**`[permissions]`** — what the plugin may do beyond its own folder and the API every plugin has. The
install question lists them in plain words, Settings → Plugins → the plugin → Permissions too; the
sandbox and the API enforce them, and a call they don't allow fails with an error that names the
missing permission. Ask for what the plugin needs, no more: the user decides by this list.

| Key | Default | What it gives |
|-----|---------|---------------|
| `project` | `"none"` | The project's content. `"read"`: the project folder in the plugin's file system (at its real path), `project.search`, [Git](#git), reading [problems](#problems), [proposed edits](#proposed-edits) (the answer carries the file's text), the commit message's template, [contexts](#contexts). `"write"`: also writing the project's files through the file system and changing its Git repositories — a new branch, a checkout. |
| `network` | `[]` | Requests to the hosts listed ([The network](#the-network)): `"api.example.com"`; `"*.example.com"` — the domain and its subdomains; `"localhost"` — with `127.0.0.1` and `::1`; `"*"` — any host, which the install question warns about. A host, not a URL: no scheme, port or path. Every redirect is checked against the list too. |
| `server` | `false` | A server of the plugin's own on `127.0.0.1` ([A local server](#a-local-server)): a web service's sign-in redirects to it, programs talk to the IDE through it. |
| `processes` | `[]` | Programs by name ([Programs](#programs)): `["git", "gh"]`; `["*"]` — any, which the install question warns about. A program runs with the user's rights: it can do whatever the user can. |
| `terminal` | `false` | Terminal tabs ([Terminals](#terminals)): open them with a command or the user's shell, type into them. A shell runs whatever is typed into it, so this is as strong as any program — but the user sees every tab. |
| `folders` | `[]` | Folders outside the project in the plugin's file system, at their real paths: `[{ path = "~/.config/gh", access = "read" }]`. `~` is the home folder ([`system.home-dir`](#secrets-timers-and-the-system)); `access` is `"read"` (the default) or `"write"`. A folder that doesn't exist is skipped. |
| `claude` | `false` | Claude Code sessions the plugin opens with its own context and requests ([Claude sessions](#claude-sessions)). A session runs with the rights the user gave Claude Code — it can edit files and run commands — so the install question warns about it. |

**`[notifications]`** — `display`: how the plugin's notifications show until the user changes it in
Settings → Notifications: `"balloon"` (a card that goes away, the default), `"sticky"` (a card that
stays), `"log"` (the Notifications window only), `"hidden"`.

**`[[commands]]`** — a command for the palette («category: title»), its keys, context menus and
notification actions.

| Key | | |
|-----|-|-|
| `id` | required | What `run_command` gets. |
| `title` | required | Also the label of its menu items. |
| `category` | the plugin's name | The palette's chip. |
| `keys` | — | A default shortcut in gpui's notation, in macOS keys: `cmd-shift-h`, `alt-f7`. On Windows ⌘ becomes Ctrl and ⌥ Alt (`ctrl-shift-h`). A key Flux already uses is not taken (the plugin's log says so). |
| `palette` | `true` | `false` — not in the palette: a command only a notification's action, a settings button or a status bar item runs. |

**`[[menus]]`** — an item of a context menu that runs one of the plugin's commands on what the menu
was opened on ([Commands and their context](#commands-and-their-context)). Flux adds the plugins'
items below its own; an item shows while the plugin runs and hasn't grayed the command out
(`commands.set-enabled`).

| Key | | |
|-----|-|-|
| `command` | required | One of `[[commands]]`: its title is the item's label. |
| `location` | required | `"editor"` — the editor's menu (the right button over the text, ⇧F10); `"tree"` — the project tree's; `"tab"` — a tab's. |
| `when` | always | `"selection"` — the editor has a selection (`editor`); `"file"` — on a file (`tree`, `tab`); `"folder"` — on a folder (`tree`). |

**`[[tool-windows]]`** — a window in the island on the right, opened by its icon in the launchpad —
or a tab among the editor's tabs ([Tool windows](#tool-windows)).

| Key | | |
|-----|-|-|
| `id` | required | The window the plugin fills with `ui.set-view`. |
| `title` | required | `ui.set-title` changes it while the plugin runs. |
| `icon` | a puzzle piece | A monochrome 16×16 SVG in the folder, drawn in the launchpad's colors. |
| `keys` | — | A default shortcut that opens and hides the window. |
| `location` | `"island"` | `"editor"` — a tab among the editor's tabs, for something wide: a board, a preview. No launchpad icon: `ui.show` and the palette open it. |

**`[[status-items]]`** — `id`: an item of the status bar; the plugin sets its text with
`status-bar.set` (`status::set`).

**`[[settings]]`** — a setting; Settings → the plugin shows a form of them, in their order. Values
are kept in `settings.json` under `plugins.settings.<id>` (a secret — apart, encrypted); a change
comes as the `settings-changed` event.

| Key | | |
|-----|-|-|
| `key` | required | What `setting(key)` reads. |
| `title` | required | |
| `description` | — | Under the field. |
| `type` | required | With a value: `bool` (a switch), `string` (a field), `text` (several lines: a prompt, a template), `integer` (a number field), `choice` (one of `options`), `string-list` (a list of strings, a row each), `records` (a list of records, below). Without one: `secret`, `button`, `status`, `section` (below). |
| `default` | `false`, `""`, `0` or `min`, the first option, `[]` | Must fit the type. A `status`'s text until the plugin writes one. `records` — `[[settings.default]]` tables. A `secret` has none. |
| `min`, `max` | — | For `integer`. |
| `options` | — | For `choice`: `[{ value = "file", title = "Current File" }, …]`. |
| `command`, `argument` | — | For `button`: one of `[[commands]]`, and what its context's `argument` carries. |

The kinds without a value make the page a plugin's own place — a sign-in, an organization, what a
plugin asks of the user once ([The settings page](#the-settings-page)):

- `section` — a heading with its description; the settings below it are its group.
- `secret` — a masked field, kept with the plugin's secrets, never in `settings.json`: the
  plugin's secret named by the setting's key (`secrets::get(key)`); `setting(key)` reads none.
- `button` — runs the plugin's command with `argument` in its context; grayed out while the plugin
  isn't running. Buttons in a row share a line.
- `status` — a line the plugin writes (`settings::status`): «Signed in as anna@example.com».
- `records` — a list of records with the same fields, a card each, which the user adds, removes and
  moves: the plugin's own actions, bookmarks. `[[settings.fields]]` — `key`, `title`, `type`
  (`string`, `text`, `bool`, `choice` with `options`), `default`; `[[settings.default]]` — the
  records it starts with. The value is a list of objects by the fields' keys.

```toml
[[settings]]
key = "account"
type = "section"
title = "Account"

[[settings]]
key = "token"
type = "secret"
title = "Token"
description = "A personal token of the service."

[[settings]]
key = "check-button"
type = "button"
title = "Check Connection"
command = "check-connection"

[[settings]]
key = "connection"
type = "status"
title = "Connection"
default = "Not checked"

[[settings]]
key = "actions"
type = "records"
title = "Claude actions"

[[settings.fields]]
key = "name"
title = "Name"
type = "string"

[[settings.fields]]
key = "prompt"
title = "Prompt"
type = "text"

[[settings.fields]]
key = "mode"
title = "Mode"
type = "choice"
options = [{ value = "default", title = "As usual" }, { value = "plan", title = "Plan mode" }]

[[settings.default]]
name = "Take to Work"
prompt = "Implement {key}: {summary}."
mode = "default"
```

## The API

Each interface of [the WIT](../crates/flux-plugin/wit/v0.3/flux-plugin.wit) is a module of `host`
(`host::editors::open`); the SDK's shortcuts above cover the common cases. Positions in documents are
zero-based lines and columns **in characters** (Unicode scalar values), as Flux counts them; lines
end at `\n`. Documents are named by ids that stay the same while the document is open; notifications,
questions, requests, programs, terminals, proposals and timers by ids the plugin gets back. Paths the
plugin passes are relative to the project root or absolute; paths Flux gives back are absolute.

| Interface | Functions |
|-----------|-----------|
| `log` | `write(level, message)` — `debug`, `info`, `warn`, `error`. |
| `i18n` | `language()` — `"en"`, `"ru"`; `translate(text)`. |
| `commands` | `set-enabled(command, enabled)` — grays a command out in the palette and the menus (it stops running). |
| `notifications` | `notify(notification) -> id`; `update(id, notification)`; `set-progress(id, progress)` — a task in progress (indeterminate or a fraction; none — over); `expire(id)` — its actions stop working; `remove(id)`. A notification has a kind (info, success, warning, error), a title, a body, actions (a label, one of the plugin's commands and an `argument` the command gets — which of several notifications) and `sticky`. |
| `dialogs` | `ask(question) -> id` — a question with buttons (roles: primary, normal, danger, cancel), a message and monospace details; `ask-text(question) -> id` — a question with a text field; `ask-form(form) -> id` — a dialog with fields ([Forms](#forms)). The answers come as events. |
| `editors` | `active()`, `list()` — the documents (`id`, `path`, `language`, `modified`); `text(id)`; `selections(id)` — the primary first; `set-selections(id, ranges)`; `edit(id, edits)` — one undo step, ranges in the current text, not overlapping; `open(path, selection) -> id` — opens a file in a tab or goes to it, selects the span in the middle of the view; `save(id)` — starts saving (`editor-saved` says when it's on disk); `close(id)` — closes the tab, unless the document has unsaved changes (then an error: they are the user's to decide about). |
| `project` | `root()`; `search(query, max-matches)` — the project's files by the rules of Find in Files (`.gitignore`, no binary or huge files): lines with matches, paths relative to the root. A query is text with `case-sensitive`, `whole-word` and `regex` (the syntax of the Rust `regex` crate). It needs the `project` permission and blocks the plugin, not Flux. |
| `storage` | `get(key)`, `set(key, value)` — a small store kept between launches; `data-dir()` — the plugin's folder, readable and writable. |
| `settings` | `get(key)` — the value as JSON: the user's, or the manifest's default (none for a secret, a button, a status, a section); `set(key, value?)` — writes one; `set-status(key, status?)` — a `status` line ([The settings page](#the-settings-page)). |
| `status-bar` | `set(id, item)` — the text, a tooltip and a command a click runs; none hides it. |
| `ui` | `set-view(window, view)` — a tool window's content; `show(window)`, `hide(window)`; `set-title(window, title)` — its header, its tab. |
| `http` | `fetch(request)` — a request and its whole response; `start(request) -> id` — a request in the background, its response as events; `cancel(id)`. `network` permission. |
| `server` | `listen(port?) -> port`, `stop()`; `respond(id, response)`, `respond-stream(id, status, headers)`, `send-chunk(id, bytes)`, `end-stream(id)`; `ws-send(connection, message)`, `ws-close(connection)`. `server` permission. |
| `process` | `run(command, stdin, timeout-ms)` — a program to its end; `spawn(command) -> id` — in the background, its output as events; `write(id, bytes)`, `close-stdin(id)`, `kill(id)`. `processes` permission. |
| `terminal` | `open(options) -> id`, `send-text(id, text)`, `show(id, focus)`, `close(id)`. `terminal` permission. |
| `review` | `propose(proposal) -> id`, `withdraw(id)`. `project` permission. |
| `git` | `repositories()`, `status()`, `diff(path)`, `branches(root)`, `set-commit-template(template?)` — `project` permission; `new-branch(root, name, start?) -> id`, `checkout(root, branch) -> id` — `project = "write"`. |
| `diagnostics` | `get(path?)` (`project` permission), `publish(path, diagnostics)`, `clear()`. |
| `secrets` | `get(key)`, `set(key, value?)`. |
| `timers` | `after(ms) -> id`, `every(ms) -> id`, `cancel(id)`. |
| `system` | `open-url(url)`, `copy-text(text)`, `home-dir()`, `utc-offset(at)`, `time-zone()`, `open-settings()`. |
| `picker` | `open(spec) -> id`, `update(id, items, loading)`, `close(id)` ([Pickers](#pickers)). |
| `links` | `set-patterns(patterns)`, `describe(request, markdown?)` ([Links](#links)). |
| `claude` | `new-session(spec)`. `claude` permission ([Claude sessions](#claude-sessions)). |
| `contexts` | `switch(save?, load?)`, `remove(key)`. `project` permission ([Contexts](#contexts)). |

### Commands and their context

`run_command` gets the command's id and a `CommandContext`: where it was run from, and what it acts
on, as it was at that moment.

| Field | |
|-------|-|
| `source` | `palette`, `keys`, `editor-menu`, `tree-menu`, `tab-menu`, `notification`, `status-bar`, `settings` — a button of the settings page (`source.name()` gives these names). |
| `editor` | The document: the active one (the editor's menu, the palette, keys), or the tab's (a tab's menu); none when it isn't a document. |
| `selections` | The document's selections, the primary first; an empty range is a cursor. |
| `paths` | The files and folders, absolute: the rows selected in the tree, the tab's file, the document's file. |
| `argument` | What a notification's action or a settings button carries (`context.argument()`): which task, which meeting of several. |

```rust
fn run_command(&mut self, command: &str, context: &CommandContext) {
    match command {
        // [[menus]] command = "create-issue", location = "editor", when = "selection"
        "create-issue" => {
            let summary = context.selected_text().unwrap_or_default();
            let file = context.path().unwrap_or("");
            self.create_issue(&summary, file);
        }
        // [[menus]] command = "lint-folder", location = "tree", when = "folder"
        "lint-folder" if context.from_menu() => self.lint(&context.paths),
        _ => {}
    }
}
```

### The network

`http` — requests to the hosts of the `network` permission. Flux makes the connection (HTTPS with the
system's certificates, so a company's own certificate authority works), follows redirects (up to 10,
each checked against the permission; `no_redirects()` returns a redirect as the response) and keeps a
line about every request in the plugin's log. Any method and headers go: REST, GraphQL, CalDAV's
`PROPFIND` and `REPORT`. A request gives up after 30 seconds unless told otherwise.

```rust
use flux_plugin_api::http;

let response = http::get("https://api.example.com/v1/issues?assignee=me")
    .bearer(&token)
    .header("Accept", "application/json")
    .fetch()
    .map_err(|err| err.message())?;
if response.is_success() {
    let issues: Vec<Issue> = response.json()?;
}

let created = http::post("https://api.example.com/v1/issues")
    .bearer(&token)
    .json(&serde_json::json!({ "summary": summary }))
    .fetch();
```

`fetch` waits for the whole response (64 MB of body at most). `start` returns at once, and the
response comes as events: `http-response` (the status and the headers), `http-body` (chunks of the
body as they arrive), then `http-done` — for server-sent events, a long download, or not to keep the
user waiting. `http::EventStream` turns the chunks of a `text/event-stream` into events:

```rust
// Starting.
self.stream = Some(http::post(url).json(&body).header("Accept", "text/event-stream").start());

// In on_event.
Event::HttpBody((id, chunk)) if Some(id) == self.stream => {
    for event in self.events.feed(&chunk) {
        self.append(&event.data);
    }
}
Event::HttpDone((id, result)) if Some(id) == self.stream => {
    self.stream = None;
    if let Err(err) = result {
        notify::error(&tr("The request failed"), Some(&err.message()));
    }
}
```

The errors (`HttpError`): `denied` — the manifest doesn't allow the host; `invalid` — not a valid URL
or request; `connection` — no connection (the name, a refusal, TLS); `timed-out`; `cancelled`;
`too-large`. `err.message()` puts one in words for the user. `http::with_query(url, &[(name,
value)])` builds a URL with an encoded query; `.form(&[…])` sends a form's body.

### A local server

`server` — the plugin's own server on `127.0.0.1`, for a web service's sign-in that redirects back to
the IDE ([Signing in to a web service](#signing-in-to-a-web-service)) and for programs that talk to
the IDE over HTTP or WebSocket (the IDE protocols of agents and tools). `listen()` takes a free port
(or the one asked for) and returns it; requests come as `server-request` events, and the plugin
answers each by its id:

```rust
let port = server::listen()?;

// In on_event.
Event::ServerRequest(request) => match request.route() {
    "/" => server::respond_html(request.id, 200, "<h1>Hello</h1>"),
    "/api/state" => server::respond_json(request.id, 200, &self.state),
    "/events" => {
        server::start_events(request.id); // then send_event(id, Some("update"), data), end_events(id)
        self.listeners.push(request.id);
    }
    _ => server::not_found(request.id),
},
Event::StreamClosed(request) => self.listeners.retain(|id| *id != request),
Event::WsOpened(open) => {
    if open.header("Authorization") != Some(self.token.as_str()) {
        server::ws_close(open.connection);
    }
}
Event::WsMessage((connection, WsMessage::Text(text))) => {
    let _ = server::ws_send_text(connection, &self.answer(&text));
}
```

- The server listens on `127.0.0.1` only: nothing outside the Mac reaches it.
- Flux answers **403** itself to a request whose `Host` isn't the server's own (`127.0.0.1:<port>`,
  `localhost:<port>`): a web page can't reach the server through a name of its own (DNS rebinding).
  Pages and programs on the Mac still can — guard what matters with a token of your own, as the
  WebSocket example does.
- A request the plugin doesn't answer within 60 seconds gets **504**.
- A streamed answer (`respond-stream`, `send-chunk`, `end-stream`) is chunked: server-sent events, a
  long poll. `stream-closed` says the client went away.
- A WebSocket upgrade is accepted, and `ws-opened` tells the plugin with the path and the headers;
  `ws-close` drops one it doesn't want.
- One server per plugin: `listen` again restarts it; it stops when the plugin stops.

### Programs

`process` — the programs of the `processes` permission. They run with the user's rights and the
environment of the user's login shell (its `PATH`, so tools installed with Homebrew are found even
when Flux was started from the Dock), in the project root unless told otherwise.

```rust
use flux_plugin_api::process;

let status = process::run("git", &["status", "--porcelain"])?; // waits; 60 s at most
if status.success() {
    let changes = status.stdout_text();
}

let build = process::command("cargo")
    .args(["build", "--message-format=json"])
    .env("CARGO_TERM_COLOR", "never")
    .spawn()?;
// Event::ProcessOutput(output) — output.process == build, output.channel (stdout, stderr), output.bytes
// Event::ProcessExited((build, code)) — code: none when ended by a signal
```

`run` returns the exit code and the output whatever the code; an error is a program that didn't
start or didn't end in time (it is killed). `write`, `close-stdin` and `kill` talk to a started
program. The plugin's programs end when it stops.

### Terminals

`terminal` — terminal tabs with a command or the user's shell, when the user should see the work:
tests, a dev server, a command-line agent. The tab is the user's like any other.

```rust
use flux_plugin_api::terminal;

let tests = terminal::command(["cargo", "test"]).title("Tests").env("RUST_BACKTRACE", "1").open()?;
// Event::TerminalExited((tests, code)) — the command ended; its tab stays, with the exit code.

let shell = terminal::shell().in_editor().focus().open()?;
terminal::run_line(shell, "git status")?; // types the line and presses Enter
// Event::TerminalClosed(shell) — the user closed the tab.
```

Tabs open in the terminal panel at the bottom, or among the editor's tabs (`in_editor`); `focus`
gives them the keyboard (by default it stays where the user is). A plugin reaches only the terminals
it opened.

### Proposed edits

`review` — the plugin proposes a file's new text, and Flux shows a diff tab, as it does for Claude's
edits: the file as it is on the left, the proposal on the right — editable, hunks rejected one by one
— and Accept (⌘↵) and Reject on a banner. The answer comes as `proposal-answered`:

```rust
use flux_plugin_api::review::{self, ProposalOutcome};

let fixed = format(&text);
self.proposal = Some(review::proposal(&path, &fixed).title("Format").apply().send()?);

// In on_event.
Event::ProposalAnswered((id, outcome)) if Some(id) == self.proposal => match outcome {
    ProposalOutcome::Accepted(text) => {} // the text as the user left it; written already (apply)
    ProposalOutcome::Rejected => {}
    ProposalOutcome::Closed => {}         // the tab was closed without an answer, or withdrawn
},
```

With `apply`, Flux writes the accepted text itself — into the open document as one undo step, saved,
or into the file (a new file is created); without it only the plugin hears the answer and does what
it wants with the text (an agent that writes its files itself). A proposal may be for a file that
doesn't exist yet. `review::withdraw(id)` closes the tab without an answer. It needs the `project`
permission: the answer carries the file's text.

### Git

`git` — the project's repositories, with the `project` permission: `repositories()` (the root, the
branch — none when HEAD is detached, the HEAD commit, an operation in progress: merge, rebase…),
`status()` (the changes of the working copy against HEAD: added, modified, deleted, renamed,
untracked, conflicted), `diff(path)` (a file's changes against HEAD as a unified diff),
`branches(root)` (the local branches, then the remote ones — `origin/main`; the current one marked).
A change of any of it comes as `git-changed`.

With `project = "write"` a plugin changes them the way Flux does — the Branches popup's way:
`new_branch(root, name, start)` creates a branch from `start` (a branch, a tag, a commit; none —
HEAD) and checks it out; `checkout(root, branch)` checks one out (a remote one — as a new local
branch tracking it). Local changes come along, Smart Checkout asks when they'd be in the way, and the
messages are Flux's own; how it ended comes as `git-done`: done, or why not — the user declined, git
failed (Flux has told the user).

While a task of the plugin's is active, `commit_template(text, prefix)` fills the commit window's
empty message — and replaces the previous template's text the user didn't change; a message
generated with Claude starts with `prefix`. `clear_commit_template()` — the task is over.

```rust
use flux_plugin_api::git;

// A task tracker's plugin: the issue of the current branch, "feature/FLUX-12-search".
let branch = git::repository_of(&path).and_then(|repo| repo.branch);

// Starting a task: its branch (or back to it), its key in the commit message.
let repo = git::repository_of(&root).ok_or("no repository")?;
let exists = git::branches(&repo.root)?.iter().any(|branch| branch.name == name);
self.branching = Some(match exists {
    true => git::checkout(&repo.root, &name),
    false => git::new_branch(&repo.root, &name, None),
});
git::commit_template(&format!("{key} {summary}"), Some(&key));
// Event::GitDone((id, result)) if Some(id) == self.branching → on with the task, or stop
```

### Problems

`diagnostics` — problems in files. `get(path)` reads what Flux knows — the language servers', other
plugins' (the `project` permission); `publish(path, problems)` shows the plugin's own as Flux shows
the servers': underlined, on hover, on F2, in the status bar's counters. A publish replaces what the
plugin published for that file before; an empty list or `clear()` takes them away. Positions are in
the file's text as it is now (the open document's). `diagnostics-changed` says which files' problems
changed.

```rust
use flux_plugin_api::diagnostics::{self, Diagnostic};
use flux_plugin_api::{Range, process};

// A linter: shellcheck on save.
let output = process::run("shellcheck", &["--format=json1", &path])?;
let problems: Vec<Diagnostic> = parse(&output.stdout_text())
    .map(|item| {
        Diagnostic::warning(Range::on_line(item.line - 1, item.column - 1, item.end_column - 1), &item.message)
            .code(&format!("SC{}", item.code))
    })
    .collect();
diagnostics::publish(&path, &problems); // source: the plugin's name unless `.source(…)`
```

### Forms

`dialogs.ask-form` — a dialog with several fields, such as a tracker's Open Task: a line of text,
several lines (↵ — a new line, ⌘↵ — confirm), a dropdown, checkboxes, notes between them; the
placeholder, the code font. A field can be enabled by a checkbox (`enabled_by`): the branch's name
only while «Create a branch» is on. The answer comes as `form-answered`: every field's value by its
key — the text, the chosen option's value, `"true"` / `"false"` — or none, cancelled.

```rust
use flux_plugin_api::dialog::{Form, Values};

let transitions = [("start", tr("Start Progress")), ("review", tr("Review"))];
let options: Vec<(&str, &str)> =
    transitions.iter().map(|(id, title)| (*id, title.as_str())).collect();
self.form = Some(
    Form::new(&tr("Open Task"), &tr("Start"))
        .message(&format!("{key} · {summary}"))
        .choice("status", &tr("Move to"), &options, Some("start"))
        .toggle("branch", &tr("Create a branch"), true)
        .text("branch-name", &tr("Branch"), &name).code().enabled_by("branch")
        .multiline("comment", &tr("Comment"), "").placeholder(&tr("Optional"))
        .send(),
);

// In on_event.
Event::FormAnswered((id, answer)) if Some(id) == self.form => match answer.map(Values::from) {
    Some(values) if values.on("branch") && values.text("branch-name").is_empty() => {
        self.ask_again(&tr("Name the branch, or turn Create a branch off."), &values)
    }
    Some(values) => self.start(values.get("status"), &values.text("branch-name")),
    None => {} // cancelled
},
```

`Form::error` shows what was wrong with the previous answer above the buttons: ask again with the
values the user gave — the form keeps only what the plugin passes back. A form waits while a dialog
of the user's is open, as questions do.

### Pickers

`picker` — a popup list drawn as Flux's own (Open Task, the branches): a search field over items —
an icon, a label of spans, a dim detail, a badge on the right, a group's heading — or a short menu
without the field (`Picker::menu`). Flux filters the items by the query (fuzzily, as ⌘P); a `live`
picker searches itself: every change of the query comes as `picker-query` (a burst of typing — one
event), and the plugin answers with `picker::update` (a spinner while `loading`). The choice comes as
`picker-chosen`: the item's key, or none — dismissed (Esc, a click outside). A picker gets exactly one
answer; a plugin's new picker closes its previous one.

```rust
use flux_plugin_api::picker::{self, Picker, item};

// Open Task: the user's tasks at once, then the tracker's search as they type.
self.search = Some(
    Picker::new(&tr("Find a task by its key or words"))
        .live()
        .items(self.mine.iter().map(|task| {
            item(&task.key, &task.summary).detail(&task.key).group(&task.status)
        }))
        .open(),
);
// Event::PickerQuery((id, query)) → picker::update(id, self.find(&query), false)
// Event::PickerChosen((id, Some(key))) → open the task

// A status's transitions: a menu under the badge of the tool window.
self.transitions = Some(
    Picker::menu()
        .items(transitions.iter().map(|transition| item(&transition.id, &transition.title)))
        .under("tracker", "status")
        .open(),
);
```

Where it opens: under the title bar, as the command palette (the default); above one of the plugin's
status bar items (`at_status_item`) — the day's meetings from a «Meeting in 12 min» item; under an
element of its tool window (`under`). An item or an element
that isn't on screen — under the title bar.

### The settings page

Settings → the plugin is a form of its `[[settings]]` ([The manifest](#the-manifest)). Beyond the
values the user types, the plugin writes to the page:

- `settings::set(key, &value)` — a value, checked against the setting's kind (the error says why it
  doesn't fit); `settings::reset(key)` — the default. An open page shows it at once; the plugin
  doesn't get `settings-changed` about its own write.
- `settings::status(key, text, Tone::Success)` — a `status` line: «Signed in as anna@example.com»;
  `settings::busy(key, text)` — with a spinner: «Checking…»; `settings::clear_status(key)` — the
  manifest's text again. The lines are kept while the plugin runs.
- `system::open_settings()` — Settings on the plugin's page: a «Sign in» or «Set the organization»
  link of its tool window.

A `secret` setting is the plugin's secret named by the setting's key: the user types a token there,
and the plugin reads it with `secrets::get(key)` when `settings-changed` comes. A `button` runs its
command with the button's `argument`, from the `settings` source:

```rust
use flux_plugin_api::settings::{self, Tone};

fn run_command(&mut self, command: &str, context: &CommandContext) {
    match command {
        // [[settings]] type = "button", command = "check-connection"
        "check-connection" => {
            settings::busy("connection", &tr("Checking…"));
            match self.who_am_i() {
                Ok(login) => {
                    let text = trf("Signed in as {0}", &[&login]);
                    settings::status("connection", &text, Tone::Success)
                }
                Err(err) => settings::status("connection", &err, Tone::Error),
            }
        }
        _ => {}
    }
}
```

A `records` setting reads as a list of objects: give the struct's fields `#[serde(default)]`, so a
record saved before a field was added still reads.

```rust
#[derive(serde::Deserialize)]
struct Action {
    #[serde(default)]
    name: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    mode: String,
}

let actions: Vec<Action> = setting("actions").unwrap_or_default();
```

### Links

`links` — the plugin's patterns of links on text that names something of its own — a task's key,
`TRACK-142` — wherever the text is: in the editor (hold ⌘ over it: underlined, a pointing hand;
⌘-click), in commit messages (the Git log, a commit's details), in Claude's chat, in the plugins'
Markdown and tooltips. A click comes as `link-activated` — the pattern's id and the text. A pattern
with `tooltip` asks the plugin when the mouse rests over a link (or F1 at the caret):
`link-hovered` with a request, which the plugin answers with Markdown — within 3 seconds, or there
is no tooltip.

```rust
use flux_plugin_api::links;

// The keys of the user's queues — \b(?:TRACK|WEB)-\d+\b — not any CAPS-123: UTF-8 stays text.
links::set([links::pattern("task", &links::key_regex(&["TRACK", "WEB"])).tooltip()])?;

// In on_event.
Event::LinkHovered((request, _, key)) => match self.cached(&key) {
    Some(task) => {
        links::tooltip(request, &format!("**{key}** {}\n\n{}", task.summary, task.status))
    }
    None => links::no_tooltip(request),
},
Event::LinkActivated((_, key)) => self.open_card(&key),
```

A pattern is a Rust `regex`, and the whole match is the link; one that doesn't compile is refused
with an error naming it. `set` replaces the plugin's patterns; an empty list (`links::clear()`)
removes them, and they go when the plugin stops.

### Claude sessions

`claude` — a Claude Code session in Flux's chat, opened by the plugin with its context: a task to
discuss, or to work on. The manifest asks for `claude = true`: a session's requests run with the
rights the user gave Claude Code, and the install question says so.

```rust
use flux_plugin_api::claude;

// Discuss: the task attached, the field takes the keyboard — the user writes the request.
claude::session()
    .title(&format!("{key}: {summary}"))
    .attach(&key, &task_markdown)
    .start()?;

// Plan: plan mode, the request sent at once.
claude::session()
    .title(&format!("{key}: {summary}"))
    .attach(&key, &task_markdown)
    .prompt(&tr("Study the task and the code it concerns, then propose a plan."))
    .send()
    .plan_mode()
    .start()?;
```

An attachment is a chip in the field — its label — and its Markdown goes with the first message.
Without `send` the prompt waits in the field. `start` fails when Claude Code is turned off, not
installed or not signed in: the error says which, and the Claude window shows how to fix it.

### Contexts

`contexts` — the window's tabs kept under a key, one set per task: a task's
documents (with the caret and the scroll), pictures and Claude chats come back when the user
returns to the task. `switch(save, load)` saves the open tabs as `save` — a key and a title, which
the user sees in **Load Context…** — closes them (documents with unsaved changes and running Claude
sessions stay) and opens the tabs of `load`, if it was saved. The keys are the plugin's own: other
plugins' and the user's contexts are apart. It needs the `project` permission.

```rust
use flux_plugin_api::contexts;

// Starting TRACK-142 after TRACK-140: the old task's tabs are kept, the new one's come back.
contexts::switch(Some(("TRACK-140", "TRACK-140 Fix the header")), Some("TRACK-142"))?;
// The task is closed: its tabs are no longer needed.
contexts::remove("TRACK-142");
```

The user has the same without plugins: **Save Context…**, **Load Context…** and **Clear Context**
(⌥⇧X) in the command palette.

### Secrets, timers and the system

`secrets` keeps tokens and passwords apart from the plugin's files: Flux keeps them encrypted in a
file only the user can read (`~/Library/Application Support/flux/secrets.bin`); only this plugin
reads them, and they go when it is uninstalled. Keep a sign-in's tokens there, not in `storage`.

```rust
secrets::set("token", &token)?;
let token = secrets::get("token");
secrets::remove("token")?;
```

`timers` send `timer` events after a delay or periodically, while the plugin runs — to poll a
service, to remind of a meeting. A periodic tick that still waits in the plugin's queue isn't added
again, so a busy plugin doesn't fall behind.

```rust
self.poll = Some(timers::every(Duration::from_secs(60)));
// Event::Timer(id) if Some(id) == self.poll => self.refresh(),
```

`system` — `open_url(url)` opens an `http`, `https` or `mailto` link in the default browser;
`copy_text(text)` puts text on the clipboard; `home_dir()` (and `system::expand_home("~/.config/gh")`)
— where the folders of the `folders` permission are; `open_settings()` — Settings on the plugin's
page. The clock in the user's time zone: `system::local_time(at)` — a moment (seconds since 1970,
UTC; `system::now()`) with `.hm()` («11:00»), `.date()`, `.iso()` («2026-10-14T10:00:00.000+0300» —
as Yandex Tracker takes a work log's start); `utc_offset(at)` (summer time included) and
`time_zone()` («Europe/Moscow») underneath.

### Background work and the time limit

A call that waits — `fetch`, `run`, `project.search`, a document's text, Git, a terminal — waits on
the plugin's own thread; Flux doesn't wait for it, and the waiting doesn't count toward the 10
seconds. But while it waits, the plugin answers nothing else: its tool window doesn't react, its
commands queue up. Long work goes to the background — `http.start`, `process.spawn`, the server, a
terminal, a proposal, a timer, a branch or a checkout — and reports as events into the plugin's
queue, between its other calls; so do the user's answers to questions, forms and pickers.
Everything in the background is the plugin's: its requests are cancelled, its programs end, its
server and timers stop when the plugin stops (turned off, reloaded, removed, the window closing; a
plugin with the `project` permission is also restarted when the window's project changes).

### Events

`on_event` gets them.

| Event | |
|-------|-|
| `active-editor-changed(editor?)` | The active tab changed: its document, or none (a terminal, a log). |
| `editor-opened(editor)`, `editor-closed(id)` | |
| `editor-changed(id)` | The text changed: typing, undo, a reload from disk. A burst is one event. |
| `selection-changed(id)` | The selections or the cursor moved. A burst is one event. |
| `editor-saved(editor)` | The document was written to disk. |
| `project-changed(root?)` | The window's project changed. A plugin with the `project` permission is deactivated and activated again in a new sandbox for the new folder, its state afresh. |
| `settings-changed` | The user changed the plugin's settings. |
| `tool-window-shown(id)`, `tool-window-hidden(id)` | |
| `dialog-answered((id, button?))` | The index of the pressed button; none — dismissed. |
| `text-answered((id, text?))` | The text; none — cancelled. |
| `ui(input)` | The user did something in a tool window: `window`, `element` and the `ui-event`. |
| `timer(id)` | A timer fired. |
| `http-response(head)`, `http-body((id, bytes))`, `http-done((id, result))` | A started request: its status and headers, chunks of its body, its end. |
| `server-request(request)` | A request to the plugin's server: `id`, `method`, `path` (with the query), `headers`, `body`. |
| `stream-closed(id)` | The client of a streamed answer went away. |
| `ws-opened(open)`, `ws-message((connection, message))`, `ws-closed(connection)` | WebSocket connections to the server and their text or binary messages. |
| `process-output(output)`, `process-exited((id, code?))` | A started program's output (stdout or stderr) and its end. |
| `terminal-exited((id, code?))`, `terminal-closed(id)` | A terminal's command ended (the tab stays); its tab was closed. |
| `proposal-answered((id, outcome))` | `accepted(text)`, `rejected`, `closed`. |
| `git-changed` | The status, a branch or a HEAD of a repository changed. |
| `diagnostics-changed(paths)` | The problems of these files changed. |
| `form-answered((id, values?))` | The answer to `ask-form`: the values by the fields' keys; none — cancelled. |
| `picker-query((id, query))` | The query of a `live` picker changed (a burst of typing — one event). |
| `picker-chosen((id, key?))` | A picker closed: the chosen item's key; none — dismissed. |
| `link-activated((pattern, text))` | One of the plugin's links was clicked. |
| `link-hovered((request, pattern, text))` | A link of a pattern with `tooltip` wants its tooltip: answer with `links.describe`. |
| `git-done((id, result))` | `new-branch` or `checkout` ended: done, or why not. |

## Signing in to a web service

Most services sign a user in with OAuth 2.0: the user approves the plugin on the service's page, and
the plugin gets a token. With the API that is a few steps — the manifest asks for the network to the
service and a server:

```toml
[permissions]
network = ["oauth.example.com", "api.example.com"]
server = true
```

1. **Listen** for the redirect: `let port = server::listen()?;` — the redirect URI is
   `server::url(port, "/callback")` (register `http://127.0.0.1` as a redirect URI with the service;
   most accept any port on the loopback address, as RFC 8252 asks of them).
2. **Open the service's page** in the browser with a random `state` (and PKCE's `code_challenge`
   where the service supports it — `getrandom` and `sha2` work on `wasm32-wasip2`):

   ```rust
   let url = http::with_query("https://oauth.example.com/authorize", &[
       ("response_type", "code"),
       ("client_id", CLIENT_ID),
       ("redirect_uri", &server::url(port, "/callback")),
       ("state", &self.state),
   ]);
   system::open_url(&url)?;
   ```
3. **Take the code** when the browser comes back: check the `state`, thank the user, stop the server.

   ```rust
   Event::ServerRequest(request) if request.route() == "/callback" => {
       if request.query("state").as_deref() != Some(self.state.as_str()) {
           return server::respond_text(request.id, 400, "Unexpected sign-in");
       }
       server::respond_html(request.id, 200, "<p>Signed in. You can close this tab.</p>");
       server::stop();
       self.exchange(request.query("code"));
   }
   ```
4. **Exchange the code** for tokens and keep them in `secrets`:

   ```rust
   let response = http::post("https://oauth.example.com/token")
       .form(&[
           ("grant_type", "authorization_code"),
           ("code", &code),
           ("client_id", CLIENT_ID),
           ("redirect_uri", &redirect_uri),
       ])
       .fetch()
       .map_err(|err| err.message())?;
   let tokens: Tokens = response.json()?;
   secrets::set("access-token", &tokens.access_token)?;
   secrets::set("refresh-token", &tokens.refresh_token)?;
   ```
5. **Call the API** with the token (`.bearer(&token)`, or the header the service wants); on `401`,
   refresh it with the refresh token, or sign in again.

A service with the **device flow** needs no server: the plugin asks the service for a code, shows it
to the user (`dialog::Ask` with the code in its message, `system::copy_text` for convenience), opens
the verification page with `system::open_url`, and polls the token endpoint with
`timers::every(interval)` until the user approves.

## A task tracker plugin

The API is made to put a task tracker into Flux, with each task keeping its own tabs; the
catalog's [`flux.yandex`](https://github.com/egor4geev/flux-plugins/tree/main/plugins/flux.yandex) —
Yandex Tracker and Calendar — is built of these pieces:

1. **Signing in** — [Signing in to a web service](#signing-in-to-a-web-service); the settings page is
   the account: a pasted token — a `secret` setting, Sign In and Sign Out — `button` settings, who is
   signed in — a `status` setting.
2. **Finding a task** — a `live` [picker](#pickers) under the title bar (⌥⇧N): the user's tasks at
   once, then the tracker's search as they type.
3. **Starting it** — a [form](#forms) (the status to move to, a branch, the task's tabs), or none at
   all for a one-click action; then the tracker's transition, `git::new_branch` or `git::checkout`
   (`git-done`), `contexts::switch` (the previous task's tabs kept, this one's back),
   `git::commit_template` (the key in the commit message), a status bar item with a timer whose
   click opens a menu `at_status_item`.
4. **The task** — a tool window: a `menu_badge` status with its transitions under it, the description
   and comments as Markdown, a `multiline_field` for a new comment; the board — a window among the
   editor's tabs (`location = "editor"`).
5. **Keys in the text** — [links](#links) of the queues' keys: the tooltip from the plugin's cache, a
   click opens the task.
6. **Claude** — a [session](#claude-sessions) with the task attached: to discuss it, or a prompt of
   the plugin's settings filled from the task — a `records` setting of actions, each a name, a
   prompt and a mode.
7. **News** — polling with `timers::every`; notifications whose actions carry the task's key
   (`Notice::action_with`, `context.argument()`).

## From 0.2 to 0.3

- Plugins built for 0.2 keep working as they are: Flux answers their calls through the 0.2 interfaces
  (`wit/v0.2`) and doesn't send them what 0.2 doesn't know — the new events; a settings button's
  command comes to them as from the palette, without its argument. A plugin without code — a
  language, a theme — may name either version.
- A plugin rebuilt with the new SDK is a 0.3 component: its manifest must say `api = "0.3"`. Flux
  checks the component against the manifest and refuses a mismatch with a message that names both
  versions.
- New: [forms](#forms), [pickers](#pickers), [the settings page](#the-settings-page)'s kinds (`text`,
  `records`, `secret`, `button`, `status`, `section`) with `settings.set` and `set-status`, Git's
  `branches`, `new-branch`, `checkout` and `set-commit-template`, [links](#links),
  [Claude sessions](#claude-sessions) with the `claude` permission, [contexts](#contexts),
  `system.utc-offset`, `time-zone` and `open-settings`, `ui.set-title`, windows among the editor's
  tabs (`location = "editor"`), the badge and the multi-line field, `palette = false`, the
  `argument` of notification actions and settings buttons.
- Code that builds `CommandContext`, `notifications::Action` or `TextFieldSpec` itself gets the new
  fields (`argument`, `multiline`); a `match` over `CommandSource`, `ElementKind` or `Event` gets the
  new cases.

## From 0.1 to 0.2

- Manifests say `api = "0.2"`. Flux doesn't run 0.1: a 0.1 plugin is listed as built for another
  version until it is rebuilt.
- `run_command` gets the command's context: `fn run_command(&mut self, command: &str, context:
  &CommandContext)` (`run-command(command, context)` in the WIT).
- `editors.close`, `[[menus]]`, permissions beyond `project` and the interfaces `http`, `server`,
  `process`, `terminal`, `review`, `git`, `diagnostics`, `secrets`, `timers`, `system` are new;
  everything else is as it was.

## Tool windows

A tool window's content is a **view**: a tree of elements, which the SDK builds and flattens.

```rust
use flux_plugin_api::view::*;

let mut tree = Tree::new().empty_text(&tr("No items"));
let file = tree.add(RowSpec::new("src/main.rs", "main.rs").icon("file:main.rs").badge("2"));
tree.add_child(file, RowSpec::new("src/main.rs:12", "TODO: handle errors").detail("line 12"));
set_view(
    "todo",
    column("root", [
        toolbar("toolbar", [icon_button("refresh", "refresh", &tr("Refresh"))]),
        tree.into_element("items"),
    ]),
);
```

| Element | | Events (`ui-event`) |
|---------|-|---------------------|
| `column`, `row` | Children top to bottom, or left to right. | |
| `toolbar` | Buttons in a row under the window's title, as in Flux's own tool windows. | |
| `text` | Spans of text, each with a tone (`normal`, `muted`, `dim`, `accent`, `success`, `warning`, `error`), bold, code, highlighted. `label` is plain text. | |
| `markdown` | Markdown, drawn as Flux draws documentation. | |
| `button` | A label and/or an icon, a tooltip, primary, enabled. `icon_button` — the toolbar's kind. | `clicked` |
| `text_field` | A text field. | `changed(text)`, `submitted(text)` on ↵ |
| `switch`, `checkbox` | A toggle with a label. | `toggled(on)` |
| `tree` | Rows with a key, a parent, an icon, a label of spans, a dim detail, a badge; flat rows make a list. Takes the remaining height, scrolls, draws only the visible rows. API 0.3: a row's `subtitle` (spans under the label), and `cards` — each row a card: the icon, the label and the badge, the subtitle in up to two lines, the detail (`Tree::cards()`, `RowSpec::subtitle`; a board's column is a tree of cards — a row of columns of cards scrolls sideways when narrow). | `selected(key)`; `activated(key)` — ↵, a double click, a click on a row without children; `expanded((key, expanded))`; `row-action((key, id))` — a row's button (0.3) |
| `divider`, `spacer`, `progress` | A line; the free space; a progress bar (none — indeterminate). | |
| `badge` | A short colored label in a tone: a status, a priority, a count. `menu_badge` — with ▾: open a [menu](#pickers) under it. | `clicked` (`menu_badge`) |
| `multiline_field` | A field of several lines that grows with the text: ↵ — a new line, ⌘↵ — `submitted`. A comment, a description. | `changed(text)`, `submitted(text)` on ⌘↵ |
| `properties` | API 0.3: labels and their values in two aligned columns — a task's fields. The labels' column is as wide as the longest label (at most 40% of the list; a longer one truncates), a value takes one line (a longer one ends with "…" — long text belongs in Markdown), an empty one is a dim "—". An `editable_property` has a ▾ and its whole row answers the mouse. | `activated(id)` — a click on an editable property |

A tree's row can carry **tags** and **buttons** (API 0.3): `RowSpec::tag(text, tone)` — small colored
labels (a tag, a deadline, story points), on a card in a line under the subtitle (the ones that
don't fit are counted as "+N"), in a list after the label; `RowSpec::action(id, label, icon,
primary)` — on a card at its foot on the right (a board's "Take to Work"), in a list at the row's
end while it is hovered or selected. A press comes as `row-action((row key, action id))` with the
tree's element id, and neither selects nor opens the row; ⌘↵ on the selected row presses its first
primary button. A tree of cards where some row has tags or buttons leaves room for them on every
card (one card height per tree).

**Properties** (API 0.3) are a task's fields that the user changes in place: a click on an editable
one comes as `activated(id)` with the list's element id, and the plugin opens a picker right under
that row — the anchor `element((window, "<list>/<id>"))` (the list itself until the row is drawn) —
or a form:

```rust
let fields = properties("fields", vec![
    editable_property("priority", &tr("Priority"), vec![span("Critical").tone(Tone::Error)]),
    editable_property("assignee", &tr("Assignee"), vec![span(&assignee)]),
    property("created", &tr("Created"), vec![span(&created).tone(Tone::Muted)]),
]);
// on_event: Event::Ui(UiInput { element, event: UiEvent::Activated(id), .. }) if element == "fields"
Picker::menu().items(priorities).under(WINDOW, &format!("fields/{id}")).open();
```

A view without a tree scrolls as a whole; a row of buttons that doesn't fit wraps onto the next line
(the toolbar doesn't: keep its buttons few).

**Ids keep state.** Flux keeps an element's state between views by its id: a field's text, the
scroll, the expanded and selected rows. A row's `expanded` applies when the row first shows; after
that the user's choice wins. To start an element afresh — say, **Expand All** applying `expanded`
again — give it a new id (`"items-2"`).

**A window among the editor's tabs.** `location = "editor"` in `[[tool-windows]]` makes a window a
tab of the editor — for something wide: a board of tasks in columns, a preview. It has no launchpad
icon: `ui.show` opens the tab (or brings it forward), `ui.hide` closes it, and so can the user; the
palette and the window's keys open it too, and `tool-window-shown` / `tool-window-hidden` say when it
is on screen. Its content is a view like any tool window's.

**Titles.** `ui.set-title(window, title)` renames a window while the plugin runs — its header in the
island, its tab in the editor: «Board: Backend». An empty title brings back the manifest's.

**Icons** are named:

- a built-in icon by the file name in [`crates/flux-app/assets/icons`](../crates/flux-app/assets/icons)
  without `.svg`: `refresh`, `expand-all`, `collapse-all`, `search`, `settings`, `plus`, `minus`,
  `trash`, `pencil`, `info`, `warning`, `error`, `check`, `clock`, `history`, `terminal`, `branch`,
  `commit`, `star`, `tag`, `folder`, `file`, `globe`, `plug`, `puzzle`…;
- the icon of a file type by a file name: `"file:main.rs"`, `"file:Cargo.toml"`;
- an SVG of the plugin's folder: `"icons/todo.svg"` — monochrome, 16×16, stroked like Flux's own (1.5,
  round caps and joins): Flux draws it in the color of the place.

## Localization

Strings are written in English; translations live in `locales/<language>.toml` in the plugin's
folder, English text = translation, as Flux's own tables:

```toml
"Say Hello" = "Поздороваться"
"Documents: {0}" = "Документов: {0}"
```

Flux translates the manifest's strings with them (the name, command titles and categories — and so
the menu items — tool window titles, settings); the plugin translates its own with `tr` and `trf`
(`{0}`, `{1}`… filled in). There are no plural forms: prefer «Files: 4» to «4 files». Translations
are often longer than English: check that a row of buttons still fits.

## Plugins under development

**Install Plugin from Disk…** with a folder links it as a plugin under development: it stays where it
is, and an id it shares with an installed or bundled plugin overrides that one (that's how a bundled
plugin is developed). If the folder has a `Cargo.toml`, Flux builds it
(`cargo build --release --target wasm32-wasip2`, the output in the plugin's log); then it reloads the
plugin each time its component changes — after your own `cargo build` too. The manifest's `wasm`
names the component where cargo puts it: `target/wasm32-wasip2/release/<crate>.wasm` (dashes in the
crate's name become underscores). Its permissions are asked about when the folder is installed, as
an archive's are, and enforced the same way.

The log keeps the plugin's `log` lines, what it prints — stdout as information, stderr (`eprintln!`,
a panic's message) as warnings — and what happened to it: started, stopped and why, built, its
requests. It is also a file: `~/Library/Logs/Flux/plugins/<id>.log`.

**Against a fake service.** Two variables of Flux's environment — Flux's, not the plugin's: a plugin
can't set them — let a plugin's author check it without a real account, by hand or in Flux's UI
scenarios (`scripts/ui-scenario.sh`):

- `FLUX_PLUGIN_HTTP_OVERRIDE` — comma-separated `<prefix>=<replacement>` pairs. A request whose URL
  starts with a prefix goes to the replacement with the rest of the URL:
  `https://api.tracker.yandex.net=http://127.0.0.1:8765` sends the plugin's requests to a fake server
  on the Mac. The permission is checked against the URL the plugin asked for, and so is every
  redirect; the plugin's log shows the request with `(override: …)`.
- `FLUX_SCENARIO_OPEN_URL=<file>` — in a build with the `scenario` feature, `system.open-url` and
  the links of Markdown write the address into the file instead of opening the browser: a scenario
  reads it — an OAuth sign-in's page — and plays the browser (fetch the page, follow its redirect to
  the plugin's server).

The catalog's `flux.yandex` comes with such a fake service: `mock/mock_yandex.py`.

## Packaging

An archive — `.zip`, `.tar.gz` or `.tgz` — with the plugin's folder: the manifest, the component at
the manifest's `wasm` path, `locales/`, `icons/`. The folder may be the archive's root or its only top
folder. **Install Plugin from Disk…** with the archive asks the user about the plugin's permissions,
as with a folder, then unpacks it into `~/Library/Application Support/flux/plugins/<id>/`, replacing an
older version.

```sh
cargo build --release --target wasm32-wasip2
tar czf hello-0.1.0.tar.gz flux-plugin.toml locales icons \
    target/wasm32-wasip2/release/hello_plugin.wasm
```

The plugin's data is in `~/Library/Application Support/flux/plugin-data/<id>/`; removing the plugin
keeps it, so a reinstalled plugin finds its settings again (its secrets go).

To publish a plugin for everyone, add it to the catalog — the Marketplace of Settings → Plugins:
[The plugin catalog](catalog.md).

## Bundled plugins

The plugins shipped with Flux are folders of [`plugins/`](../plugins) in the repository —
[`plugins/todo`](../plugins/todo), the TODO window, is one. `crates/flux-app/build.rs` builds each of
them for `wasm32-wasip2` (into `target/plugins`) and embeds its files into the binary, under the same
paths the folder has. Without the target installed, Flux is built without them, with a warning. A
bundled plugin can be turned off in Settings → Plugins, not removed. Examples that aren't bundled —
[`examples/api-playground`](../examples/api-playground) — live in [`examples/`](../examples).
