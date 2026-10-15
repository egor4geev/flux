<div align="center">

<img src="docs/images/icon.png" width="112" alt="Flux">

# Flux

**A fast, native code editor for macOS — Git the way JetBrains IDEs do it, Claude Code built in.**

![status](https://img.shields.io/badge/status-early%20development-8590ff)
![platform](https://img.shields.io/badge/platform-macOS-a3abc3)
![license](https://img.shields.io/badge/license-MIT-4fd18b)

</div>

<p align="center">
  <img src="docs/images/hero.png" alt="Flux: the project tree, a Rust file with Claude's change marked in the gutter, and the Claude chat on the right with the edit and a summary of the changed files" width="100%">
</p>

## Features

### Fast and native

A Mac app written in Rust and drawn by the GPU: Flux opens instantly, and typing, scrolling and
searching never wait, even in files with tens of thousands of lines. Find a file by a few letters of
its name, search the whole project with a live preview, run any command from the palette — with the
keys you know from JetBrains IDEs, in English or Russian.

### Understands your code

Errors as you type, completion, go to definition, find usages and rename — from language servers Flux
installs by itself. <kbd>⌥</kbd><kbd>↵</kbd> on an error offers the quick fixes, and the scrollbar maps
the file's errors, warnings and changes.

### Git, the way JetBrains IDEs do it

A commit window with changelists and checkboxes down to single changes, a side-by-side diff you can
edit, every branch operation in one popup, a three-way merge tool, a log with a graph and an
interactive rebase, and who changed each line right in the gutter.

### Claude Code, built in

A native chat (<kbd>⌘</kbd><kbd>Esc</kbd>) that runs your own `claude` with your Claude subscription —
no API key. Every edit Claude proposes opens as a diff you accept or trim, and everything it changed
gathers in its own changelist to review, roll back or commit. <kbd>⌥</kbd><kbd>↵</kbd> asks Claude to
fix or explain the code right where it is.

### Plugins in a sandbox

Plugins run as WebAssembly: they can't crash Flux and reach only what you allowed when you installed
them. The Marketplace brings languages, color themes and file icons in one click — and integrations
such as [Yandex Tracker & Calendar](https://github.com/egor4geev/flux-plugins/tree/main/plugins/flux.yandex).
Write your own in Rust: [Writing plugins](docs/plugins.md).

### A terminal built in

Your own shell with its prompt and colours, under the editor or as a tab next to your files
(<kbd>⌥</kbd><kbd>F12</kbd>). Split it, search its output, and ⌘-click a `file:line` from a compiler
or a test run to jump right there.

## Install

Flux is getting ready for its first public beta. From the first release on, download the `.dmg` from
[Releases](https://github.com/egor4geev/flux/releases) and drag Flux to Applications; until then,
[build it from source](#build-from-source).

Flux isn't notarized by Apple, so macOS blocks the first launch of a downloaded Flux, saying it can't
verify the app. Once:

1. Open Flux and close the message.
2. Open **System Settings → Privacy & Security**, find the note that Flux was blocked and click
   **Open Anyway**.
3. Confirm with your password or Touch ID.

Since macOS 15, Control-click → Open no longer gets past this. The updates Flux downloads itself open
without asking.

## Build from source

You need [Rust](https://rustup.rs) with the WebAssembly target for the built-in plugins
(`rustup target add wasm32-wasip2`; without it Flux builds without them), [CMake](https://cmake.org)
(`brew install cmake`) and the Xcode Command Line Tools.

```sh
git clone https://github.com/egor4geev/flux.git
cd flux
scripts/bundle-macos.sh
open target/release/Flux.app
```

`main` follows the releases; development happens on `pre-release`. Open a folder with
<kbd>⌘</kbd><kbd>O</kbd> or pick a recent project on the start screen. For Claude, install
[Claude Code](https://code.claude.com) and sign in once.

## Feedback

- **Found a bug?** Help → Report a Bug… opens the form on GitHub with your version of Flux and your Mac
  already filled in — or [open an issue](https://github.com/egor4geev/flux/issues/new/choose) yourself.
- **An idea or a question?** Help → Suggest a Feature… and Ask a Question… take you to
  [Discussions](https://github.com/egor4geev/flux/discussions).
- **A problem with a plugin from the Marketplace?** The catalog's plugins have their own issues in
  [flux-plugins](https://github.com/egor4geev/flux-plugins/issues).
- **A security issue?** Please report it privately, as [SECURITY.md](SECURITY.md) explains.

## Contributing

Pull requests are welcome — read [CONTRIBUTING.md](CONTRIBUTING.md) first. They go to the
`pre-release` branch, not `main`.

## License

[MIT](LICENSE)
