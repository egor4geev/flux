#!/bin/zsh
# Flux for Windows, built on this Mac (part 10.9): a release build of Flux.exe for x64 with
# mingw-w64, packed as dist/windows/Flux-<version>-windows-x86_64.zip — Flux.exe and its licence,
# to unpack and run anywhere (an x64 Flux.exe runs on Windows on ARM too, emulated). Releases are
# built by CI with MSVC, for x64 and ARM64, with the installer (.github/workflows/release.yml).
#
# Once: `rustup target add x86_64-pc-windows-gnu wasm32-wasip2` and `brew install mingw-w64`.
#
#   scripts/bundle-windows.sh          → dist/windows/Flux-<version>-windows-x86_64.zip
#
# FLUX_BUILD_VERSION, if set, is the version Flux reports and the zip names; otherwise the
# workspace's. cargo builds into target/x86_64-pc-windows-gnu/release: target/release, where the
# Mac's Flux.app lives, stays untouched. Flux.exe is linked statically (`-static`): mingw's runtime
# (libgcc, winpthread) goes inside, so it needs only Windows' own DLLs — the script prints them and
# fails on any other. Not signed: Windows SmartScreen asks once (More info → Run anyway).
set -euo pipefail
cd "${0:A:h}/.."
toolchain="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin"
export PATH="$toolchain/bin:/opt/homebrew/bin:$PATH"
# rust-lld finds libLLVM through it (the bundled plugins' build).
export DYLD_FALLBACK_LIBRARY_PATH="$toolchain/lib"
target=x86_64-pc-windows-gnu
# For this target alone: build scripts and the plugins (wasm32-wasip2) don't get it.
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C link-arg=-static"
log=target/bundle-windows.log
mkdir -p target
cargo build --release -p flux-app --target $target 2>&1 | tee "$log"
# A bundled plugin that didn't build is left out with a warning (crates/flux-app/build.rs): not in
# a download.
if grep -F 'built without the bundled plugin' "$log"; then
  echo "error: Flux.exe is missing bundled plugins (above)" >&2
  exit 1
fi

version=${FLUX_BUILD_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
build=${CARGO_TARGET_DIR:-target}/$target/release
out=dist/windows
name=Flux-$version-windows-x86_64
stage=$out/$name
rm -rf "$stage" "$out/$name.zip"
mkdir -p "$stage"
cp "$build/flux.exe" "$stage/Flux.exe"
cp LICENSE "$stage/LICENSE.txt"

dlls=(${(f)"$(x86_64-w64-mingw32-objdump -p "$stage/Flux.exe" | sed -n 's/^.*DLL Name: //p' | sort -fu)"})
echo "Flux.exe imports: ${(j:, :)dlls}"
# mingw's own runtime DLLs: Windows doesn't have them.
if foreign=$(print -l -- $dlls | grep -iE '^(libgcc|libstdc\+\+|libwinpthread|libssp|libatomic)'); then
  echo "error: Flux.exe needs mingw's DLLs, it wouldn't start on its own: $foreign" >&2
  exit 1
fi

# The files at the zip's root: Explorer's Extract All puts them into a folder of the zip's name.
(cd "$stage" && zip -q -X -9 -r "../$name.zip" .)
rm -rf "$stage"
zip=$(cd "$out" && pwd)/$name.zip
echo "$zip ($(du -h "$zip" | cut -f1 | tr -d ' '), Flux.exe $(du -h "$build/flux.exe" | cut -f1 | tr -d ' '))"
