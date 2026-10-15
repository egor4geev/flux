<div align="center">

<img src="docs/images/icon.png" width="112" alt="Flux">

# Flux

**A fast, minimal code editor for macOS.**

![status](https://img.shields.io/badge/status-early%20development-8590ff)
![platform](https://img.shields.io/badge/platform-macOS-a3abc3)
![license](https://img.shields.io/badge/license-MIT-4fd18b)

</div>

<p align="center">
  <img src="docs/images/editor.png" alt="Flux: the project tree, the editor and a split terminal on a frosted-glass window" width="100%">
</p>

Flux is a code editor that gets out of your way. It opens instantly, stays responsive on huge files
and keeps everything you need one shortcut away — no setup, no clutter.

> [!NOTE]
> Flux is in early development. It runs on macOS today; more platforms will follow.

## Features

### Fast and calm

Typing, scrolling and searching never wait — even in files with tens of thousands of lines. Your
project and your code live in separate islands on a soft glass window, and colour is used for meaning:
every file type has its own icon, every change its own shade. The start screen keeps your recent
projects and the actions you need first, each with its shortcut.

<img src="docs/images/start-screen.png" alt="Start screen with quick actions and recent projects" width="100%">

### Light or dark, your way

Flux Night and Flux Day: the same calm glass, dark or light. Let Flux follow macOS between a light and
a dark theme, or pick one — Settings → Appearance shows each theme as it will look, and more themes and
sets of file icons come from the Marketplace. Switch the theme, the icons or the interface language
without leaving the keyboard: <kbd>⌃</kbd><kbd>`</kbd>, as in JetBrains IDEs.

<img src="docs/images/flux-day.png" alt="Flux Day, the light theme: the project tree and a SCSS file highlighted by a plugin from the Marketplace" width="100%">

<img src="docs/images/appearance.png" alt="Settings → Appearance → Theme: Flux Night and Flux Day as previews, and Sync with OS" width="100%">

### Islands or flat, at any size

Keep the panels as islands on the glass, or set them flat — edge to edge, divided by thin lines, on a
solid window, the classic look of JetBrains IDEs. And make the whole window larger or smaller, the code
included, from 80 to 200 %: <kbd>⌃</kbd><kbd>⌥</kbd><kbd>=</kbd> and <kbd>⌃</kbd><kbd>⌥</kbd><kbd>-</kbd>,
<kbd>⌃</kbd><kbd>⌥</kbd><kbd>0</kbd> to go back, as Zoom IDE does. Both change at once, in every window.

<img src="docs/images/flat.png" alt="The flat look: the project tree, a Rust file and a terminal edge to edge, divided by lines" width="100%">

Settings → Appearance → Interface shows both looks and the scale; Quick Switch (<kbd>⌃</kbd><kbd>`</kbd>) has
the look too.

<img src="docs/images/interface.png" alt="Settings → Appearance → Interface: Islands and Flat, and the scale from 80 to 200 %" width="100%">

### Find anything

Jump to a file by a few letters of its name (<kbd>⌘</kbd><kbd>P</kbd>). Search the whole project with a
live preview of every match, grouped by file, and open the one you need without leaving the keyboard
(<kbd>⇧</kbd><kbd>⌘</kbd><kbd>F</kbd>) — type a file's name there, even with a typo, and the file comes
first. Inside a file, find and replace with case, whole-word and regular-expression options — and turn
every match into a cursor to edit them all at once.

<img src="docs/images/find-in-files.png" alt="Find in Files: the file whose name matches first, then the matches in the text, and a live preview" width="100%">

### Markdown, rendered

A Markdown file opens with its preview beside it, as in JetBrains IDEs: the text, the text and the
preview, or the preview alone — three buttons in the corner, and Flux remembers your choice. The preview
follows your typing and your scrolling, shows pictures, tables and code, and its links open files in Flux
and pages in the browser. Long lines of prose wrap at the edge of the editor — in Markdown and text files
by default, in any file you add in Settings → Editor, and in any editor at all with <kbd>⌃</kbd><kbd>⌥</kbd><kbd>W</kbd>.

<img src="docs/images/markdown.png" alt="README.md with its long lines wrapped, and its rendered preview beside the text" width="100%">

### Understands your code

Errors as you type, completion with documentation, documentation on hover, go to definition, find
usages, rename across the project and reformat. JavaScript and TypeScript come with Flux; Rust, Go,
Python, TOML, YAML, JSON, Bash, Markdown, HTML, CSS and SCSS, Dockerfile, SQL and Makefile are one click
away in the Marketplace — open such a file and Flux offers the plugin. Flux uses the language servers
you already have and quietly installs the missing ones in the background.

Press <kbd>⌥</kbd><kbd>↵</kbd> on an error for the quick fixes and refactorings the language server
offers, as in JetBrains IDEs, or right-click the code for everything else: go to, find usages, rename,
reformat, Git and Claude.

<img src="docs/images/context-actions.png" alt="⌥↵ on an error: the language server's quick fix and refactorings, and Claude to fix or explain the code" width="100%">

The scrollbar maps the whole file, as in JetBrains IDEs: errors, warnings, what you searched for and your
changes since the last commit — hover a mark to read it, click it to go there. The corner of the editor
counts the problems, and <kbd>F2</kbd> takes you to the next one. When a language server can't load your
project, Flux says so in the status bar and tells you why; click the status for every server of the window
— restart, stop, read its log, or set it up in `settings.json`, where your settings apply without a restart.

<img src="docs/images/scrollbar.png" alt="A Rust file with error, warning and change marks on the scrollbar, a tooltip over an error mark, and the problem count in the corner" width="100%">

### A terminal built in

Your own shell with its prompt and colours, in a panel under the editor or as a tab next to your
files. Split it, search its output, and ⌘-click a `file:line` from a compiler or a test run to jump
right there. Closing a terminal that still runs something asks first.

### Your project at a glance

A file tree that hides what Git ignores, follows the file you are editing and updates as files change
on disk. Create, rename, move and delete files right in it — open tabs keep up with renamed and moved
files, and open files pick up changes made outside Flux. Pictures open in a tab of their own: fitted to
the window or at 1:1, zoomed with <kbd>⌘</kbd><kbd>=</kbd> / <kbd>⌘</kbd><kbd>−</kbd>, with their size in
pixels — and an SVG switches between the picture and its text.

### A window for every project

Open as many projects as you like, each in its own window with its own language servers, terminals
and Claude sessions — or switch the current window to another project. Like JetBrains IDEs, Flux
asks which you prefer until you tell it to remember, and switching to a project that is already open
just brings its window forward. Every project keeps its tabs, cursor positions, chats and layout, and
when you start Flux from the Dock, the projects you had open come back where you left them. Quit from the
Dock or log out with unsaved files, and Flux asks first, as it does for <kbd>⌘</kbd><kbd>Q</kbd>.

<img src="docs/images/open-project.png" alt="Flux asks whether to open another project in this window or a new one" width="100%">

Click the project's name in the title bar for your recent projects with their branches — open one here
or in a new window (<kbd>⌥</kbd><kbd>↵</kbd>), or forget it.

<img src="docs/images/recent-projects.png" alt="The project's menu in the title bar: recent projects with their branches, and Open…" width="100%">

### Git, the way JetBrains IDEs do it

Everything around Git is one shortcut away, and it works with all the repositories in your folder.

**Changes and commit.** Changed lines are marked in the gutter as you type, with a rollback in one
click. The commit window lists your changes with checkboxes for files and even single changes,
remembers your previous messages, amends, and commits and pushes in one go; push shows what is about
to leave.

<img src="docs/images/commit.png" alt="The commit window with checkboxes, changed lines in the gutter and the previous text of a change" width="100%">

**A diff you can edit.** Side by side or unified, with changed words highlighted. The right side is
your file: fix it right there, roll back a change with an arrow, tick single changes for the commit.

<img src="docs/images/diff.png" alt="A side-by-side diff with connectors, changed words and checkboxes for single changes" width="100%">

**Branches in one popup.** Search, recent and favorite branches, local and remote ones grouped by
prefix, tags — and every operation a click away: a checkout that keeps your changes, a new branch,
merge, rebase, compare, push and pull. Update the whole project, fetch, stash and unstash.

<img src="docs/images/branches.png" alt="The branches popup with search, recent and local branches, favorites and a branch's actions" width="100%">

**Conflicts.** A merge, a rebase or an unstash that conflicts opens a three-way merge tool: yours, the
result and theirs. The changes that don't conflict are already applied; take a side with an arrow, or
let the magic wand merge what it can.

<img src="docs/images/merge.png" alt="The three-way merge tool: yours, the result and theirs, with arrows that take a side" width="100%">

**History.** The log draws your branches as a graph, filters by branch, author, date, path or text and
shows the details of every commit. Cherry-pick, revert, reset and an interactive rebase are right
there, and so is the history of a file or of the lines you selected.

<img src="docs/images/log.png" alt="The Git window: the log with a graph of branches, filters and the details of the selected commit" width="100%">

**Who changed each line.** Turn on annotations (<kbd>⌥</kbd><kbd>⌘</kbd><kbd>A</kbd>) to see the
author and the date of the last change of every line; hover for the commit, click to find it in the
log.

<img src="docs/images/blame.png" alt="Annotations in the gutter: the date and the author of the last change of every line" width="100%">

### Never lose a message

Results of Git operations, errors with their full output, language servers installed or stopped, files
changed on disk — all land in one Notifications window with the time, the source and the actions;
unread ones are counted on the bell. Choose per source how much you see: a card that fades, one that
stays, only the journal, or nothing — or turn on Do Not Disturb.

<img src="docs/images/notifications.png" alt="The Notifications window on the right: results and errors with their source, time and actions" width="100%">

Questions come in Flux's own dialogs that look like the rest of the editor, with the keys you expect
from macOS.

<img src="docs/images/dialog.png" alt="A Flux dialog asking to save changes, with Don't Save, Cancel and Save" width="100%">

### Plugins that can't break your editor

Each plugin runs in its own WebAssembly sandbox: it can't crash or freeze Flux, and it reaches only
what you allowed when you installed it. Turn plugins on and off without a restart, install one from a
folder or an archive, read its log — all in Settings → Plugins.

<img src="docs/images/plugins.png" alt="Settings → Plugins: plugins under development and built-in ones with their switches, and the details of the TODO plugin" width="100%">

Plugins add commands with shortcuts, tool windows, status bar items, settings pages and items of the
context menus, and look like the rest of Flux. A TODO window comes built in; write your own in Rust —
see [Writing plugins](docs/plugins.md).

<img src="docs/images/todo.png" alt="The TODO window on the right: TODO and FIXME comments of the project grouped by file" width="100%">

A plugin can work with the world outside the editor: call a web service and sign in to it, run a
local server for a tool or an AI agent to talk to, run programs and terminals, propose an edit you
review as a diff, read Git and the problems in your code, and show problems of its own. It can ask
in a dialog with several fields, offer a searchable popup, open a wide view as an editor tab, make
a branch the way Flux does, turn text into links, start a Claude session with its context and
switch the set of open tabs. It gets
only what you allowed: when you install it, Flux lists in plain words which sites it may reach,
which programs it may run and which folders it may read, and points out what deserves a second
look.

<img src="docs/images/plugin-permissions.png" alt="The question before installing a plugin: the project's files, the sites it may connect to, a local server, the programs it may run, terminals — highlighted — and a folder it may read" width="100%">

Find plugins in the Marketplace: languages with their language servers, color themes and sets of file
icons, ready to install in one click and updated when a new version comes out. When you open a file
Flux has no language for, it tells you which plugin knows it.

<img src="docs/images/marketplace.png" alt="Settings → Plugins → Marketplace: languages, themes and icon sets, with the Rust plugin's details" width="100%">

<img src="docs/images/plugin-suggestion.png" alt="A Rust file without its plugin: a banner over the editor offers to install Rust" width="100%">

### Your tasks, next to the code

The Yandex Tracker & Calendar plugin brings your work into Flux. The board of tasks opens as a tab,
its columns side by side, as on Tracker's own board, with filters by assignee, type, priority, tags and
components that you can save; a task opens on the right with every one of its fields, its description,
its links to other tasks, its comments and its history. Take a task to work with one
button: Flux moves it to In Progress, makes its branch, opens the task's own tabs, starts a timer —
and hands Claude your prompt, filled in from the task. Your prompts are yours to write: several
Claude actions in the plugin's settings, with the task's key, title, description, links and comments
put in where you say.

<img src="docs/images/tracker.png" alt="The board of tasks in an editor tab, columns by status with cards, and a task on the right: its status, Take to Work and Plan, the description, links to other tasks and comments" width="100%">

Task keys in code, commit messages and Claude's chat become links with a tooltip; the task you're
working on stays in the status bar with its timer, and closing it logs the time. The next meeting of
the day waits there too — click it for today's and tomorrow's agenda, and join a Telemost call
from it or from the reminder.

<img src="docs/images/calendar.png" alt="Today's and tomorrow's meetings in a popup over the status bar, with Telemost calls marked" width="100%">

Every task has its own set of tabs, as in JetBrains IDEs: switching tasks puts your files and
Claude chats away and brings the other task's back — and Save Context, Load Context and Clear
Context work without a tracker too.

### Claude Code, built in

Talk to Claude in a window on the right (<kbd>⌘</kbd><kbd>Esc</kbd>), or move the chat to a tab next
to your files. It reads like a chat: your messages on the right, Claude's answers on the left, the quick
steps it takes folded into a single line, and its commands, edits and subagents in cards of their own.
Every turn that changed files ends with a summary of them — review the changes or roll them back from
there. Claude asks before it changes anything: each edit opens as a diff you can trim or edit before you
accept it — and Claude learns what you changed.

<img src="docs/images/claude.png" alt="A Claude chat in an editor tab: the message on the right, Claude's edit with its diff, the summary of the changed files, and a shell command run with !" width="100%">

Answer Claude's questions and approve its plans with a click or a key. Mention files with `@` or send
your selection (<kbd>⌥</kbd><kbd>⌘</kbd><kbd>K</kbd>), paste screenshots, pick the model, the effort
and the mode, and keep an eye on your subscription's limits in the status bar. Start a message with `!`
to run a shell command in the project — Claude sees its output. Open a subagent — from its card or from
the list of what runs in the background — to watch it work, search
the conversation with <kbd>⌘</kbd><kbd>F</kbd>, edit an earlier message to take the conversation another
way, or roll back the code Claude changed since then. Flux runs your own `claude` command with your
Claude subscription — no API key.

<img src="docs/images/claude-question.png" alt="Claude asks a question with options to choose from, right in the chat" width="100%">

Ask about code right where it is: from the editor's menu or <kbd>⌥</kbd><kbd>↵</kbd>, Claude explains
the selection, fixes the errors in it, looks for bugs, writes tests or documentation — each request in
a fresh session named after it. Send files and folders from the tree or a tab, or drop them on the chat.
Claude sees what Flux sees: it asks the language servers for errors, definitions and usages, and learns
about the errors its edit caused right after making it.

Everything Claude changed in a session gathers in its own changelist in the commit window. Compare a
file with its text before Claude, roll back a file or a single change, and commit Claude's work with
one checkbox — Claude can even write the commit message, in the style of your history.

<img src="docs/images/claude-review.png" alt="The commit window with Claude's changelist, a file compared with its text before Claude, and the session on the right" width="100%">

Pick up any earlier conversation of the project — started in Flux or in the terminal — from the history
(or type `/resume`), and the sessions you had open come back when you reopen the project. Each session's
tab shows what it's busy with — a command and its clock, subagents, a question for you — and macOS lets
you know when a session finishes or waits while you're in another app.

<img src="docs/images/claude-history.png" alt="The history of the project's Claude sessions with search, branch and time" width="100%">

### Keyboard first, in your language

Every command is in the command palette with its shortcut, and the commands you run most come first.
The keys are familiar if you come from JetBrains IDEs. The interface speaks English or Russian — following your system, or the one you pick in
Settings → Appearance, at once.

## Get started

```sh
git clone git@github.com:egor4geev/flux.git
cd flux
scripts/bundle-macos.sh
open target/release/Flux.app
```

You will need [Rust](https://rustup.rs) with the WebAssembly target for the built-in plugins:
`rustup target add wasm32-wasip2` (without it Flux still builds, just without them), and
[CMake](https://cmake.org) for the build: `brew install cmake`. Open a folder
with <kbd>⌘</kbd><kbd>O</kbd> or pick a recent project on the start screen. For Claude, install
[Claude Code](https://code.claude.com) and sign in once; <kbd>⌘</kbd><kbd>Esc</kbd> opens the chat.

## Shortcuts

| | |
|---|---|
| <kbd>⌘</kbd><kbd>P</kbd> | Find a file |
| <kbd>⇧</kbd><kbd>⌘</kbd><kbd>F</kbd> | Find in Files |
| <kbd>⇧</kbd><kbd>⌘</kbd><kbd>P</kbd> | All commands |
| <kbd>⌘</kbd><kbd>F</kbd> · <kbd>⌘</kbd><kbd>R</kbd> | Find · replace in a file |
| <kbd>⌘</kbd><kbd>B</kbd> · <kbd>⌥</kbd><kbd>F7</kbd> | Go to definition · find usages |
| <kbd>⌥</kbd><kbd>↵</kbd> | Quick fixes and context actions, Fix with Claude |
| <kbd>F2</kbd> · <kbd>⇧</kbd><kbd>F2</kbd> | Next · previous problem |
| <kbd>⌃</kbd><kbd>⌥</kbd><kbd>W</kbd> | Wrap long lines in this editor, or stop wrapping them |
| <kbd>⇧</kbd><kbd>F6</kbd> | Rename everywhere |
| <kbd>⌘</kbd><kbd>1</kbd> | Show or hide the project tree |
| <kbd>⌥</kbd><kbd>F12</kbd> · <kbd>⌘</kbd><kbd>T</kbd> | Show or hide the terminal · new terminal |
| <kbd>⌘</kbd><kbd>K</kbd> · <kbd>⇧</kbd><kbd>⌘</kbd><kbd>K</kbd> | Commit · push |
| <kbd>⇧</kbd><kbd>⌘</kbd><kbd>B</kbd> · <kbd>⌘</kbd><kbd>T</kbd> | Branches · update the project (in a terminal, ⌘T opens a new one) |
| <kbd>⌘</kbd><kbd>0</kbd> · <kbd>⌃</kbd><kbd>V</kbd> | Show or hide the commit window · Git operations |
| <kbd>⌘</kbd><kbd>9</kbd> · <kbd>⌥</kbd><kbd>⌘</kbd><kbd>A</kbd> | Show or hide the Git log · who changed each line |
| <kbd>⌘</kbd><kbd>Esc</kbd> · <kbd>⌥</kbd><kbd>⌘</kbd><kbd>K</kbd> | Claude · mention the selection in Claude's message |
| <kbd>⌃</kbd><kbd>`</kbd> | Quick Switch: theme, look, file icons, language |
| <kbd>⌃</kbd><kbd>⌥</kbd><kbd>=</kbd> · <kbd>⌃</kbd><kbd>⌥</kbd><kbd>-</kbd> · <kbd>⌃</kbd><kbd>⌥</kbd><kbd>0</kbd> | Zoom in · zoom out · back to 100 % |
| <kbd>⌘</kbd><kbd>`</kbd> · <kbd>⇧</kbd><kbd>⌘</kbd><kbd>`</kbd> | Next · previous project window |
| <kbd>⌥</kbd><kbd>⇧</kbd><kbd>N</kbd> · <kbd>⌥</kbd><kbd>⇧</kbd><kbd>B</kbd> | Open a task · the task in the browser (Yandex Tracker plugin) |
| <kbd>⌥</kbd><kbd>⇧</kbd><kbd>X</kbd> | Clear the context: close the tabs, keep them for later |

## Roadmap

- [x] Editing, tabs, syntax highlighting
- [x] Search and navigation
- [x] Project tree
- [x] New look
- [x] Code intelligence: errors, completion, go to definition
- [x] Built-in terminal
- [x] Git: changes, diff, commit and push
- [x] Git: branches, stash, conflicts
- [x] Git: history and blame
- [x] Notification center and unified dialogs
- [x] Plugins: sandboxed plugins, the plugin manager, a TODO window
- [x] Claude Code: a chat that runs your `claude`, with every edit as a diff
- [x] Claude Code: a review of Claude's changes, session history, the editor's menu
- [x] Plugins: web services, programs, terminals, edits to review, context menus — with your permission
- [x] Plugins: the Marketplace, languages, themes and file icons as plugins, a light theme, Appearance settings
- [x] Before the beta: fixes after a week of daily use, a window for every project
- [x] Before the beta: Claude polish — a new chat, `!` commands, live subagents, search, edit and rewind
- [x] Before the beta: tasks — Yandex Tracker and Calendar, task contexts, plugin API 0.3
- [x] Before the beta: a flat look and zoom, Markdown preview, files in Find in Files, recent projects
- [x] Before the beta: soft wraps, a scrollbar with the file's marks, language servers' status and settings, a safe quit
- [ ] Before the beta: updates over the air, a signed and notarized app
- [ ] Public beta

## License

MIT
