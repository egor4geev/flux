#!/bin/bash
# Starts Flux as on its very first launch, so the welcome guide opens (part 10.7): every file of its
# state — settings, recent projects, open projects, Claude sessions, plugins and their secrets, the
# last version that ran, logs — goes to a fresh profile folder, and the real ones aren't touched.
#
#   scripts/first-run.sh [Flux.app | flux binary] [profile folder]
#
# Without arguments: target/release/Flux.app and a new folder under $TMPDIR. Run it again with the
# same profile folder to see a second launch (the guide doesn't come back, What's New is checked).
# Flux opens as a separate app instance (`open -n`), next to a Flux that may already be running.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP=${1:-"$ROOT/target/release/Flux.app"}
PROFILE=${2:-$(mktemp -d "${TMPDIR:-/tmp}/flux-first-run.XXXXXX")}
mkdir -p "$PROFILE"
PROFILE=$(cd "$PROFILE" && pwd)

ENV=(
    "FLUX_SETTINGS_FILE=$PROFILE/settings.json"
    "FLUX_RECENT_FILE=$PROFILE/recent-projects"
    "FLUX_PROJECTS_FILE=$PROFILE/projects.json"
    "FLUX_CLAUDE_SESSIONS_FILE=$PROFILE/claude-sessions.json"
    "FLUX_COMMAND_USAGE_FILE=$PROFILE/command-usage.json"
    "FLUX_CONTEXTS_FILE=$PROFILE/contexts.json"
    "FLUX_GIT_FAVORITES_FILE=$PROFILE/git-favorites.json"
    "FLUX_PLUGINS_HOME=$PROFILE/plugins"
    "FLUX_PLUGIN_SECRETS_FILE=$PROFILE/plugins/secrets.json"
    "FLUX_LAST_VERSION_FILE=$PROFILE/last-version"
    "FLUX_LOG_DIR=$PROFILE/logs"
)

echo "Profile: $PROFILE"
if [[ "$APP" == *.app ]]; then
    args=()
    for pair in "${ENV[@]}"; do
        args+=(--env "$pair")
    done
    open -n "${args[@]}" "$APP"
else
    env "${ENV[@]}" "$APP" &
    disown
fi
