<div align="center">

<img src="docs/images/icon.png" width="112" alt="Flux">

# Flux

**A fast, minimal code editor for macOS.**

![status](https://img.shields.io/badge/status-early%20development-8590ff)
![platform](https://img.shields.io/badge/platform-macOS-a3abc3)
![license](https://img.shields.io/badge/license-MIT-4fd18b)

</div>

<p align="center">
  <img src="docs/images/editor.png" alt="Flux: the project tree and the editor on a frosted-glass window" width="100%">
</p>

Flux is a code editor that gets out of your way. It opens instantly, stays responsive on huge files
and keeps everything you need one shortcut away — no setup, no clutter.

> [!NOTE]
> Flux is in early development. It runs on macOS today; more platforms will follow.

## Why Flux

- **Fast.** Typing, scrolling and searching never wait — even in files with tens of thousands of lines.
- **Calm and clear.** Your project and your code live in separate panels on a soft glass window.
  Colour is used for meaning: every file type has its own icon.
- **Find anything.** Jump to a file by a few letters, search the whole project with a live preview,
  find and replace inside a file.
- **Your project at a glance.** A file tree that hides what Git ignores, updates as files change on
  disk and keeps your open tabs in sync when you rename or move files.
- **Keyboard first.** Every command is in the command palette with its shortcut; familiar keys if
  you come from JetBrains IDEs.
- **Speaks your language.** English or Russian, following your system settings.

<table>
  <tr>
    <td width="50%"><img src="docs/images/start-screen.png" alt="Start screen with quick actions and recent projects"></td>
    <td width="50%"><img src="docs/images/find-in-files.png" alt="Find in Files with results and a live preview"></td>
  </tr>
  <tr>
    <td align="center"><sub><b>Start screen</b></sub></td>
    <td align="center"><sub><b>Find in Files</b></sub></td>
  </tr>
</table>

## Get started

```sh
git clone git@github.com:egor4geev/flux.git
cd flux
scripts/bundle-macos.sh
open target/release/Flux.app
```

You will need [Rust](https://rustup.rs). Open a folder with <kbd>⌘</kbd><kbd>O</kbd> or pick a
recent project on the start screen.

## Shortcuts

| | |
|---|---|
| <kbd>⌘</kbd><kbd>P</kbd> | Find a file |
| <kbd>⇧</kbd><kbd>⌘</kbd><kbd>F</kbd> | Find in Files |
| <kbd>⇧</kbd><kbd>⌘</kbd><kbd>P</kbd> | All commands |
| <kbd>⌘</kbd><kbd>F</kbd> · <kbd>⌘</kbd><kbd>R</kbd> | Find · replace in a file |
| <kbd>⌘</kbd><kbd>B</kbd> | Show or hide the project tree |

## Roadmap

- [x] Editing, tabs, syntax highlighting
- [x] Search and navigation
- [x] Project tree
- [x] New look
- [ ] Code intelligence: errors, completion, go to definition
- [ ] Built-in terminal
- [ ] Vim mode
- [ ] Public beta

## License

MIT
