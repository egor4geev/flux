#!/bin/bash
# Runs a UI scenario with screenshots, for agents that can't press keys.
#
#   FLUX_SCENARIO="type:let space type:x cmd-s shot:saved" [FLUX_ANSWERS=1] \
#       scripts/ui-scenario.sh <screenshot-dir> [flux args...]
#
# Builds flux with the `scenario` feature, launches it, takes a screenshot of every window of the
# process at each `SHOT name` marker into <dir>/<name>-<window id>.png and stops the process at
# `END` (or after FLUX_SCENARIO_TIMEOUT, 120 s by default). The flux output goes to
# <dir>/stdout.log and stderr.log. FLUX_SCENARIO_BACKGROUND=1 — neither flux nor this script
# activates the app: the keyboard stays with the app the user works in. FLUX_SCENARIO_HEADLESS=1 (set
# by itself when the screen is locked) — flux draws its frames on a timer and saves the shots itself,
# rendered offscreen, with the same names. Scenario steps and dialog auto-answers are described in
# crates/flux-app/src/scenario.rs.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

if [ $# -lt 1 ]; then
    sed -n '2,10p' "$0"
    exit 2
fi
ROOT=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$1"
OUT=$(cd "$1" && pwd)
shift
cd "$ROOT"
cargo build --quiet -p flux-app --features scenario

# A Swift helper (included in the Command Line Tools): the window ids of the process and app
# activation.
TOOL="$ROOT/target/scenario-tools/window-tool"
if [ ! -x "$TOOL" ] || [ "$ROOT/scripts/window-tool.swift" -nt "$TOOL" ]; then
    mkdir -p "$(dirname "$TOOL")"
    swiftc -O -o "$TOOL" "$ROOT/scripts/window-tool.swift"
fi

# A locked screen (the author is away): gpui stops drawing the windows and the window server won't
# capture them — flux draws and saves the shots itself.
if [ -z "${FLUX_SCENARIO_HEADLESS:-}" ] &&
    ioreg -n Root -d1 -a 2>/dev/null | grep -A1 CGSSessionScreenIsLocked | grep -q '<true/>'; then
    export FLUX_SCENARIO_HEADLESS=1
fi
export FLUX_SCENARIO_OUT="$OUT"

LOG="$OUT/stdout.log"
./target/debug/flux "$@" >"$LOG" 2>"$OUT/stderr.log" &
PID=$!
trap 'kill "$PID" 2>/dev/null || true' EXIT
sleep 1
# FLUX_SCENARIO_BACKGROUND=1: the app isn't activated (the user keeps typing in their own app).
if [ -z "${FLUX_SCENARIO_BACKGROUND:-}" ]; then
    "$TOOL" activate "$PID"
fi

seen=0
deadline=$((SECONDS + ${FLUX_SCENARIO_TIMEOUT:-120}))
while kill -0 "$PID" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do
    names=$(sed -n 's/^SHOT //p' "$LOG")
    count=$(printf '%s' "$names" | grep -c . || true)
    while [ "$seen" -lt "$count" ]; do
        seen=$((seen + 1))
        name=$(printf '%s\n' "$names" | sed -n "${seen}p")
        # Headless: flux saves the shot itself.
        [ -n "${FLUX_SCENARIO_HEADLESS:-}" ] && continue
        sleep 0.5 # the frame has time to finish drawing
        for id in $("$TOOL" windows "$PID"); do
            screencapture -x -o -l "$id" "$OUT/$name-$id.png"
        done
    done
    grep -q '^END' "$LOG" && break
    sleep 0.2
done

# Headless: the last shot is saved half a second after its marker.
[ -n "${FLUX_SCENARIO_HEADLESS:-}" ] && sleep 1
if kill -0 "$PID" 2>/dev/null; then
    kill "$PID"
    echo "flux: stopped after the scenario"
else
    wait "$PID" && echo "flux: exited with 0" || echo "flux: exited with $?"
fi
echo "--- stdout"
cat "$LOG"
echo "--- stderr"
cat "$OUT/stderr.log"
echo "--- shots: $OUT"
