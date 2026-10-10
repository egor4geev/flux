# File icons for Flux

The icons of files and folders — in the project tree, on tabs, in the commit window, in Find in
Files, in the file finder — come from a **set of file icons**. A set is a file that maps file names
and extensions to SVGs and colors; sets come with plugins. Flux's own set, **Flux Icons**, is the
bundled plugin `flux.icons` ([`plugins/icons`](../plugins/icons)); the user picks a set in
**Settings → Appearance → File Icons**, and the icons change at once, without a restart.

With no set in use (Flux Icons turned off in Settings → Plugins, say), files and folders get Flux's
plain icons.

## A set file

```toml
name = "Pastel"

[defaults]
file = { icon = "icons/file.svg", color = "text-muted" }
folder = { icon = "icons/folder.svg", color = "folder" }
folder-open = { icon = "icons/folder-open.svg", color = "folder" }

[file-names]
"Cargo.toml" = { icon = "icons/package.svg", color = "orange" }
"Dockerfile" = { icon = "icons/docker.svg", color = "#2496ed" }

[name-prefixes]
"license" = { icon = "icons/license.svg", color = "amber" }
".env." = { icon = "icons/config.svg", color = "lime" }

[extensions]
"rs" = { icon = "icons/rust.svg", color = "orange" }
"d.ts" = { icon = "icons/types.svg", color = "blue" }
"ts" = { icon = "icons/typescript.svg", color = "blue" }
"png" = { icon = "icons/image.svg" }
```

- `name` — the name people choose the set by; it is also its key in the settings. A set with the name
  of another one replaces it (a plugin under development over an installed one, for example).
- `[defaults]` — `file`: any file nothing else matches; `folder` and `folder-open`: folders, closed
  and expanded (an expanded folder takes `folder` when the set has no `folder-open`). Each is
  optional: what a set leaves out stays Flux's plain icon.
- `[file-names]` — exact file names, in any case: `"cargo.toml"` matches `Cargo.toml`.
- `[name-prefixes]` — the start of a name, in any case: `LICENSE`, `LICENSE-MIT`, `licence.txt`
  all start with `"lic"`. The longest prefix that matches wins.
- `[extensions]` — what comes after a dot, without the dot, in any case. Compound extensions work:
  `"d.ts"` matches `types.d.ts`, `"tar.gz"` matches `archive.tar.gz`; the longest one that matches
  wins, so `index.d.ts` takes `"d.ts"` and `index.ts` takes `"ts"`.

**The order** for a file: its exact name, then a name prefix, then its extension, then the icon the
file's language plugin gives (below), then the set's `defaults.file`, then Flux's plain icon.

An unknown key — a section, a kind of default, a key of an entry — is an error naming the key; the
set doesn't load, and the mistake is in the plugin's log (Settings → Plugins → the plugin → Log). An
entry whose SVG isn't among the plugin's files is dropped (the log says which), and that file falls
back to what comes next.

## Icons and colors

An entry is `{ icon = "icons/rust.svg", color = "orange" }`:

- `icon` — an SVG in the plugin's folder, 16×16 (`viewBox="0 0 16 16"`).
- `color` — makes the icon **monochrome**: Flux draws the SVG's shape as a mask in this color (the
  SVG's own colors don't matter). A color is either a **shade of the theme's palette**, so the icon
  follows the theme — light and dark ones alike — or a fixed `"#rrggbb"` / `"#rrggbbaa"`.
- No `color` — a **full-color** icon: Flux draws the SVG with its own colors. Make sure it reads on
  both light and dark themes (a colored tile with a white glyph does; a dark glyph on nothing doesn't).
  `tint` — optional, a palette shade or `#rrggbb` — is the color of what goes along with the file:
  the file type's badge in the status bar, for one; `text-muted` by default.

The palette shades: `blue`, `indigo`, `violet`, `pink`, `red`, `orange`, `amber`, `lime`, `green`,
`teal`, `cyan`, `folder`; the text colors `foreground`, `text-muted`, `dim`, `text-disabled`; and
`accent`, `accent-text`, `success`, `warning`, `error`, `info`, `modified`. What each looks like is
in [the themes' documentation](themes.md) — Flux Icons uses the shades for the files' languages and
roles (Rust and packages orange, TypeScript and Python blue, Go cyan, lock files dim…).

**Monochrome glyphs** look like Flux's own when they are drawn as Flux's are: a 16×16 grid, about
1.75 px from the edge, a 1.5 stroke with round caps and joins, or a filled shape; letters as paths
(Flux's SVG renderer has no fonts, so `<text>` isn't drawn); cut-outs through a `<mask>`.

## Language plugins' own icons

A language plugin may give its files an icon (`icon` and `icon-color` of `[[languages]]` —
[Language plugins](languages.md)): a Kotlin plugin, for example, brings the Kotlin glyph. It is used
when the set in use has no icon for those files — so a set needn't know every language, and a set
that does know one wins.

## A set plugin

A set is a plugin without code: a manifest that names its set files, the files, and the SVGs.

```
pastel/
  flux-plugin.toml
  icon-themes/pastel.toml
  icons/rust.svg  icons/package.svg  …
  locales/ru.toml        (optional: the manifest's strings in Russian)
```

```toml
id = "someone.pastel-icons"
name = "Pastel Icons"
version = "1.0.0"
api = "0.2"
authors = ["Someone"]
description = "Soft colored file icons."

[[icon-themes]]
file = "icon-themes/pastel.toml"
```

One plugin may bring several sets — an `[[icon-themes]]` each. Read Flux's own
[`flux.toml`](../plugins/icons/icon-themes/flux.toml) for a complete monochrome set; the catalog's
**Flux Tiles** is a full-color one (white glyphs on colored tiles).

**Trying it out.** Settings → Plugins → ⚙ → **Install Plugin from Disk…** and pick the folder: it
becomes a plugin under development, and the set appears in Settings → Appearance → File Icons. Edit
the set file and the icons follow: Flux reads a plugin under development again when its files change
(**Reload Dev Plugins** in the same menu does it at once). An SVG edited in place may stay cached
until Flux restarts.

**Publishing.** Sets go to the plugin catalog like any plugin — see [the catalog](catalog.md).
