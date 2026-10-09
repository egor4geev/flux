#!/bin/sh
# A git sandbox for checking Flux's Git features by hand or in UI scenarios (branches, sync, stash,
# conflicts — stage 6.2).
#   scripts/git-sandbox.sh <dir>
# Makes <dir>/repo (the project to open in Flux) and <dir>/remote.git (its origin), plus
# <dir>/other (a second clone that pushed commits the repo hasn't fetched).
#
# State of <dir>/repo:
#   main            — 4 commits; 1 local commit not pushed; origin/main has 1 commit main hasn't fetched
#                     (after Fetch: ↓1 ↑1 — diverged, Update Project merges or rebases)
#   feature/login   — 2 commits on top of main's 3rd, pushed (origin/feature/login), touches login.rs only:
#                     merges into main cleanly
#   feature/conflict— changes the greeting line of src/main.rs that main also changed: merge/rebase conflict
#   feature/rebase  — 2 commits on main's 3rd: the greeting line (conflicts when rebased onto main), util.rs
#   bugfix/typo     — local only, 1 commit (README)
#   origin/feature/remote-only — exists only on the remote (checkout creates a tracking branch)
#   tag v0.1        — on main's 2nd commit
#   stash@{0}       — "Experiment with the parser" (src/parser.rs) + an untracked file (notes/idea.md)
#   working tree    — src/main.rs modified on the greeting line (checkout of feature/conflict needs Smart Checkout),
#                     src/util.rs modified elsewhere
set -e
DIR="$1"
[ -n "$DIR" ] || { echo "usage: $0 <dir>"; exit 1; }
rm -rf "$DIR"
mkdir -p "$DIR"
cd "$DIR"
DIR=$(pwd -P)
git init -q --bare -b main remote.git
git init -q -b main repo
cd repo
git config user.name "Flux Sandbox"
git config user.email "sandbox@flux.dev"
git config commit.gpgsign false
git remote add origin "$DIR/remote.git"

c() { # c "message" — commit everything with a fixed, increasing date
  N=$((${N:-0} + 1))
  GIT_AUTHOR_DATE="2026-10-0${N}T10:00:00" GIT_COMMITTER_DATE="2026-10-0${N}T10:00:00" \
    git commit -q -m "$1"
}

mkdir -p src
cat > src/main.rs <<'EOF'
mod parser;
mod util;

fn main() {
    let greeting = "Hello";
    println!("{greeting}, world!");
    let words = parser::words("one two three");
    println!("{} words", words.len());
    util::report(&words);
}
EOF
cat > src/parser.rs <<'EOF'
/// Splits a text into words.
pub fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}
EOF
cat > src/util.rs <<'EOF'
/// Prints every word on its own line.
pub fn report(words: &[&str]) {
    for word in words {
        println!("- {word}");
    }
}
EOF
cat > README.md <<'EOF'
# Sandbox

A small project to try Git in Flux: branches, stash, conflicts.
EOF
printf 'target/\n' > .gitignore
cat > Cargo.toml <<'EOF'
[package]
name = "sandbox"
version = "0.1.0"
edition = "2021"
EOF
git add -A && c "Word counter"

printf '\nRun it with `cargo run`.\n' >> README.md
git add -A && c "Usage in README"
git tag v0.1

cat > src/parser.rs <<'EOF'
/// Splits a text into words.
pub fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Counts the lines of a text.
pub fn lines(text: &str) -> usize {
    text.lines().count()
}
EOF
git add -A && c "Line counting"
BASE=$(git rev-parse HEAD)

# feature/login: on top of the 3rd commit, its own file only.
git switch -q -c feature/login
cat > src/login.rs <<'EOF'
/// Checks a password.
pub fn check(password: &str) -> bool {
    password.len() >= 8
}
EOF
git add -A && c "Login check"
printf '\n/// The user name for a greeting.\npub fn user() -> &'"'"'static str {\n    "guest"\n}\n' >> src/login.rs
git add -A && c "Guest user"
git push -q -u origin feature/login 2>/dev/null

# feature/conflict: changes the greeting line.
git switch -q -c feature/conflict "$BASE"
sed -i '' 's/let greeting = "Hello";/let greeting = "Hi there";/' src/main.rs
git add -A && c "Friendlier greeting"

# feature/rebase: two commits on the base — the first changes the greeting line (a conflict when
# rebased onto main), the second the bullets of util.rs.
git switch -q -c feature/rebase "$BASE"
sed -i '' 's/let greeting = "Hello";/let greeting = "Howdy";/' src/main.rs
git add -A && c "Casual greeting"
sed -i '' 's/- {word}/* {word}/' src/util.rs
git add -A && c "Star bullets"

# bugfix/typo: local only.
git switch -q -c bugfix/typo "$BASE"
sed -i '' 's/A small project/A tiny project/' README.md
git add -A && c "Fix the README wording"

# main: changes the same greeting line, pushed; then one more local commit.
git switch -q main
sed -i '' 's/let greeting = "Hello";/let greeting = "Good morning";/' src/main.rs
git add -A && c "Morning greeting"
git push -q -u origin main 2>/dev/null

# Another clone pushes to main and creates a remote-only branch.
git clone -q "$DIR/remote.git" "$DIR/other" 2>/dev/null
(
  cd "$DIR/other"
  git config user.name "Teammate"
  git config user.email "teammate@flux.dev"
  git config commit.gpgsign false
  printf '\n## Contributing\n\nSend a pull request.\n' >> README.md
  git add -A
  GIT_AUTHOR_DATE="2026-10-08T12:00:00" GIT_COMMITTER_DATE="2026-10-08T12:00:00" git commit -q -m "Contributing section"
  git push -q origin main 2>/dev/null
  git switch -q -c feature/remote-only
  printf '/// Shouts a word.\npub fn shout(word: &str) -> String {\n    word.to_uppercase()\n}\n' > src/shout.rs
  git add -A
  GIT_AUTHOR_DATE="2026-10-08T13:00:00" GIT_COMMITTER_DATE="2026-10-08T13:00:00" git commit -q -m "Shouting"
  git push -q origin feature/remote-only 2>/dev/null
)

# A local commit on main, not pushed (main ↑1; ↓1 after a fetch).
N=7
printf '\n/// Counts the characters of a text.\npub fn chars(text: &str) -> usize {\n    text.chars().count()\n}\n' >> src/parser.rs
git add -A && c "Character counting"
# Recent branches in the reflog: feature/login, then bugfix/typo, back to main.
git switch -q feature/login
git switch -q bugfix/typo
git switch -q main

# A stash with a tracked change and an untracked file.
printf '\n/// Experimental: words in reverse.\npub fn reversed(text: &str) -> Vec<&str> {\n    let mut words = words(text);\n    words.reverse();\n    words\n}\n' >> src/parser.rs
mkdir -p notes
printf '# Idea\n\nReverse the words.\n' > notes/idea.md
git stash push -q -u -m "Experiment with the parser"

# Working tree: the greeting line changed (Smart Checkout to feature/conflict), and another file.
sed -i '' 's/let greeting = "Good morning";/let greeting = "Good evening";/' src/main.rs
sed -i '' 's/- {word}/• {word}/' src/util.rs
echo "sandbox: $DIR/repo"
git -C "$DIR/repo" log --oneline --all --graph | head -20
git -C "$DIR/repo" status --short
git -C "$DIR/repo" stash list
