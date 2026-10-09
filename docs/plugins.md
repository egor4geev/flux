# Writing plugins for Flux

A Flux plugin adds commands, tool windows, status bar items and settings to the editor, and reacts
to what happens in the window: documents opened, edited and saved, the active tab changing, the
project changing. Plugins look like Flux itself — they describe their windows with elements, and Flux
draws them in its own design system.

> [!NOTE]
> The plugin API is version **0.1** and still young: it will grow (terminals, diffs, Git, a localhost
> server, themes, languages) and may change between versions. A manifest names the version it is
> built for.

## How plugins run

A plugin is a **WebAssembly component** of the `flux:plugin` world, run by
[wasmtime](https://wasmtime.dev) inside Flux. The API is described in WIT —
[`crates/flux-plugin/wit/flux-plugin.wit`](../crates/flux-plugin/wit/flux-plugin.wit) is the
reference; this guide summarizes it. Plugins are written in Rust with the SDK
([`crates/flux-plugin-api`](../crates/flux-plugin-api)) and built for the `wasm32-wasip2` target.

- **Isolated.** A plugin can't crash or freeze Flux. Each one runs on its own thread; a panic, a call
  longer than **10 seconds** or more than **512 MB** of memory stops the plugin, and Flux tells the user
  (with the details) and offers to start it again.
- **Sandboxed.** A plugin sees only what its permissions give it: its own data folder always, the
  project's files — through the file system or `project.search` — only with the `project` permission.
  No network, no other files, no processes, no environment.
- **One file for every platform.** The component is the same on macOS, Linux and Windows. Flux
  compiles it when it first loads it (tens of milliseconds) and keeps the result in a cache
  (`~/Library/Caches/flux/plugins`, the 64 most recently used).
- **Calls are cheap** — a fraction of a microsecond. Flux calls the plugin one call at a time and
  never waits for it; a call that needs the window (the text of a document) waits on the plugin's
  thread for Flux's answer — at most 5 seconds, then it gets an empty result or an error.
- **Declared up front.** The commands, tool windows and status bar items a plugin uses are those of its
  manifest: a call naming another one is ignored (the plugin's log says so), and a notification's
  action naming an unknown command is dropped.

## Quick start

1. Install the target once: `rustup target add wasm32-wasip2`.
2. Copy [`templates/plugin`](../templates/plugin) somewhere and rename it. In its `Cargo.toml`, point
   the `flux-plugin-api` dependency at your checkout of Flux (or use the `git` form in the comment).
3. Set the plugin's `id` and `name` in `flux-plugin.toml`.
4. In Flux: **Settings → Plugins → ⚙ → Install Plugin from Disk…** and pick the folder. A folder
   becomes a **plugin under development**: Flux builds it with cargo, loads it, and reloads it whenever
   its component changes. Rebuild it yourself (`cargo build --release --target wasm32-wasip2`), or
   run **Reload Dev Plugins** from the command palette (⇧⌘P), which rebuilds and reloads.
5. Run **Hello: Say Hello** from the palette, open the **Documents** window from its icon in the
   launchpad.

What the plugin logs and prints is in **Settings → Plugins → the plugin → Log**.

## A plugin in Rust

```rust
use flux_plugin_api::notify::Notice;
use flux_plugin_api::view::*;
use flux_plugin_api::{Event, Plugin, UiEvent, register_plugin, tr, trf};

struct Counter {
    count: u32,
}

impl Plugin for Counter {
    fn new() -> Self {
        Counter { count: 0 }
    }

    // `[[commands]] id = "count"` in the manifest.
    fn run_command(&mut self, command: &str) {
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
| `run_command(id)` | One of the manifest's commands ran: the palette, its keys, a notification's action, a status bar item. |
| `on_event(event)` | Something happened in the window, or the answer to a question came ([Events](#events)). |

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
| `view::*` | A tool window's content ([Tool windows](#tool-windows)). |
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
api = "0.1"
authors = ["Someone"]
description = "Says hello."
repository = "https://github.com/someone/hello"
icon = "icons/hello.svg"
wasm = "target/wasm32-wasip2/release/hello_plugin.wasm"

[permissions]
project = "read"

[notifications]
display = "balloon"

[[commands]]
id = "hello"
title = "Say Hello"
category = "Hello"
keys = "alt-cmd-shift-h"

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
| `id` | required | Lowercase letters and digits, words joined by `.` or `-`: `someone.hello-world`. The plugin's key everywhere: its folder, settings, notification group. |
| `name` | required | Shown in the manager, the palette's chips, Settings. |
| `version` | required | The plugin's version. |
| `api` | required | The plugin API it is built for: `"0.1"`. Flux runs only the version it has. |
| `authors` | `[]` | |
| `description` | `""` | Shown in the manager. |
| `repository` | — | A link for the manager. |
| `icon` | — | A monochrome SVG in the folder, for the manager. |
| `wasm` | — | The component, relative to the folder. A plugin without it has no code: it starts at once and only declares. |

**`[permissions]`** — what the plugin may do beyond its own folder and the API. The install question
lists them; the sandbox enforces them.

| Key | | |
|-----|-|-|
| `project` | `"none"` | The project's files: `"none"`, `"read"`, `"write"`. With it the project folder is open to the plugin's file system (at its real path) and `project.search` works — its results are the project's text. |

**`[notifications]`** — `display`: how the plugin's notifications show until the user changes it in
Settings → Notifications: `"balloon"` (a card that goes away, the default), `"sticky"` (a card that
stays), `"log"` (the Notifications window only), `"hidden"`.

**`[[commands]]`** — a command for the palette («category: title»), its keys and notification actions.

| Key | | |
|-----|-|-|
| `id` | required | What `run_command` gets. |
| `title` | required | |
| `category` | the plugin's name | The palette's chip. |
| `keys` | — | A default shortcut in gpui's notation: `cmd-shift-h`, `alt-f7`. A key Flux already uses is not taken (the plugin's log says so). |

**`[[tool-windows]]`** — a window in the island on the right, opened by its icon in the launchpad.

| Key | | |
|-----|-|-|
| `id` | required | The window the plugin fills with `ui.set-view`. |
| `title` | required | |
| `icon` | a puzzle piece | A monochrome 16×16 SVG in the folder, drawn in the launchpad's colors. |
| `keys` | — | A default shortcut that opens and hides the window. |

**`[[status-items]]`** — `id`: an item of the status bar; the plugin sets its text with
`status-bar.set`.

**`[[settings]]`** — a setting; Settings → the plugin shows a form of them. Values are kept in
`settings.json` under `plugins.settings.<id>`; a change comes as the `settings-changed` event.

| Key | | |
|-----|-|-|
| `key` | required | What `setting(key)` reads. |
| `title` | required | |
| `description` | — | Under the field. |
| `type` | required | `bool` (a switch), `string` (a field), `integer` (a number field), `choice` (one of `options`), `string-list` (a list of strings, a row each). |
| `default` | `false`, `""`, `0` or `min`, the first option, `[]` | Must fit the type. |
| `min`, `max` | — | For `integer`. |
| `options` | — | For `choice`: `[{ value = "file", title = "Current File" }, …]`. |

## The API

Each interface of [the WIT](../crates/flux-plugin/wit/flux-plugin.wit) is a module of `host`
(`host::editors::open`). Positions in documents are zero-based lines and columns **in characters**
(Unicode scalar values), as Flux counts them; lines end at `\n`. Documents are named by ids that stay
the same while the document is open; notifications and questions by ids the plugin gets back.

| Interface | Functions |
|-----------|-----------|
| `log` | `write(level, message)` — `debug`, `info`, `warn`, `error`. |
| `i18n` | `language()` — `"en"`, `"ru"`; `translate(text)`. |
| `commands` | `set-enabled(command, enabled)` — grays a command out in the palette (it stops running). |
| `notifications` | `notify(notification) -> id`; `update(id, notification)`; `set-progress(id, progress)` — a task in progress (indeterminate or a fraction; none — over); `expire(id)` — its actions stop working; `remove(id)`. A notification has a kind (info, success, warning, error), a title, a body, actions (a label and one of the plugin's commands) and `sticky`. |
| `dialogs` | `ask(question) -> id` — a question with buttons (roles: primary, normal, danger, cancel), a message and monospace details; `ask-text(question) -> id` — a question with a text field. The answers come as events. |
| `editors` | `active()`, `list()` — the documents (`id`, `path`, `language`, `modified`); `text(id)`; `selections(id)` — the primary first; `set-selections(id, ranges)`; `edit(id, edits)` — one undo step, ranges in the current text, not overlapping; `open(path, selection) -> id` — opens a file in a tab or goes to it, selects the span in the middle of the view; `save(id)` — starts saving (`editor-saved` says when it's on disk). |
| `project` | `root()`; `search(query, max-matches)` — the project's files by the rules of Find in Files (`.gitignore`, no binary or huge files): lines with matches, paths relative to the root. A query is text with `case-sensitive`, `whole-word` and `regex` (the syntax of the Rust `regex` crate). It needs the `project` permission and blocks the plugin, not Flux. |
| `storage` | `get(key)`, `set(key, value)` — a small store kept between launches; `data-dir()` — the plugin's folder, readable and writable. |
| `settings` | `get(key)` — the value as JSON: the user's, or the manifest's default. |
| `status-bar` | `set(id, item)` — the text, a tooltip and a command a click runs; none hides it. |
| `ui` | `set-view(window, view)` — a tool window's content; `show(window)`, `hide(window)`. |

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
| `toolbar` | Buttons in a row under the window's title, as in JetBrains tool windows. | |
| `text` | Spans of text, each with a tone (`normal`, `muted`, `dim`, `accent`, `success`, `warning`, `error`), bold, code, highlighted. `label` is plain text. | |
| `markdown` | Markdown, drawn as Flux draws documentation. | |
| `button` | A label and/or an icon, a tooltip, primary, enabled. `icon_button` — the toolbar's kind. | `clicked` |
| `text_field` | A text field. | `changed(text)`, `submitted(text)` on ↵ |
| `switch`, `checkbox` | A toggle with a label. | `toggled(on)` |
| `tree` | Rows with a key, a parent, an icon, a label of spans, a dim detail, a badge; flat rows make a list. Takes the remaining height, scrolls, draws only the visible rows. | `selected(key)`; `activated(key)` — ↵, a double click, a click on a row without children; `expanded((key, expanded))` |
| `divider`, `spacer`, `progress` | A line; the free space; a progress bar (none — indeterminate). | |

**Ids keep state.** Flux keeps an element's state between views by its id: a field's text, the
scroll, the expanded and selected rows. A row's `expanded` applies when the row first shows; after
that the user's choice wins. To start an element afresh — say, **Expand All** applying `expanded`
again — give it a new id (`"items-2"`).

**Icons** are named:

- a built-in icon by the file name in [`crates/flux-app/assets/icons`](../crates/flux-app/assets/icons)
  without `.svg`: `refresh`, `expand-all`, `collapse-all`, `search`, `settings`, `plus`, `minus`,
  `trash`, `pencil`, `info`, `warning`, `error`, `check`, `clock`, `history`, `terminal`, `branch`,
  `commit`, `star`, `tag`, `folder`, `file`, `puzzle`…;
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

Flux translates the manifest's strings with them (the name, command titles and categories, tool
window titles, settings); the plugin translates its own with `tr` and `trf` (`{0}`, `{1}`… filled
in). There are no plural forms: prefer «Files: 4» to «4 files».

## Plugins under development

**Install Plugin from Disk…** with a folder links it as a plugin under development: it stays where it
is, and an id it shares with an installed or bundled plugin overrides that one (that's how a bundled
plugin is developed). If the folder has a `Cargo.toml`, Flux builds it
(`cargo build --release --target wasm32-wasip2`, the output in the plugin's log); then it reloads the
plugin each time its component changes — after your own `cargo build` too. The manifest's `wasm`
names the component where cargo puts it: `target/wasm32-wasip2/release/<crate>.wasm` (dashes in the
crate's name become underscores).

The log keeps the plugin's `log` lines, what it prints — stdout as information, stderr (`eprintln!`,
a panic's message) as warnings — and what happened to it: started, stopped and why, built. It is also
a file: `~/Library/Logs/Flux/plugins/<id>.log`.

## Packaging

An archive — `.zip`, `.tar.gz` or `.tgz` — with the plugin's folder: the manifest, the component at
the manifest's `wasm` path, `locales/`, `icons/`. The folder may be the archive's root or its only top
folder. **Install Plugin from Disk…** with the archive unpacks it into
`~/Library/Application Support/flux/plugins/<id>/`, replacing an older version.

```sh
cargo build --release --target wasm32-wasip2
tar czf hello-0.1.0.tar.gz flux-plugin.toml locales icons \
    target/wasm32-wasip2/release/hello_plugin.wasm
```

The plugin's data is in `~/Library/Application Support/flux/plugin-data/<id>/`; removing the plugin
keeps it, as JetBrains IDEs keep a removed plugin's settings.

## Bundled plugins

The plugins shipped with Flux are folders of [`plugins/`](../plugins) in the repository —
[`plugins/todo`](../plugins/todo), the TODO window, is one. `crates/flux-app/build.rs` builds each of
them for `wasm32-wasip2` (into `target/plugins`) and embeds its files into the binary, under the same
paths the folder has. Without the target installed, Flux is built without them, with a warning. A
bundled plugin can be turned off in Settings → Plugins, not removed.
