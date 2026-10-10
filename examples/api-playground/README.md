# API Playground

Every capability of the Flux plugin API 0.2 behind a button, with what it returned under the button:
the manual check of the API, and an example to copy from ([`src/lib.rs`](src/lib.rs)). The guide is
[`docs/plugins.md`](../../docs/plugins.md).

The playground isn't bundled into Flux. To try it:

1. Once: `rustup target add wasm32-wasip2`.
2. In Flux: **Settings → Plugins → ⚙ → Install Plugin from Disk…**, pick this folder. Flux asks
   about its permissions (below), then it becomes a plugin under development: Flux builds it with
   cargo and reloads it when it changes.
3. Open the **API Playground** window from its flask in the launchpad, or **Playground: Open API
   Playground** in the palette (⇧⌘P).

Its manifest asks for every permission there is: `project = "write"`, the network to `api.github.com`
and `localhost`, a server, the program `git`, terminals, and reading `~/.config`.

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
| Secrets | **Save…**, **Read**, **Delete** | A value in the macOS keychain (an item named after the plugin): it survives a restart of Flux. |
| Timers | **Start Clock** | A clock in the status bar, ticking every second; a click on it stops it. |
| System | **Copy Text** | Text on the clipboard. |
| | **Home Folder** | `~/.config` is readable (the `folders` permission); `~/Documents` isn't. |
| | **Open Link** | Flux's repository in the browser. |
| Context Menus | — | Right-click a selection in the editor (**Show Context**), a file and a folder in the tree (**Show File Context**, **Show Folder Context**), a tab (**Show Tab Context**): the section and a notification show where the command was run from, the document, the selections and the paths. From the palette, the same commands get `Palette`. |
