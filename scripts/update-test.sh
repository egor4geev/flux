#!/bin/bash
# A manual check of updates over the air (part 10.8), without GitHub and without touching the real
# Flux: an "old" Flux (0.1.0) finds a "new" one (0.1.1) on a local server, downloads it, checks it
# and replaces itself.
#
#   scripts/update-test.sh [old version] [new version]
#
# Both are release builds of this checkout (target/update-test, apart from target/release), signed
# with "Flux Dev" like every build of the author's (the new bundle must satisfy the old one's
# signature requirement), with a test key pair for the archives' signatures instead of the real
# one (FLUX_UPDATE_PUBLIC_KEY at build time). The new Flux is packed as a release is
# (scripts/package-macos.sh) and described by a latest.json (flux-release manifest), served by
# `python3 -m http.server` on 127.0.0.1. The old Flux runs from a folder of its own (its
# "Applications") on a fresh profile, as scripts/first-run.sh does, with FLUX_UPDATE_FEED pointing
# at the server: the real settings, projects, plugins and the installed Flux aren't touched.
#
# Flux's helper reopens the new version on the same profile (it passes Flux's FLUX_* variables to
# `open --env`); this script only reports the swap. The replaced 0.1.0 goes to the Trash. The script
# keeps the server up until Ctrl-C (a second check then says Flux is up to date).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OLD=${1:-0.1.0}
NEW=${2:-0.1.1}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/flux-update-test.XXXXXX")
WORK=$(cd "$WORK" && pwd)
PROFILE=$WORK/profile
APPS=$WORK/Applications
FEED=$WORK/feed
mkdir -p "$PROFILE" "$APPS" "$FEED"
export CARGO_TARGET_DIR=$ROOT/target/update-test
cd "$ROOT"

echo "== flux-release and a test key pair"
cargo build --quiet --release -p flux-update --bin flux-release
RELEASE=$CARGO_TARGET_DIR/release/flux-release
KEYS=$("$RELEASE" keygen)
PRIVATE=$(sed -n 's/^private: //p' <<<"$KEYS")
PUBLIC=$(sed -n 's/^public: //p' <<<"$KEYS")
export FLUX_UPDATE_PUBLIC_KEY=$PUBLIC

echo "== Flux $NEW (the update)"
FLUX_SIGN_REQUIRED=1 FLUX_BUILD_VERSION=$NEW scripts/bundle-macos.sh --out "$WORK/new" >/dev/null
scripts/package-macos.sh "$WORK/new/Flux.app" "$FEED" >/dev/null

echo "== Flux $OLD (installed)"
FLUX_SIGN_REQUIRED=1 FLUX_BUILD_VERSION=$OLD scripts/bundle-macos.sh --out "$APPS" >/dev/null

case "$(uname -m)" in
    arm64) ARCH=aarch64 ;;
    *) ARCH=x86_64 ;;
esac
PORT=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
cat >"$WORK/notes.md" <<NOTES
A test update for the manual check of part 10.8.

- **Updates over the air**: this version came from a local server, checked by its SHA-256 and its
  Ed25519 signature.
- Flux $OLD was replaced while it was closed, and this one opened in its place.
NOTES
NAME=Flux-$NEW-macos-$ARCH
FLUX_UPDATE_PRIVATE_KEY=$PRIVATE "$RELEASE" manifest --version "$NEW" --date "$(date +%F)" \
    --notes "$WORK/notes.md" --base-url "http://127.0.0.1:$PORT" \
    --platform "macos-$ARCH=$FEED/$NAME.zip,$FEED/$NAME.dmg" >"$FEED/latest.json"

python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$FEED" >"$WORK/server.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT

# The profile of scripts/first-run.sh, past the first run: settings exist (no welcome guide) and the
# last version is the old one (no What's New).
echo '{}' >"$PROFILE/settings.json"
echo "$OLD" >"$PROFILE/last-version"
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
    "FLUX_UPDATES_FILE=$PROFILE/updates.json"
    "FLUX_UPDATE_FEED=http://127.0.0.1:$PORT/latest.json"
)
OPEN_ENV=()
for pair in "${ENV[@]}"; do
    OPEN_ENV+=(--env "$pair")
done

APP=$APPS/Flux.app
version_of() {
    /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$1/Contents/Info.plist" 2>/dev/null || true
}
running() {
    pgrep -f "^$APP/Contents/MacOS/flux" >/dev/null
}

echo
echo "Work folder: $WORK"
echo "Feed:        http://127.0.0.1:$PORT/latest.json (Flux $NEW)"
echo "Flux $OLD:   $APP"
open -n "${OPEN_ENV[@]}" "$APP"
echo
echo "Flux $OLD is open. Update it (the notification, or Flux → Check for Updates…), then Update and"
echo "Restart: Flux $NEW opens by itself once the swap is done. Ctrl-C stops the server."

# Wait for the swap: the bundle in place is the new version.
until [ "$(version_of "$APP")" = "$NEW" ]; do
    sleep 1
done
echo
echo "Swapped: $APP is Flux $(version_of "$APP") (log: $PROFILE/logs/update.log)."
for _ in $(seq 1 30); do
    running && break
    sleep 1
done
if running; then
    echo "Flux $NEW is running."
else
    echo "Flux $NEW didn't start within 30 s — see the log."
fi
echo "Ctrl-C stops the server."
wait "$SERVER"
