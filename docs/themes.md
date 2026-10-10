# Color themes for Flux

A Flux theme is a file: the colors of the window's glass and islands, of text and states, of version
control and diffs, of the code's highlighting and of the terminal. Themes come with plugins — Flux's
own Flux Night and Flux Day are the bundled plugin `flux.themes`
([`plugins/themes`](../plugins/themes)) — and the user picks one in **Settings → Appearance →
Theme**, or a light and a dark one that follow macOS (**Sync with OS**).

A theme is chosen by its name; the theme changes at once, without a restart.

## A theme file

```toml
name = "Dusk"
appearance = "dark"

[ui]
accent = "#ff8a65"
accent_text = "#ffab91"
selection = "#ff8a654d"

[syntax]
keyword = "#ff8a65"
"text.title" = { color = "#ffab91", bold = true }
comment = { color = "#7a8199", italic = true }

[terminal]
cursor = "#ffab91"
```

- `name` — the name people choose the theme by; it is also its key in the settings. A theme with the
  name of another one replaces it (a plugin under development over an installed one, for example).
- `appearance` — `"dark"` or `"light"`: the macOS appearance the theme is made for. Flux gives the
  windows this appearance (the window frame, the system's file panels), and **Sync with OS** offers
  dark themes for macOS's dark appearance and light ones for its light appearance.
- **A theme names only what it changes.** Everything else comes from the base of its appearance: Flux
  Night for a dark theme, Flux Day for a light one. The bases are complete themes — read
  [`flux-night.toml`](../plugins/themes/themes/flux-night.toml) and
  [`flux-day.toml`](../plugins/themes/themes/flux-day.toml) to start from one.
- Colors are `"#rrggbb"` or `"#rrggbbaa"`. The alpha matters for the surfaces: the window is glass, and
  the blurred desktop shows through what isn't opaque.
- A key Flux doesn't know is an error, with its name: a misspelled token doesn't silently do nothing.
  A theme that doesn't read shows up in the plugin's log and isn't offered.

### `[ui]` — the interface

The tokens of the design system, by name.

**Surfaces (glass)**

| Token | Colors |
|-------|--------|
| `frame` | the window's frame: under the islands, the title bar, the status bar (translucent: the glass) |
| `frame_glow` | the glow of the frame from its top left corner |
| `island` | an island: the project tree, the editor, the terminal panel, tool windows |
| `island_border` | the edge of an island, of the active tab |
| `sheen` | the highlight along the top edge of an island and a popup |
| `elevated` | popups: menus, the palette, pickers, tooltips, dialogs (make it opaque: there is no blur behind a popup) |
| `elevated_border` | the edge of a popup |
| `shadow` | shadows (components take a share of it) |
| `backdrop` | the veil over the window under a question dialog |
| `divider` | lines inside islands and popups |

**Text**

| Token | Colors |
|-------|--------|
| `foreground` | text: the editor's plain text is the theme's `[syntax]`, everything else this |
| `text_muted` | secondary text: paths, labels, inactive tabs, the status bar |
| `dim` | tertiary text: line numbers, placeholders, section labels, excluded files |
| `text_disabled` | what can't be used now |

**Interaction**

| Token | Colors |
|-------|--------|
| `accent` | the accent: focus, selection, the brand, switches and checkboxes that are on |
| `accent_text` | text and icons in the accent (links, the active section) — readable on the islands |
| `accent_soft` | an accent background: a toggle that is on, an icon's tile |
| `on_accent` | text and icons on a solid accent fill: a checked box's tick, a switch's knob |
| `hover` / `pressed` | a row or a button under the pointer / pressed |
| `list_selected` | the selected row of a list that has the focus |
| `list_selected_inactive` | the selected row without the focus (the active file in the tree) |
| `input_background` / `input_border` | a text field |
| `focus_border` / `focus_ring` | a focused field's border / the ring around it |
| `drop_target` | where a dragged file will drop |
| `keycap` / `keycap_border` | a key in a shortcut hint |

**States**

| Token | Colors |
|-------|--------|
| `success`, `warning`, `error`, `info` | states: icons, messages, counters |
| `modified` | unsaved changes: the dot on a tab |

**Editor**

| Token | Colors |
|-------|--------|
| `current_line` | the line of the cursor |
| `selection` | selected text |
| `cursor` | the cursor |
| `match_text` | the matched characters in lists (fuzzy search) |
| `search_match` / `search_match_active` | what a search found in the text / the current match |

**Version control** — as JetBrains IDEs color them

| Token | Colors |
|-------|--------|
| `vcs_modified`, `vcs_added`, `vcs_deleted`, `vcs_renamed`, `vcs_untracked`, `vcs_conflict` | the names of changed files: the tree, the tabs, the commit window |
| `diff_added`, `diff_modified`, `diff_deleted` | the change markers in the editor's gutter |
| `diff_added_bg`, `diff_modified_bg`, `diff_deleted_bg` | changed blocks in a diff |
| `diff_added_word`, `diff_modified_word`, `diff_deleted_word` | the changed words inside a block |
| `diff_conflict`, `diff_conflict_bg`, `diff_conflict_word` | the merge tool: a conflict |
| `diff_resolved_bg` | the merge tool: a change already taken into the result |
| `graph_lanes` | the lanes of the log's commit graph: a list of 8 colors, used in turn |
| `blame_recent` | the background of the annotations' column at the newest commit (older ones fade) |

**Shades** — they carry meaning: file types, categories, counters

| Token | Is |
|-------|----|
| `blue`, `indigo`, `violet`, `pink`, `red`, `orange`, `amber`, `lime`, `green`, `teal`, `cyan` | the palette's shades; sets of file icons color their icons with them by name |
| `folder` | a folder's icon |

### `[syntax]` — the code

A highlight scope and its color, or `{ color, bold, italic }`. Scopes are the capture names of the
languages' highlighting queries (`keyword`, `function.method`, `string.special`…); a scope a theme
doesn't name falls back along the dots — `function.method.builtin` takes `function.method`, then
`function`. A scope with no color at all is drawn in the text color.

The bases name these scopes: `attribute`, `boolean`, `comment`, `constant`, `constructor`,
`embedded` (code inside `${…}` and f-strings), `escape`, `function`, `keyword`, `label`, `number`,
`operator`, `property`, `punctuation`, `punctuation.special`, `string`, `string.escape`,
`string.special`, `tag`, `text.literal`, `text.reference`, `text.title`, `text.uri`, `type`,
`type.builtin`, `variable`, `variable.builtin`, `variable.parameter`. A theme may add finer ones
(`function.builtin`, `markup.heading`) for languages whose queries capture them.

### `[terminal]` — the terminal

| Key | Is |
|-----|----|
| `foreground` | the default text |
| `background` | the background programs assume (inverse video, answers to color queries); the terminal itself is transparent and shows the island, so this is the island without its alpha |
| `cursor` | the cursor |
| `ansi` | the 16 colors programs pick from: black, red, green, yellow, blue, magenta, cyan, white, then the same eight bright |

## A theme plugin

A theme is a plugin without code: a manifest that names its theme files, and the files.

```
dusk/
  flux-plugin.toml
  themes/dusk.toml
  locales/ru.toml        (optional: the manifest's strings in Russian)
```

```toml
id = "someone.dusk"
name = "Dusk"
version = "1.0.0"
api = "0.2"
authors = ["Someone"]
description = "A warm dark theme."

[[themes]]
file = "themes/dusk.toml"
```

One plugin may bring several themes — a `[[themes]]` each, as `flux.themes` does.

**Trying it out.** Settings → Plugins → ⚙ → **Install Plugin from Disk…** and pick the folder: it
becomes a plugin under development, and the theme appears in Settings → Appearance → Theme. Edit the
theme file and the theme follows: Flux reads a plugin under development again when its files change
(**Reload Dev Plugins** in the same menu does it at once). A mistake in the file — an unknown token, a
color that isn't one — is in the plugin's log (Settings → Plugins → the plugin → Log).

**Publishing.** Themes go to the plugin catalog like any plugin — see [the catalog](catalog.md).
