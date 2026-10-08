# For agents

Project documentation lives in the Obsidian wiki `../flux-wiki` (`~/dev/personal/flux-dev/flux-wiki`),
written in Russian. Before working, read `Agent Guide.md`, `Roadmap.md` and the latest entries of
`Journal.md`; after working, update the journal, the roadmap and the affected arc42 sections as
`Agent Guide.md` describes. Any UI work follows the design system (`design/Design System.md`).

Code, comments and docs in this repository are in English. The UI is localized (English, Russian):
user-visible strings go through `crate::i18n::tr`/`trf`/`trn`, with Russian translations in
`crates/flux-app/src/i18n/ru/`.

Rust is installed via Homebrew rustup (keg-only): in a non-interactive shell run
`export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` first. `ls` may be aliased to `eza`, which can
hang — use `/bin/ls`.
