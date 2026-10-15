# Contributing to Flux

Thanks for helping make Flux better! Here is where things go and how the work is organized.

## Reporting a bug

In Flux, **Help → Report a Bug…** opens the [bug form](https://github.com/egor4geev/flux/issues/new?template=bug_report.yml)
with the version, macOS, the processor and your plugins already filled in. Please look through the
[existing issues](https://github.com/egor4geev/flux/issues) first, and keep to one problem per issue.

- **After a crash**, Flux offers to report it the next time it starts, with the crash report in the
  form.
- **The log**: Help → Show Log in Finder opens the folder with `flux.log`. The lines from around the
  problem help a lot.
- **A plugin from the Marketplace** — a language, a theme, an icon set, Yandex Tracker & Calendar —
  has its own [issues in flux-plugins](https://github.com/egor4geev/flux-plugins/issues/new/choose).
- **A security problem**: privately, please, as [SECURITY.md](SECURITY.md) describes — not in a
  public issue.

## Ideas and questions

They live in [Discussions](https://github.com/egor4geev/flux/discussions):
[Ideas](https://github.com/egor4geev/flux/discussions/categories/ideas) (Help → Suggest a Feature…)
and [Q&A](https://github.com/egor4geev/flux/discussions/categories/q-a) (Help → Ask a Question…). An
upvote on an idea that's already there counts too.

## Branches

| Branch | What it holds |
|---|---|
| `pre-release` | Development: everything new lands here, and beta builds come from it. **Pull requests go here.** |
| `main` | The released version. It changes only through a pull request from `pre-release` (a release) or from a `hotfix/*` branch. |
| `hotfix/<name>` | An urgent fix of the released version: it starts from `main` and goes back into `main`. |

After every change of `main`, a workflow merges `main` into `pre-release`, so a hotfix is in the next
release too.

## Pull requests

- **Base branch `pre-release`.** A pull request into `main` from any other branch fails the
  `PR source` check: change its base with **Edit** next to the title.
- Keep it to one change, and say what changes for someone using Flux and why; link the issue or the
  discussion ("Fixes #123"). For a bigger change, please start a discussion or an issue first.
- Before sending: `cargo fmt --all`, `cargo test --workspace` passes and
  `cargo clippy --workspace --all-targets -- -D warnings` is clean. CI checks the same on every pull
  request (**Format** and **Clippy and tests (macOS)**); a release or a hotfix merges into `main`
  only when they pass. For a change in the interface, add screenshots — before and after.

## Building

Flux runs on macOS. You need:

- [Rust](https://rustup.rs) (stable) and the target of the built-in plugins:
  `rustup target add wasm32-wasip2` (without it Flux still builds, just without those plugins);
- [CMake](https://cmake.org): `brew install cmake`;
- the Xcode Command Line Tools: `xcode-select --install`.

```sh
cargo run -p flux-app -- ~/some/project       # Flux on a project (a debug build)
cargo test --workspace                        # the tests
cargo clippy --workspace --all-targets        # should be clean
cargo fmt --all                               # the format CI checks
scripts/bundle-macos.sh                       # target/release/Flux.app
```

The first build takes a while: tree-sitter's grammars are C code.

## Code

- The workspace: `flux-core` (text, selections, undo), `flux-syntax` (tree-sitter), `flux-search`,
  `flux-fs`, `flux-git`, `flux-lsp`, `flux-term`, `flux-plugin` and `flux-plugin-api` (plugins on
  wasmtime and their SDK), `flux-claude` (Claude Code), and `flux-app` — the editor itself, on
  [gpui](https://www.gpui.rs). gpui comes with a small patch of Flux's: `vendor/gpui`.
- Code, comments and docs are in English. Every string the interface shows goes through `tr`,
  `trf` or `trn` (`crates/flux-app/src/i18n`) and gets its Russian translation in
  `crates/flux-app/src/i18n/ru/` — a test checks that none is missing.
- The code is formatted by rustfmt with its defaults: run `cargo fmt --all` before committing (CI
  fails otherwise). The commit that formatted everything at once is in `.git-blame-ignore-revs`:
  `git config blame.ignoreRevsFile .git-blame-ignore-revs` makes `git blame` look past it.
- Shortcuts: look through the existing keymap before picking one, and keep the IDE conventions it
  follows (⌘B goes to a definition, ⌥↵ shows the context actions, ⌘K commits).
- Plugins: [`docs/plugins.md`](docs/plugins.md). Plugins for everyone go to the
  [catalog](https://github.com/egor4geev/flux-plugins).

## License

By contributing, you agree that your work is licensed under the [MIT license](LICENSE), as Flux is.

## Releases and hotfixes (for the maintainer)

- **A release**: a pull request `pre-release → main`, merged with a merge commit (the only method
  `main` allows: `pre-release`'s commits stay as they are, and the two branches don't drift apart).
- **A hotfix**: `git switch -c hotfix/<name> origin/main`, a pull request into `main`; once it's
  merged, `main` comes into `pre-release` by itself — on a conflict the `Sync pre-release` workflow
  fails and says what to merge by hand.
- The repository's settings and its branch rules (`.github/rulesets/`): `scripts/github-setup.sh`.

### Cutting a release

A tag publishes a release (`.github/workflows/release.yml`): `vX.Y.Z` on `main` — a stable one,
`vX.Y.Z-beta.N` on `pre-release` — a beta (a pre-release on GitHub, for the Beta channel). CI builds
Flux for Apple silicon and Intel, signs it with the "Flux Dev" certificate, packs a `.dmg` (the
first install) and a `.zip` (the update Flux downloads itself) and writes `latest.json` — the
release for Flux's updater, each archive signed with the update key.

1. On `pre-release`: the version in `Cargo.toml` (`[workspace.package]`, then `cargo check` to update
   `Cargo.lock`), the release notes in `docs/release-notes/<version>.md` (Markdown, English — the
   release's page and Flux's update notification show them) and, for a new minor version, What's New
   in `crates/flux-app/assets/whats-new/<version>.toml` with the release date.
2. **A beta**: tag the commit on `pre-release` — `git tag v0.2.0-beta.1 && git push origin
   v0.2.0-beta.1`.
3. **A stable release**: a pull request `pre-release → main`. Besides CI it gets a dry run of the
   release (the `Release` workflow): the `.dmg`, the `.zip` and `latest.json` as the run's artifacts —
   try them before merging. Merge it, then tag the merge commit on `main`:
   `git switch main && git pull && git tag v0.2.0 && git push origin v0.2.0`.
4. The workflow checks that the tag matches `Cargo.toml`'s version, sits on its branch and has
   release notes, then publishes the release. Flux finds it at its next check (on start and once a
   day; Flux → Check for Updates… at once).

A dry run of any branch: Actions → Release → Run workflow. The secrets the workflow needs:
`MACOS_CERT_P12` (the `.p12` of "Flux Dev", base64), `MACOS_CERT_PASSWORD`,
`FLUX_UPDATE_PRIVATE_KEY` (the update key's seed, base64; its public half is built into Flux —
`crates/flux-update`). Updates without GitHub, on this Mac: `scripts/update-test.sh`.
