# API Playground

Every capability of the Flux plugin API (0.3) behind a button, with what it returned under the
button: the manual check of the API, and an example to copy from ([`src/lib.rs`](src/lib.rs); the
sections of API 0.3 — [`src/api03.rs`](src/api03.rs)). The guide is
[`docs/plugins.md`](../../docs/plugins.md). Every result also goes to the plugin's log (**Settings →
Plugins → API Playground → Log**): the window scrolls only with the mouse.

The playground isn't bundled into Flux. To try it:

1. Once: `rustup target add wasm32-wasip2`.
2. In Flux: **Settings → Plugins → ⚙ → Install Plugin from Disk…**, pick this folder. Flux asks
   about its permissions (below), then it becomes a plugin under development: Flux builds it with
   cargo and reloads it when it changes.
3. Open the **API Playground** window from its flask in the launchpad, or **Playground: Open API
   Playground** in the palette (⇧⌘P).

Its manifest asks for every permission there is: `project = "write"`, the network to `api.github.com`
and `localhost`, a server, the program `git`, terminals, reading `~/.config`, and Claude Code sessions
(`claude = true`). Its settings page (**Settings → API Playground**) has a setting of every kind.

## What to try

| Section | Buttons | What should happen |
|---------|---------|--------------------|
| Network | **Fetch** | The repository of Flux from the GitHub API: status 200, the name, the stars, the default branch. |
| | **Denied Host** | `example.com` isn't in the manifest: the request is denied, and the error names the permission. |
| | **Stream** | With the server started: `/events` of the playground's own server through `http.start` — the status, then five events, one every 0.4 s, then «Done». |
| Local Server | **Start** / **Stop** | The address of the server on 127.0.0.1. |
| | **Open in Browser** | A page from the server; its WebSocket says hello and the plugin echoes it back (the page shows the echo, the section the message). |
| Programs | **git --version** | The output of `git --version`. |
| | **Spawn git log** | `git log` in the background: the output comes in pieces, then the exit code. |
| | **ls (denied)** | `ls` isn't in the manifest: denied. |
| Terminal | **Run git log** | A tab «Git Log» in the terminal panel; it stays after the command ends, and the section gets the exit code. |
| | **Shell in Editor** | A shell among the editor's tabs, with the keyboard. |
| | **Send echo** | Types `echo $FLUX_PLAYGROUND` into that shell: it prints «hello from the API Playground». |
| Proposed Edits | **Propose Header** | A diff tab for the active file with a comment line added on top. Accept (⌘↵) writes it (one undo step); Reject leaves the file; the section says which. |
| | **Withdraw** | Closes the open proposal without an answer. |
| Git | **Repositories**, **Status**, **Diff** | The project's repositories (branch, HEAD), the changes, the active file's diff. «git-changed events» counts commits, checkouts, saves. |
| Problems | **List** | The problems Flux knows of, by file. |
| | **Warn Here** | A warning of the plugin's own on the caret's line: underlined, on hover, F2 goes to it, the status bar counts it. |
| | **Clear** | The warning goes. |
| Secrets | **Save…**, **Read**, **Delete** | A value Flux keeps encrypted, under the plugin's name: it survives a restart of Flux. |
| Timers | **Start Clock** | A clock in the status bar, ticking every second; a click on it stops it. |
| System | **Copy Text** | Text on the clipboard. |
| | **Home Folder** | `~/.config` is readable (the `folders` permission); `~/Documents` isn't. |
| | **Open Link** | Flux's repository in the browser. |
| Context Menus | — | Right-click a selection in the editor (**Show Context**), a file and a folder in the tree (**Show File Context**, **Show Folder Context**), a tab (**Show Tab Context**): the section and a notification show where the command was run from, the document, the selections and the paths. From the palette, the same commands get `Palette`. |

The sections of API 0.3:

| Section | Buttons | What should happen |
|---------|---------|--------------------|
| Forms | **Open Task…** | A dialog as a tracker's Open Task: a dropdown (Move to), a checkbox that enables the branch's name, another checkbox, a comment of several lines (↵ — a new line, ⌘↵ — Start), a note. The section shows the answer. Empty the branch's name and Start: the form comes back with the error and the values kept; Cancel — «Cancelled». |
| Pickers | **Search** | A picker the plugin searches itself: type «remind» — PLAY-12 under its status; ↵ chooses it. |
| | **Filter** | The same tasks, filtered by Flux (fuzzy); Esc — «dismissed». |
| | **At the Clock** | A menu above the clock of the Timers section (start the clock first; without it the menu opens under the title bar); **Stop the Clock** stops it. |
| | the status badge | A badge with ▾: a menu of statuses under it; the choice becomes the badge. |
| Settings Page | **Status**, **Busy** | The `status` line of Settings → API Playground: a green text with the time; a spinner with «Checking…». |
| | **Set Note** | `settings.set` writes the two-line note: an open settings page shows it at once. |
| | **Read** | The token (`secrets.get` — only whether it is saved and its length), the note, the bookmarks (`records`). |
| | **Open Settings** | Settings on the playground's page: the section API 0.3, a token (a masked field — a secret), Check (runs a command with its argument: the status line says so), the status, the note (several lines), bookmarks (records: add, remove, move). |
| Branches | **List** | The repository's branches, local then remote, the current marked. |
| | **New Branch** | `playground/<n>` from the current branch, as the Branches popup makes it; «done» when it's checked out (git-done). |
| | **Back** | Checks out the branch it came from. |
| | **Template On** / **Off** | The commit window (⌘0): an empty message gets «PLAY-12 Remind about a meeting»; off — it is withdrawn while unedited. |
| Links | **Link PLAY-n** | `PLAY-12` and the like become links: hold ⌘ over one in a file (underlined; ⌘-click — the section says «Activated»), hover it or press F1 at it (the plugin's tooltip), commit messages in the Git log and its details, Claude's chat. |
| | **Bad Pattern** | A regex that doesn't compile: rejected, the error names it. |
| | **Clear** | The links go. |
| Claude | **Discuss** | A new Claude session with the task as a chip (PLAY-12) and a question in the field; nothing is sent. |
| | **Plan** | A new session in plan mode: the task and the request go at once. |
| Contexts | **Switch A ↔ B** | The open tabs are saved as «Playground B» and closed (A has none yet); again — saved as A, and B's tabs come back. **Load Context…** in the palette lists both. |
| | **Forget** | Both contexts are forgotten. |
| Local Time | **Local Time** | The time zone, the offset from UTC, the local time as Tracker's work log takes it. |
| Badges and Fields | — | A badge of every tone; a field of several lines: ⌘↵ sends it, the section shows what came. |
| A Window in the Editor | **Open Board** | A window of the plugin among the editor's tabs: the tasks in columns by status. **Playground: Open Playground Board** in the palette too. |
| | **Rename** | The tab's title changes (`ui.set-title`). |
| | **Close** | The tab closes (`ui.hide`). |
