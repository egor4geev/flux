#!/usr/bin/env python3
"""flux icon generator: 16×16, single-colour (gpui supplies the colour). UI glyphs use a 1.5
stroke with round caps and joins; file types are recognisable glyphs (filled or stroked).

Source of the icon set: edit icons here, then
    python3 scripts/gen-icons.py crates/flux-app/assets/icons
A new icon also needs a variant in `icons!` (`crates/flux-app/src/icons.rs`). To see the whole
set: `swift scripts/icon-sheet.swift crates/flux-app/assets/icons sheet.png [filter]`
(64 px plus 16/32 px as in the tree). Rules: the wiki note "Iconography"."""
import sys, os

OUT = sys.argv[1]
S = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">'
F = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="#000">'
B = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">'

# Document outline with a folded corner: shared by file, file-code, file-text, file-plus,
# find-in-files.
DOC = '<path d="M9.25 1.75H4.75a1.5 1.5 0 0 0-1.5 1.5v9.5a1.5 1.5 0 0 0 1.5 1.5h6.5a1.5 1.5 0 0 0 1.5-1.5V5.25z"/><path d="M9.25 1.75v2.5a1 1 0 0 0 1 1h2.5"/>'

def knockout(shape, cut, sw="1.4"):
    """A filled shape with letters or signs cut out by a stroke (mask)."""
    return (B + '<defs><mask id="k" maskUnits="userSpaceOnUse" x="0" y="0" width="16" height="16">'
            '<rect width="16" height="16" fill="#fff"/>'
            f'<g fill="none" stroke="#000" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round">{cut}</g>'
            f'</mask></defs><g mask="url(#k)" fill="#000">{shape}</g>')


def spark_silhouette():
    """The flux logo in one colour: six ribbon rays of different lengths with rounded tips and a
    dot in the middle — the same geometry as the colour logo (`scripts/gen-logo.py`, table `RAYS`;
    change them together), scaled 100 → 16."""
    import math
    k = 0.16
    spec = [(-96, 45), (-37, 35), (14, 42), (70, 33), (122, 44), (178, 36)]
    out = []
    for ang, length in spec:
        ax = (math.cos(math.radians(ang)), math.sin(math.radians(ang)))
        nx = (-ax[1], ax[0])
        L, base, tip = length * k, 17 * k, 9 * k
        p = lambda d, s: (8 + ax[0] * d + nx[0] * s, 8 + ax[1] * d + nx[1] * s)
        f = lambda q: f"{q[0]:.2f} {q[1]:.2f}"
        r = tip / 2
        out.append(f'<path d="M{f(p(0, base / 2))} L{f(p(L - r, r))} A{r:.2f} {r:.2f} 0 0 0 {f(p(L - r, -r))} L{f(p(0, -base / 2))}Z"/>')
    return "".join(out) + '<circle cx="8" cy="8" r="1.3"/>'

ICONS = {
  # ---------- Interface ----------
  "arrow-down": (S, '<path d="M8 2.75v10.5M3.75 9 8 13.25 12.25 9"/>'),
  "arrow-up": (S, '<path d="M8 13.25V2.75M3.75 7 8 2.75 12.25 7"/>'),
  "chevron-down": (S, '<path d="M4 6l4 4 4-4"/>'),
  "chevron-right": (S, '<path d="M6 4l4 4-4 4"/>'),
  "close": (S, '<path d="M4 4l8 8M12 4l-8 8"/>'),
  "plus": (S, '<path d="M8 2.75v10.5M2.75 8h10.5"/>'),
  "file": (S, DOC),
  "file-plus": (S, DOC + '<path d="M8 7.75v4M6 9.75h4"/>'),
  "folder-plus": (S, '<path d="M1.75 4.25c0-.83.67-1.5 1.5-1.5h2.38c.4 0 .78.16 1.06.44L7.75 4.25h5c.83 0 1.5.67 1.5 1.5v6.5c0 .83-.67 1.5-1.5 1.5h-9.5c-.83 0-1.5-.67-1.5-1.5z"/><path d="M8 6.75v4.5M5.75 9h4.5"/>'),
  "project": (S, '<path d="M1.75 4.25c0-.83.67-1.5 1.5-1.5h2.38c.4 0 .78.16 1.06.44L7.75 4.25h5c.83 0 1.5.67 1.5 1.5v6.5c0 .83-.67 1.5-1.5 1.5h-9.5c-.83 0-1.5-.67-1.5-1.5z"/><path d="M1.75 6.75h12.5"/>'),
  "collapse-all": (S, '<path d="M4.75 2.5 8 5.75l3.25-3.25M4.75 13.5 8 10.25l3.25 3.25M2.75 8h10.5"/>'),
  "search": (S, '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5l3.25 3.25"/>'),
  "find-in-files": (S, '<path d="M8.75 1.75h-4a1.5 1.5 0 0 0-1.5 1.5v9.5a1.5 1.5 0 0 0 1.5 1.5H6.5"/><path d="M8.75 1.75l4 4V6.5"/><circle cx="10" cy="10.25" r="2.5"/><path d="M11.85 12.1l1.9 1.9"/>'),
  "replace": (S, '<path d="M2.75 5.25h7a2.75 2.75 0 0 1 0 5.5h-5"/><path d="M6.75 8.25 4.25 10.75l2.5 2.5"/>'),
  "replace-all": (S, '<path d="M3.75 4.75h6.5a2.75 2.75 0 0 1 0 5.5H7.5"/><path d="M9.25 7.75 6.75 10.25l2.5 2.5M5.75 7.75 3.25 10.25l2.5 2.5"/>'),
  "case-sensitive": (S, '<path d="M1.75 12.25 4.5 4l2.75 8.25M2.65 9.5h3.7"/><circle cx="11.25" cy="10" r="2.25"/><path d="M13.5 7.75v4.5"/>'),
  "whole-word": (S, '<circle cx="5" cy="8.25" r="2"/><path d="M7 6.25v4"/><path d="M9.25 3.5v6.75"/><circle cx="11.25" cy="8.25" r="2"/><path d="M1.75 11.75v1.5h12.5v-1.5"/>'),
  "regex": (S, '<path d="M11.25 2.5v6.5M8.4 4.1l5.7 3.3M14.1 4.1 8.4 7.4"/><rect x="2.5" y="10.25" width="3.25" height="3.25" rx=".75" fill="#000" stroke="none"/>'),
  "command": (S, '<path d="M6 6V4.25A1.75 1.75 0 1 0 4.25 6H6zm0 0h4m-4 0v4m4-4V4.25A1.75 1.75 0 1 1 11.75 6H10zm0 0v4m0 0h1.75A1.75 1.75 0 1 1 10 11.75V10zm0 0H6m0 0v1.75A1.75 1.75 0 1 1 4.25 10H6z"/>'),
  "sidebar": (S, '<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2"/><path d="M6.25 2.75v10.5M3.75 5.5h.5M3.75 7.75h.5"/>'),
  # Terminal splits: a window halved by a divider, side by side or one above another.
  "split-right": (S, '<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2"/><path d="M8 2.75v10.5"/>'),
  "split-down": (S, '<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2"/><path d="M1.75 8h12.5"/>'),
  "branch": (S, '<circle cx="4.75" cy="3.75" r="1.5"/><circle cx="4.75" cy="12.25" r="1.5"/><circle cx="11.25" cy="5.75" r="1.5"/><path d="M4.75 5.25v5.5M11.25 7.25c0 2.5-6.5 1.5-6.5 3.5"/>'),
  "clock": (S, '<circle cx="8" cy="8" r="6.25"/><path d="M8 4.75V8l2.25 1.5"/>'),
  "hash": (S, '<path d="M6.5 2.5 5.25 13.5M10.75 2.5 9.5 13.5M2.75 5.75h10.75M2.25 10.25H13"/>'),
  "warning": (S, '<path d="M7.13 2.5a1 1 0 0 1 1.74 0l5.55 9.75a1 1 0 0 1-.87 1.5H2.45a1 1 0 0 1-.87-1.5z"/><path d="M8 6.25v3"/><circle cx="8" cy="11.25" r=".4" fill="#000"/>'),
  "error": (S, '<circle cx="8" cy="8" r="6.25"/><path d="M5.75 5.75l4.5 4.5M10.25 5.75l-4.5 4.5"/>'),
  "info": (S, '<circle cx="8" cy="8" r="6.25"/><path d="M8 7.25v3.75"/><circle cx="8" cy="5" r=".4" fill="#000"/>'),
  "sparkle": (F, '<path d="M7 1.5c.42 3.15 1.35 4.08 4.5 4.5C8.35 6.42 7.42 7.35 7 10.5 6.58 7.35 5.65 6.42 2.5 6 5.65 5.58 6.58 4.65 7 1.5zM12 9c.24 1.75.75 2.26 2.5 2.5-1.75.24-2.26.75-2.5 2.5-.24-1.75-.75-2.26-2.5-2.5 1.75-.24 2.26-.75 2.5-2.5z"/>'),
  "settings": (S, '<path d="M6.85 1.75h2.3l.35 1.8 1.35.78 1.73-.6 1.15 2-1.38 1.2v1.54l1.38 1.2-1.15 2-1.73-.6-1.35.78-.35 1.8h-2.3l-.35-1.8-1.35-.78-1.73.6-1.15-2 1.38-1.2V6.73l-1.38-1.2 1.15-2 1.73.6 1.35-.78z"/><circle cx="8" cy="8" r="2"/>'),
  "terminal": (S, '<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2"/><path d="M4.75 6.25 6.75 8l-2 1.75M8.5 10h2.75"/>'),
  # The flux mark: a flow of three lines converging into the stem of an "f".
  "logo": (F, spark_silhouette()),
  "folder": (F, '<path d="M1.25 4.25c0-1.1.9-2 2-2h2.55c.53 0 1.04.21 1.41.59l.99.99c.14.14.33.22.53.22h4.02c1.1 0 2 .9 2 2v6.2c0 1.1-.9 2-2 2H3.25c-1.1 0-2-.9-2-2z"/>'),
  "folder-open": (F, '<path d="M1.25 4.25c0-1.1.9-2 2-2h2.55c.53 0 1.04.21 1.41.59l.99.99c.14.14.33.22.53.22h3.52c1.1 0 2 .9 2 2v.45H5.4c-.86 0-1.62.55-1.9 1.36l-1.73 5.1A2 2 0 0 1 1.25 12z"/><path d="M4.45 8.15c.14-.4.52-.65.94-.65h9.12c.68 0 1.16.67.95 1.32l-1.3 3.95c-.27.83-1.04 1.38-1.9 1.38H2.98c-.34 0-.58-.34-.47-.66z"/>'),

  # ---------- File types ----------
  "file-code": (S, DOC + '<path d="M6.75 8.25 5.25 9.75l1.5 1.5M9.25 8.25l1.5 1.5-1.5 1.5"/>'),
  "file-text": (S, DOC + '<path d="M5.75 8.25h4.5M5.75 10.75h3"/>'),
  # Rust: a gear with a cut-out "R".
  "file-rust": ("raw", knockout(
      '<path d="M7.1 1.1h1.8l.33 1.53 1.13.47 1.32-.86 1.27 1.27-.86 1.32.47 1.13 1.54.33v1.8l-1.54.33-.47 1.13.86 1.32-1.27 1.27-1.32-.86-1.13.47-.33 1.53H7.1l-.33-1.53-1.13-.47-1.32.86-1.27-1.27.86-1.32-.47-1.13L1.1 8.9V7.1l1.54-.33.47-1.13-.86-1.32 1.27-1.27 1.32.86 1.13-.47z"/>',
      '<path d="M6.1 11V5.1h2.4a1.6 1.6 0 0 1 0 3.2H6.1M8.3 8.3 10.1 11"/>', "1.45")),
  "file-typescript": ("raw", knockout(
      '<rect x="1.5" y="1.5" width="13" height="13" rx="2.75"/>',
      '<path d="M4.6 7.6h3.6M6.4 7.6v4.9M12.6 8.15c-.3-.4-.75-.6-1.3-.6-.75 0-1.3.45-1.3 1.05 0 .65.5.95 1.25 1.15.85.25 1.4.55 1.4 1.3 0 .75-.6 1.25-1.45 1.25-.6 0-1.15-.25-1.45-.7"/>')),
  "file-javascript": ("raw", knockout(
      '<rect x="1.5" y="1.5" width="13" height="13" rx="2.75"/>',
      '<path d="M7.35 7.6v3.55c0 .85-.45 1.35-1.2 1.35-.45 0-.85-.2-1.1-.55M12.6 8.15c-.3-.4-.75-.6-1.3-.6-.75 0-1.3.45-1.3 1.05 0 .65.5.95 1.25 1.15.85.25 1.4.55 1.4 1.3 0 .75-.6 1.25-1.45 1.25-.6 0-1.15-.25-1.45-.7"/>')),
  "file-react": ("raw", '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.15"><ellipse cx="8" cy="8" rx="6.6" ry="2.5"/><ellipse cx="8" cy="8" rx="6.6" ry="2.5" transform="rotate(60 8 8)"/><ellipse cx="8" cy="8" rx="6.6" ry="2.5" transform="rotate(120 8 8)"/><circle cx="8" cy="8" r="1.35" fill="#000" stroke="none"/>'),
  "file-json": (S, '<path d="M5.5 2.25c-1.25 0-1.9.6-1.9 1.75v1.6c0 .95-.5 1.6-1.6 2.4 1.1.8 1.6 1.45 1.6 2.4V12c0 1.15.65 1.75 1.9 1.75M10.5 2.25c1.25 0 1.9.6 1.9 1.75v1.6c0 .95.5 1.6 1.6 2.4-1.1.8-1.6 1.45-1.6 2.4V12c0 1.15-.65 1.75-1.9 1.75"/><circle cx="8" cy="8" r=".85" fill="#000" stroke="none"/>'),
  "file-markdown": (S, '<rect x="1" y="3.25" width="14" height="9.5" rx="2" stroke-width="1.3"/><path d="M3.75 10.25v-4.5l2 2.25 2-2.25v4.5M11.5 5.75v4.5M9.75 8.75l1.75 1.75 1.75-1.75"/>'),
  "file-readme": (S, '<path d="M8 4.25c-1.25-1.1-3.25-1.5-5.75-1.25v9.5c2.5-.25 4.5.15 5.75 1.25 1.25-1.1 3.25-1.5 5.75-1.25V3C11.25 2.75 9.25 3.15 8 4.25zM8 4.25v9.25"/>'),
  "file-toml": (S, '<path d="M4.25 2.5h-2v11h2M11.75 2.5h2v11h-2M5.25 5.5h5.5M8 5.5v6"/>'),
  "file-yaml": (S, '<path d="M2.5 3.75h7M5.25 8h8.25M5.25 12.25h5.5"/><path d="M2.5 8h.25M2.5 12.25h.25"/>'),
  "file-python": ("raw", F + '<path fill-rule="evenodd" d="M7.85 1.25c-2.6 0-3.35 1.05-3.35 2.4V5.4h3.6v.6H3.15C1.9 6 1.25 6.95 1.25 8.6c0 1.7.75 2.6 1.9 2.6h1.2V9.6c0-1.25.95-2.2 2.2-2.2h3.4c1.05 0 1.85-.85 1.85-1.9V3.65c0-1.3-1.2-2.4-3.95-2.4zM6.35 2.4a.65.65 0 1 1 0 1.3.65.65 0 0 1 0-1.3z"/><path fill-rule="evenodd" d="M8.15 14.75c2.6 0 3.35-1.05 3.35-2.4V10.6H7.9V10h4.95c1.25 0 1.9-.95 1.9-2.6 0-1.7-.75-2.6-1.9-2.6h-1.2v1.6c0 1.25-.95 2.2-2.2 2.2H6.05c-1.05 0-1.85.85-1.85 1.9v1.85c0 1.3 1.2 2.4 3.95 2.4zM9.65 13.6a.65.65 0 1 1 0-1.3.65.65 0 0 1 0 1.3z"/>'),
  "file-go": (S, '<path d="M7.1 5.9A2.75 2.75 0 0 0 4.75 4.75a3.25 3.25 0 0 0 0 6.5c1.65 0 2.6-1.1 2.6-2.85H5.1"/><circle cx="11.6" cy="8" r="2.75"/>'),
  "file-shell": (S, '<path d="M2.75 4.25 6.5 8l-3.75 3.75M8.75 12h4.5"/>'),
  "file-swift": (F, '<path d="M13.95 10.6c.95-3.05-.6-6.5-3.75-8.4 1.45 2.15 1.85 4.6 1.05 6.6-2.2-1.45-4.95-3.65-6.9-5.85 1.2 1.85 2.65 3.45 4.15 4.75-2.15-1.15-4.4-2.65-6.2-4.3 2.3 3.3 5.5 6 8.55 7.45-2.45 1.3-5.9 1.15-9.05-.8 1.85 2.35 4.65 3.8 7.35 3.8 1.3 0 2.35-.35 3.2-1 .65-.05 1.2.2 1.6.8.1-.8-.25-1.55-1-2.05z"/>'),
  "file-c": (S, '<path d="M8 1.6 13.55 4.8v6.4L8 14.4 2.45 11.2V4.8z" stroke-width="1.3"/><path d="M10.1 6.35a2.65 2.65 0 1 0 0 3.3"/>'),
  "file-html": (S, '<path d="M4.75 4.5 1.5 8l3.25 3.5M11.25 4.5 14.5 8l-3.25 3.5M9.25 3 6.75 13"/>'),
  "file-css": (S, '<path d="M6.75 2.75 5.5 13.25M10.75 2.75 9.5 13.25M3 5.75h10.5M2.5 10.25H13"/>'),
  "file-lock": (S, '<rect x="3" y="7" width="10" height="7.25" rx="1.75"/><path d="M5.25 7V5a2.75 2.75 0 0 1 5.5 0v2"/><path d="M8 9.75v1.75"/>'),
  "file-git": ("raw", knockout(
      '<path d="M6.94 1.69a1.5 1.5 0 0 1 2.12 0l5.25 5.25a1.5 1.5 0 0 1 0 2.12l-5.25 5.25a1.5 1.5 0 0 1-2.12 0L1.69 9.06a1.5 1.5 0 0 1 0-2.12z"/>',
      '<path d="M5.4 4.4 8 7v4.1M8 7l2.6 2.6"/><circle cx="8" cy="11.1" r=".9" fill="#000"/><circle cx="10.6" cy="9.6" r=".9" fill="#000"/><circle cx="8" cy="7" r=".9" fill="#000"/>', "1.25")),
  "file-image": (S, '<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2"/><circle cx="5.75" cy="6.25" r="1.25"/><path d="M2.5 12.25 6.25 8.5l2.25 2.25 2-2.25 3.25 3.25"/>'),
  "file-config": (S, '<path d="M2.5 4h3.25M9.25 4h4.25M2.5 8h7.25M13.25 8h.25M2.5 12h1.25M7.25 12h6.25"/><circle cx="7.5" cy="4" r="1.6"/><circle cx="11.5" cy="8" r="1.6"/><circle cx="5.5" cy="12" r="1.6"/>'),
  "file-docker": (F, '<path d="M3.25 6.25h2v2h-2zM5.75 6.25h2v2h-2zM8.25 6.25h2v2h-2zM5.75 3.75h2v2h-2zM8.25 3.75h2v2h-2zM8.25 1.25h2v2h-2z"/><path d="M1.25 9h11.5c.7-1.05 1.85-1.4 2.75-1.05-.3 1-1.1 1.7-2.1 1.8-.95 2.6-3.35 4.5-6.9 4.5-3 0-4.75-1.85-5.25-5.25z"/>'),
  "file-license": (S, '<circle cx="8" cy="6.25" r="4"/><path d="M5.6 9.45 4.75 14.25 8 12.75l3.25 1.5-.85-4.8"/><path d="M6.5 6.25 7.5 7.25l2-2"/>'),
  "file-package": (S, '<path d="M8 1.6 13.75 4.6v6.8L8 14.4 2.25 11.4V4.6z"/><path d="M2.4 4.7 8 7.6l5.6-2.9M8 7.6v6.65M5.1 3.1l5.75 3"/>'),
  "file-archive": (S, DOC + '<path d="M6.5 2.6h1.25M7.75 4.1H9M6.5 5.6h1.25M7.75 7.1H9" stroke-width="1.25"/><rect x="6.6" y="8.75" width="2.8" height="3" rx=".8" stroke-width="1.25"/>'),
}

os.makedirs(OUT, exist_ok=True)
for name, (style, body) in ICONS.items():
    if style == "raw":
        svg = body + "\n</svg>\n" if not body.rstrip().endswith("</svg>") else body
    else:
        svg = f"{style}\n{body}\n</svg>\n"
    with open(os.path.join(OUT, name + ".svg"), "w") as f:
        f.write(svg)
print(len(ICONS), "icons")
