# Security

## Reporting a vulnerability

Please don't open a public issue. Report it privately instead:
**[Report a vulnerability](https://github.com/egor4geev/flux/security/advisories/new)** — or the
**Security** tab of the repository → **Report a vulnerability**. Only the maintainer sees the report,
and the conversation about it stays private until the fix is out; the advisory published with the
fix credits you, unless you'd rather not be named.

What helps: the Flux version (Flux → About Flux → Copy), the steps or a proof of concept, and what an
attacker gains.

## What counts

- **The plugin sandbox.** A plugin reaching beyond the permissions it was installed with: files
  outside its project or its folder, hosts its `network` permission doesn't name, programs or
  terminals it may not run, the data, settings or secrets of another plugin.
- **Secrets.** Plugins' tokens and passwords, kept in the macOS keychain, getting out — to another
  plugin, to `settings.json`, to a log.
- **The Marketplace.** Flux installing a package whose SHA-256 differs from the catalog's index, or
  from somewhere else than the catalog says.
- **Flux itself.** Opening a project, a file or a link running code you didn't ask for — through a
  terminal link, a Markdown preview, a language server Flux installs, a project's settings.
- **Updates**, once Flux updates itself: an update that isn't signed by Flux's key, or an older
  version, being installed.

What doesn't: what a plugin does with the permissions you gave it; what Claude Code does with the
permissions you gave it (Claude Code's own problems go to [Anthropic](https://github.com/anthropics/claude-code/security));
an attacker who already runs code as you on your Mac.

## Supported versions

Flux is in beta: fixes go into the latest release and the next beta.

Plugins of the catalog have their own repository:
[flux-plugins](https://github.com/egor4geev/flux-plugins/security/advisories/new) takes the reports
about them the same way.
