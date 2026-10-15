#!/bin/bash
# The settings of Flux's repository on GitHub that its files can't hold (part 10.6). Idempotent: it
# changes only what differs, so it can run again at any time.
#
#   scripts/github-setup.sh            # a dry run: what would change
#   scripts/github-setup.sh --apply    # change it
#
# - Pull requests: merge commits (releases from pre-release into main keep its commits) and squash
#   (contributions into pre-release); no rebase merges. Head branches aren't deleted on merge:
#   pre-release is one. A merge commit takes the pull request's title and description.
# - Discussions on (bugs go to Issues, ideas and questions to Discussions); private vulnerability
#   reporting on (SECURITY.md); the description and the topics.
# - The rulesets of .github/rulesets/*.json, created or updated by name. main's required checks come
#   from workflows: "PR source" (.github/workflows/pr-source.yml) runs on pull_request_target, so it
#   runs as main has it; "Format" and "Clippy and tests (macOS)" (ci.yml, part 10.8) run on
#   pull_request, as the pull request's branch has them — a hotfix branch starts from main. Until a
#   check's workflow is on main, a pull request could wait for the check forever: then main's
#   ruleset is set without that check, and the script asks to run it again once the first pull
#   request into main is merged.
#
# Needs gh, logged in as an admin of the repository (the repo scope), and jq.
set -euo pipefail

REPO=${FLUX_GITHUB_REPO:-egor4geev/flux}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
# The README's one line.
DESCRIPTION="A fast, native code editor for macOS — a complete Git workflow and Claude Code, built in."
TOPICS='["code-editor","editor","ide","macos","rust","gpui","tree-sitter","lsp","git","claude-code"]'
# The workflow that reports a required check of main.
check_workflow() {
    case "$1" in
        "PR source") echo .github/workflows/pr-source.yml ;;
        "Format" | "Clippy and tests ("*) echo .github/workflows/ci.yml ;;
        *) echo "" ;;
    esac
}

case "${1:-}" in
    --apply) APPLY=true ;;
    "") APPLY=false ;;
    *)
        echo "usage: $0 [--apply]" >&2
        exit 2
        ;;
esac

for tool in gh jq; do
    command -v "$tool" >/dev/null || {
        echo "$tool is needed" >&2
        exit 1
    }
done
if [ "$(gh api "repos/$REPO" --jq .permissions.admin)" != true ]; then
    echo "gh's account isn't an admin of $REPO (gh auth status)" >&2
    exit 1
fi

if $APPLY; then
    echo "$REPO: applying"
else
    echo "$REPO: a dry run (--apply changes it)"
fi
changes=0

# Prints what changes; with --apply, runs the command after it.
change() {
    local what=$1
    shift
    changes=$((changes + 1))
    echo "  → $what"
    if $APPLY; then
        "$@" >/dev/null
    fi
}

# --- Settings ---
echo "Settings"
wanted=$(jq -n --arg description "$DESCRIPTION" '{
    allow_merge_commit: true,
    allow_squash_merge: true,
    allow_rebase_merge: false,
    delete_branch_on_merge: false,
    merge_commit_title: "PR_TITLE",
    merge_commit_message: "PR_BODY",
    squash_merge_commit_title: "PR_TITLE",
    squash_merge_commit_message: "COMMIT_MESSAGES",
    has_discussions: true,
    description: $description
}')
current=$(gh api "repos/$REPO")
# Only the keys that differ: "key: now → wanted" lines and the object to send. A merge commit's
# title and message go together (GitHub wants the title with the message), as do a squash's.
diff=$(jq -n --argjson current "$current" --argjson wanted "$wanted" '
    ($wanted | with_entries(select(.value != $current[.key]))) as $diff
    | reduce (["merge_commit_title", "merge_commit_message"],
        ["squash_merge_commit_title", "squash_merge_commit_message"]) as $pair
        ($diff; if any($pair[]; in($diff)) then . + ($wanted | with_entries(select(.key | IN($pair[]))))
            else . end)')
if [ "$diff" = "{}" ]; then
    echo "  as they should be"
else
    jq -r --argjson current "$current" 'to_entries[] | "  \(.key): " + if $current[.key] == .value
        then "\(.value | tojson) (unchanged, goes with its pair)"
        else "\($current[.key] | tojson) → \(.value | tojson)" end' <<<"$diff"
    change "update the settings" gh api -X PATCH "repos/$REPO" --input - <<<"$diff"
fi

# --- Topics ---
echo "Topics"
current=$(gh api "repos/$REPO/topics" | jq -c '.names | sort')
wanted=$(jq -c 'sort' <<<"$TOPICS")
if [ "$current" = "$wanted" ]; then
    echo "  as they should be"
else
    echo "  $current → $wanted"
    change "set the topics" gh api -X PUT "repos/$REPO/topics" --input - <<<"{\"names\": $wanted}"
fi

# --- Private vulnerability reporting ---
echo "Private vulnerability reporting"
if [ "$(gh api "repos/$REPO/private-vulnerability-reporting" --jq .enabled)" = true ]; then
    echo "  on"
else
    echo "  off → on"
    change "turn it on" gh api -X PUT "repos/$REPO/private-vulnerability-reporting"
fi

# --- Rulesets ---
# A ruleset as GitHub returns it, cut down to what the file says: the rules of the file's types with
# the parameters the file names, the conditions, the bypass list, the enforcement. Equal to the file
# (put through `wanted_ruleset`) when nothing needs changing.
current_ruleset() { # $1: the ruleset's JSON from GitHub, $2: the file's JSON
    jq -n --argjson current "$1" --argjson wanted "$2" '{
        target: $current.target,
        enforcement: $current.enforcement,
        conditions: $current.conditions,
        bypass_actors: [$current.bypass_actors[]? | {actor_type, bypass_mode}
            + (if .actor_type == "DeployKey" then {} else {actor_id} end)] | sort_by(.actor_type),
        rules: [($current.rules // [])[] as $rule
            | ($wanted.rules | map(select(.type == $rule.type)) | first) as $want
            | {type: $rule.type}
            + (if $want.parameters then {parameters: ($rule.parameters // {}
                | with_entries(select(.key as $key | $want.parameters | has($key))))} else {} end)]
            | sort_by(.type)
    }'
}
wanted_ruleset() { # $1: the file's JSON
    jq '{target, enforcement, conditions,
        bypass_actors: [.bypass_actors[]? | {actor_type, bypass_mode}
            + (if .actor_type == "DeployKey" then {} else {actor_id} end)] | sort_by(.actor_type),
        rules: [.rules[] | {type} + (if .parameters then {parameters} else {} end)] | sort_by(.type)}' \
        <<<"$1"
}

existing=$(gh api "repos/$REPO/rulesets?per_page=100" --paginate --jq '.[] | {id, name}' | jq -s .)
for file in "$ROOT"/.github/rulesets/*.json; do
    ruleset=$(jq . "$file")
    name=$(jq -r .name <<<"$ruleset")
    echo "Ruleset \"$name\" (${file#"$ROOT"/})"
    # The required checks whose workflow isn't on main yet: left out for now.
    missing=()
    while IFS= read -r check; do
        workflow=$(check_workflow "$check")
        if [ -n "$workflow" ] && ! gh api "repos/$REPO/contents/$workflow?ref=main" --silent 2>/dev/null; then
            missing+=("$check")
        fi
    done < <(jq -r '.rules[] | select(.type == "required_status_checks")
        | .parameters.required_status_checks[].context' <<<"$ruleset")
    for check in ${missing[@]+"${missing[@]}"}; do
        echo "  without the required check \"$check\" for now: $(check_workflow "$check") isn't on"
        echo "  main yet, so the check could never come. Run this script again once the first pull"
        echo "  request into main is merged."
        ruleset=$(jq --arg check "$check" '.rules |= map(
            if .type == "required_status_checks" then
                .parameters.required_status_checks |= map(select(.context != $check))
            else . end
            | select(.type != "required_status_checks"
                or (.parameters.required_status_checks | length) > 0))' <<<"$ruleset")
    done
    id=$(jq -r --arg name "$name" 'map(select(.name == $name)) | first | .id // empty' <<<"$existing")
    if [ -z "$id" ]; then
        change "create it" gh api -X POST "repos/$REPO/rulesets" --input - <<<"$ruleset"
        continue
    fi
    # Keys sorted: GitHub returns them in its own order.
    now=$(current_ruleset "$(gh api "repos/$REPO/rulesets/$id")" "$ruleset" | jq -S .)
    want=$(wanted_ruleset "$ruleset" | jq -S .)
    if [ "$now" = "$want" ]; then
        echo "  as it should be"
    else
        diff <(echo "$now") <(echo "$want") | sed 's/^/  /' || true
        change "update it (ruleset $id)" gh api -X PUT "repos/$REPO/rulesets/$id" --input - <<<"$ruleset"
    fi
done

if [ "$changes" -eq 0 ]; then
    echo "Nothing to change."
elif ! $APPLY; then
    echo "$changes change(s): run with --apply."
fi
