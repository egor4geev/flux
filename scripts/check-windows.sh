#!/bin/zsh
# Checks that Flux builds for Windows, from this Mac (part 10.9): cargo for the GNU Windows target,
# whose C parts (tree-sitter, ring, wasmtime) mingw-w64 compiles. Releases are built by CI with
# MSVC; this catches what doesn't compile on Windows (a Unix-only call, a macOS-only crate) without
# waiting for it.
#
# Once: `rustup target add x86_64-pc-windows-gnu` and `brew install mingw-w64`.
#
#   scripts/check-windows.sh                      cargo check of the workspace, every target
#   CARGO_CMD=clippy scripts/check-windows.sh -- -D warnings
#
# The output goes to target/check-windows.log; the script prints the crates that failed and the
# count of errors and warnings.
set -u
cd "${0:A:h}/.."
toolchain="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin"
export PATH="$toolchain/bin:/opt/homebrew/bin:$PATH"
# rust-lld finds libLLVM through it (the bundled plugins' build).
export DYLD_FALLBACK_LIBRARY_PATH="$toolchain/lib"
log=target/check-windows.log
mkdir -p target
cargo "${CARGO_CMD:-check}" --workspace --all-targets --keep-going --target x86_64-pc-windows-gnu \
  --message-format short "$@" >"$log" 2>&1
code=$?
grep -E '^error: could not compile' "$log" | sort | uniq -c
echo "$(grep -cE '^[^ ].*:[0-9]+:[0-9]+: (error|warning)' "$log") errors and warnings — $log"
exit $code
