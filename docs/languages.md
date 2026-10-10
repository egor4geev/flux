# Language plugins

A language plugin teaches Flux a language: which files are of it, how to highlight them, and which
language server to start for them — the errors, completion, hover, go to definition and refactoring
of that server. It needs no code: a manifest (`flux-plugin.toml`) and a few files next to it — the
highlighting query, the tree-sitter grammar compiled to WebAssembly, an icon, translations. Read
[Writing plugins for Flux](plugins.md) first for what a plugin is, its manifest and how to install
one.

Flux comes with one language plugin, JavaScript and TypeScript
([`plugins/javascript`](../plugins/javascript)); the other languages are plugins of the catalog.

```toml
id = "flux.rust"
name = "Rust"
version = "0.1.0"
api = "0.2"
authors = ["Someone"]
description = "Rust: highlighting and rust-analyzer."

[[languages]]
id = "rust"
name = "Rust"
extensions = ["rs"]
aliases = ["rs"]
grammar = "rust"
highlights = "languages/rust/highlights.scm"
icon = "icons/rust.svg"
icon-color = "orange"

[[grammars]]
id = "rust"
wasm = "grammars/rust.wasm"

[[language-servers]]
id = "rust-analyzer"
command = "rust-analyzer"
languages = ["rust"]
install = { rustup = "rust-analyzer", fallback = { github = "rust-lang/rust-analyzer", asset = "rust-analyzer-{arch}-apple-darwin.gz", bin = "rust-analyzer" } }
```

```
flux.rust/
├── flux-plugin.toml
├── grammars/rust.wasm
├── languages/rust/highlights.scm
├── icons/rust.svg
└── locales/ru.toml
```

Paths in the manifest are relative to the plugin's folder, `/`-separated, without `..`.

## Languages

Each `[[languages]]` table is a file type.

| Key | | |
|---|---|---|
| `id` | required | The key Flux knows the language by: `rust`, `typescript`, `tsx`. Lowercase letters, digits, `-`, `_`, `+`, `#` (`c++`, `c#`). It is also a name of Markdown code fences (```` ```rust ````). |
| `name` | required | The name people read: the status bar, the manager. Translated by `locales/` like any string of the manifest. |
| `extensions` | | Extensions without the dot, in lowercase: `["rs"]`. A file matches in any case. |
| `file-names` | | Exact file names: `["Cargo.lock", ".bashrc"]`. A file name wins over an extension. |
| `aliases` | | More names of code fences: `["rs"]`, `["py", "python3"]`. |
| `grammar` | required | The `id` of one of the plugin's `[[grammars]]`. Several languages may share a grammar (JavaScript and JSX do). |
| `highlights` | required | The highlighting query: a file, or a list of files joined in this order. |
| `precedence` | | `"last-pattern"` (the default) or `"first-pattern"` — see [Highlighting queries](#highlighting-queries). |
| `lsp-id` | | The language id language servers know it by (`textDocument/didOpen`); the `id` by default. |
| `icon` | | A monochrome 16×16 SVG of the plugin: the icon of the language's files when the chosen set of file icons has none for them. |
| `icon-color` | | The icon's color: a shade of the theme (`blue`, `indigo`, `violet`, `pink`, `red`, `orange`, `amber`, `lime`, `green`, `teal`, `cyan`, `text-muted`, `dim`) or `"#rrggbb"`. A shade follows the theme, light or dark. |

When two plugins claim the same file, the one Flux reads later wins: a plugin under development over an
installed one, an installed one over a bundled one — that is how a bundled language is replaced.

Language ids language servers expect differ from Flux's ones now and then:

| Files | `lsp-id` |
|---|---|
| `.jsx` | `javascriptreact` |
| `.tsx` | `typescriptreact` |
| `.sh`, `.bash`, `.bashrc` | `shellscript` |
| `.jsonc` | `jsonc` |

## Grammars

A `[[grammars]]` table is a [tree-sitter](https://tree-sitter.github.io) grammar compiled to
WebAssembly:

| Key | | |
|---|---|---|
| `id` | required | What `[[languages]]` name it by. |
| `wasm` | required | The `.wasm` in the plugin's folder. |
| `symbol` | | The grammar's own name: the module exports `tree_sitter_<symbol>`. The `id` (with `-` as `_`) by default; set it when they differ (`c_sharp`). |

Flux's bundled plugins name a grammar compiled into Flux with `builtin = "<name>"` instead of `wasm`
(only `javascript`, `typescript`, `tsx`); a catalog plugin always ships its `.wasm`.

### Building a grammar

With the tree-sitter CLI of the same version as Flux's tree-sitter, **0.27.1** (grammar ABI 13–15):

```sh
cargo install --locked tree-sitter-cli@0.27.1
git clone --depth 1 --branch v0.24.2 https://github.com/tree-sitter/tree-sitter-rust
tree-sitter build --wasm -o grammars/rust.wasm tree-sitter-rust
```

- The grammar's folder needs `src/parser.c`, the external scanner `src/scanner.c` if it has one, and
  `src/grammar.json` (without it the CLI reads `grammar.js` with its own JavaScript runtime — Node.js
  isn't needed). A repository with several grammars names the folder: `tree-sitter-typescript/tsx`.
- The first build downloads two toolchains: **wasi-sdk** (171 MB, 606 MB unpacked) and **binaryen**
  for `wasm-opt` (7 MB), into `~/.cache/tree-sitter` (`XDG_CACHE_HOME` moves it). Neither emscripten
  nor Docker is needed. `TREE_SITTER_WASI_SDK_PATH` and `TREE_SITTER_BINARYEN_PATH` point the CLI at
  existing installs.
- The CLI compiles with `clang --target=wasm32-wasip1 -fPIC -shared -Os` and optimizes with
  `wasm-opt`. An external scanner may use only the C functions of tree-sitter's WebAssembly runtime
  (memory, strings, `ctype.h` — about 25); the CLI rejects one that needs more.
- Grammars are a few hundred kilobytes to a megabyte and a half (Rust 1.1 MB, Python 460 KB, JSON
  6 KB).

### How Flux runs it

- **Sandboxed.** The grammar runs in WebAssembly: a scanner that crashes or writes out of bounds
  costs the file its highlighting, not Flux. Flux parses WebAssembly grammars off the UI thread.
- **Fast enough.** A full parse is about 1.5–2.4 times slower than the same grammar compiled natively
  (a 6,000-line Rust file: 19 ms instead of 11); a reparse after typing, 1–2 times (a fraction of a
  millisecond). The first time Flux loads a grammar it compiles it — a few to tens of milliseconds,
  in the background — and keeps the result in a cache.
- The highlighting query runs natively whatever the grammar: it costs the same.

## Highlighting queries

A `highlights.scm` in tree-sitter's query language, as the grammar's repository has it (most ship
`queries/highlights.scm`; check its license). Flux highlights what the query captures:

```scheme
(line_comment) @comment
(string_literal) @string
(function_item name: (identifier) @function)
["fn" "let" "mut"] @keyword
```

**Capture names.** Flux's themes color these scopes; a capture name falls back along its dots to the
nearest one a theme has (`function.method.builtin` → `function.method` → `function`):

`attribute`, `boolean`, `comment`, `constant`, `constructor`, `embedded`, `escape`, `function`,
`keyword`, `label`, `number`, `operator`, `property`, `punctuation`, `punctuation.special`, `string`,
`string.escape`, `string.special`, `tag`, `text.literal`, `text.reference`, `text.title`,
`text.uri`, `type`, `type.builtin`, `variable`, `variable.builtin`, `variable.parameter`.

A capture with no scope (`@none`, `@spell`) is not highlighted and doesn't hide the captures under
it; names that start with `_` are internal.

**Precedence.** When several patterns capture the same text, the later pattern wins — the rule of
tree-sitter-highlight since 0.21, and of most queries (the generic `(identifier) @variable` first,
special cases after). Queries written for the older rule put the special cases first and the generic
ones last (the JSON and Go grammars' own queries do): say `precedence = "first-pattern"` for them. A
nested capture always wins over the one around it.

**Several files.** `highlights = ["languages/javascript.scm", "languages/typescript.scm"]` joins them
in this order — with the later-pattern rule, the more specific file comes last (TypeScript after
JavaScript, though the grammar's `tree-sitter.json` lists them the other way round).

**Predicates.** `#eq?`, `#match?`, `#any-of?` and their `not-` and `any-` forms filter matches.
`#is-not? local` passes (Flux doesn't track local variables), a pattern with `#is?` is turned off,
`#set!` and unknown predicates are ignored. There are no injections (code inside Markdown fences,
strings) and no locals.

## Language servers

A `[[language-servers]]` table is a [language server](https://microsoft.github.io/language-server-protocol/)
for some files:

| Key | | |
|---|---|---|
| `id` | required | The server's name and key: the status bar, Settings → Language Servers, its folder among the servers Flux installed. |
| `command` | required | The program: a name looked up on the `PATH` (and the usual places a GUI app's `PATH` lacks — Homebrew, `~/.cargo/bin`, `~/go/bin`, nvm, Volta…), then among the servers Flux installed; or an absolute path. |
| `args` | | Its arguments: `["--stdio"]`. |
| `languages` | | The languages whose files it serves, by `id` — of this plugin or any other. |
| `extensions`, `file-names` | | Instead of the languages' files: these. For a server that serves only some of a language's files (bash-language-server: `.sh` and `.bash`, not `.zsh`). One of `languages`, `extensions`, `file-names` is required. |
| `initialization-options` | | `initializationOptions` of `initialize`, as a TOML table. |
| `settings` | | Answers to `workspace/configuration`, by section: `settings = { python = { analysis = { typeCheckingMode = "basic" } } }`. |
| `install` | | How Flux installs the server when it isn't on the Mac. |

```toml
[[language-servers]]
id = "vscode-json-language-server"
command = "vscode-json-language-server"
args = ["--stdio"]
languages = ["json", "jsonc"]
# Formatting is off unless asked for.
initialization-options = { provideFormatter = true }
install = { npm = ["vscode-langservers-extracted"], bin = "vscode-json-language-server" }
```

A file may have several servers: it is open on all of them, the first one is the main one
(completion, hover, navigation), the problems of each are kept apart, and formatting goes to the first
one that can. The order is the plugins' order (bundled first, then by name) and, within a plugin,
the manifest's — Python's pyright comes before ruff, its formatter and linter. A server with the `id`
of another plugin's server replaces it.

### Installing a server

When a file needs a server that isn't on the Mac, Flux installs it — into
`~/Library/Application Support/flux/servers/<id>/`, with the progress in the status bar — unless the
user turned that off (Settings → Language Servers, where installed servers are updated and deleted
too). Four recipes:

```toml
# npm packages — the server first, then what it needs — with a Node.js of Flux's own (the latest LTS,
# checked by its SHA-256); `bin` is the executable in node_modules/.bin.
install = { npm = ["typescript-language-server", "typescript@6"], bin = "typescript-language-server" }

# A binary of the latest GitHub release: `asset` with {arch} (aarch64, x86_64) and {tag} (the release
# tag) — an archive (.tar.gz, .tar.xz, .zip, .gz) or the binary itself; `bin` is its name inside. A
# `<asset>.sha256` next to it in the release is checked.
install = { github = "astral-sh/ruff", asset = "ruff-{arch}-apple-darwin.tar.gz", bin = "ruff" }

# `go install <package>@latest` (needs Go; without it the server says so, quietly).
install = { go = "golang.org/x/tools/gopls", bin = "gopls" }

# `rustup component add <component>` (needs rustup), otherwise another recipe.
install = { rustup = "rust-analyzer", fallback = { github = "rust-lang/rust-analyzer", asset = "rust-analyzer-{arch}-apple-darwin.gz", bin = "rust-analyzer" } }
```

A changed recipe reinstalls the server the next time it is needed.

## Icons

A language's `icon` is a monochrome 16×16 SVG — shapes only: text in an SVG isn't drawn, so letters
are paths. Flux colors it with `icon-color`. It is the icon of the language's files in the project
tree, the tabs and the lists when the chosen set of file icons (Settings → Appearance → File Icons)
has no icon of its own for them.

## Developing a language plugin

**Install Plugin from Disk…** (Settings → Plugins, ⚙) with the plugin's folder links it as a plugin
under development. Flux reads it again when its manifest or a file it names changes — a query, a
grammar, an icon — so an edited query shows in the open files at once. What Flux couldn't read (a
missing query, a grammar it couldn't load) is in the plugin's log: Settings → Plugins → the plugin →
Log.
