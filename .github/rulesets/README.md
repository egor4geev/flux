# Rulesets

The branch rules of this repository, as GitHub's ruleset JSON — `scripts/github-setup.sh` creates or
updates them by name (they can also be imported by hand: Settings → Rules → Rulesets → New ruleset →
Import a ruleset).

- `main.json` — the released version: no deletion, no force pushes, changes only through a pull
  request, merged with a merge commit (a release keeps the commits of `pre-release`, so the two
  branches' histories don't drift apart), and the required check `PR source`
  (`.github/workflows/pr-source.yml`): pull requests come only from `pre-release` or `hotfix/*`. No
  one bypasses it. `integration_id` 15368 is GitHub Actions: no one else can report that check.
- `pre-release.json` — development: no deletion, no force pushes; direct pushes are fine.
