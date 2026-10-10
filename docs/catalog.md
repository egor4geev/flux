# The plugin catalog

Flux finds plugins in a catalog, as JetBrains IDEs find them in their Marketplace: **Settings →
Plugins → Marketplace** lists them, installs one with a click, and offers updates of the installed
ones. The catalog is a public repository, [`egor4geev/flux-plugins`](https://github.com/egor4geev/flux-plugins):
the plugins' sources, a build that packs them, and an index Flux reads. Plugins of other authors come
to it as pull requests, as extensions come to Zed.

## For users

- **Marketplace.** Search by name, description, author or language; filter by kind — Languages,
  Themes, Icons, Tools. A plugin's page shows its README, what it may do (its permissions, in plain
  words) and what it adds (languages, language servers, themes, icon sets, commands, tool windows).
- **Install.** Flux downloads the package, checks its SHA-256 against the index, asks about the
  plugin's permissions — as for a plugin installed from disk — and turns the plugin on. A language
  takes its files at once: open tabs get their highlighting without reopening.
- **Updates.** When Flux starts, it reads the catalog in the background and offers the newer
  versions of the installed plugins in a notification — **Update** or **Show**. The Installed tab
  shows the updates too (**Update to …**, **Update All**); its switch at the bottom turns the check
  at start off, and the gear menu's **Check for Updates** checks at any time. An update asks about
  permissions only when it asks for more than the installed version. Plugins under development are
  never replaced.
- **Suggestions.** A file Flux has no language for — `main.rs` before Rust is installed — gets a
  banner above the editor when the catalog has a plugin for it: **Install Rust**, or **Ignore
  Extension** (remembered; for a file known by its name, `Dockerfile`, **Ignore**).
- **Offline.** Flux keeps the last index it read (`~/Library/Caches/flux/plugins/catalog/`) and the
  packages it downloaded; without the network the Marketplace shows the cached catalog, and installs
  of already downloaded packages still work.

`FLUX_CATALOG_URL` makes Flux read another index — `https://…` or `file://…`, for example a local
build: `FLUX_CATALOG_URL=file:///path/to/flux-plugins/dist/index.json`.

## For authors

A plugin is published by a pull request that adds its folder, `plugins/<plugin id>/`, to the catalog
repository. Its [README](https://github.com/egor4geev/flux-plugins#readme) describes the layout and
the review; in short:

- the folder holds the plugin as Flux loads it: `flux-plugin.toml`, `README.md` (shown in the
  Marketplace), `locales/`, `icons/`, and what the plugin brings — queries, themes, icon sets;
- a language's tree-sitter grammar isn't committed: `grammars.toml` pins its repository and revision,
  and the build compiles it to WebAssembly with the tree-sitter CLI 0.27.1 into the path the manifest
  names (`[[grammars]] wasm`);
- a plugin with code has its `Cargo.toml` and `src/`; the build compiles it for `wasm32-wasip2`;
- `python3 scripts/build.py --check --only <plugin id>` checks it as CI will.

A new version is a new `version` in the manifest: a published version is never replaced, and Flux
offers the new one to everyone who has the plugin.

## The index

`index.json` (format 1) lists every plugin's latest version — what `flux_plugin::catalog::Index`
reads:

```json
{
  "format": 1,
  "generated": "2026-10-10T12:00:00Z",
  "plugins": [{
    "id": "flux.rust", "name": "Rust", "version": "0.1.0", "api": "0.2",
    "description": "…", "authors": ["…"], "repository": "https://github.com/…",
    "categories": ["languages"],
    "manifest": "<the package's flux-plugin.toml, as it is>",
    "download": "https://github.com/egor4geev/flux-plugins/releases/download/flux.rust-v0.1.0/flux.rust-0.1.0.tar.gz",
    "sha256": "…", "size": 117373,
    "icon_svg": "<svg …>", "readme": "# Rust …",
    "languages": [{ "id": "rust", "name": "Rust", "extensions": ["rs"], "file_names": [] }],
    "themes": [{ "name": "…", "appearance": "dark" }],
    "icon_themes": ["…"],
    "updated": "2026-10-10"
  }]
}
```

- `manifest` lets Flux show a plugin's permissions and contributions before it is downloaded; the
  package's own manifest is checked to be the same plugin and version when it is installed.
- `languages` are what suggestions look at; `themes` and `icon_themes` name what the plugin adds
  (read from the theme and icon set files).
- Flux reads only the formats it knows: a newer `format` is refused with a message.

## Packages

A package is a `.tar.gz` of the plugin's folder, files at the archive's root, without its sources
and build files. The build makes it deterministic — sorted files, no times, no owners — so its
SHA-256 identifies exactly what Flux installs. The packages are the assets of the catalog's GitHub
Releases, a release per plugin version (`<plugin id>-v<version>`).

## Versions and the plugin API

A plugin is offered as an update when its version is newer than the installed one by SemVer
(`1.10.0` after `1.9.2`; a pre-release before its release). Its `api` must be the plugin API this
Flux runs (`0.2`): the Marketplace shows a plugin for another API as **Needs a newer Flux** and doesn't
install it. While the API is young, the catalog takes plugins of the current version only; once it
settles, Flux will keep running older versions of the API, as Zed does.
