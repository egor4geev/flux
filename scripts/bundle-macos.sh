#!/bin/bash
# Flux.app for macOS: a release build, an icon made from the logo, Info.plist.
#
#   scripts/bundle-macos.sh            → target/release/Flux.app
#   open target/release/Flux.app       # launch as an app (icon in the Dock and Finder)
#   open target/release/Flux.app --args ~/dev/project
#
#   scripts/bundle-macos.sh --target x86_64-apple-darwin --out dist/x86_64
#                                      → dist/x86_64/Flux.app, built for that processor
#
# --target builds for another processor (`rustup target add` it first); --out puts Flux.app into
# another folder. FLUX_BUILD_VERSION, if set, is the version Flux reports and Info.plist names (a
# release in CI, the manual check of updates: scripts/update-test.sh); otherwise the workspace's.
#
# Signed with the developer's own certificate, if there is one (see the end); not notarized (Roadmap,
# 10.8: no Apple Developer ID). FLUX_SIGN_REQUIRED=1 fails when the certificate isn't there instead
# of signing ad-hoc (CI). The icon is generated from crates/flux-app/assets/brand/logo.svg
# (scripts/app-icon.swift + iconutil).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

TARGET=
OUT=
while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            TARGET=${2:?--target needs a target triple}
            shift 2
            ;;
        --out)
            OUT=${2:?--out needs a folder}
            shift 2
            ;;
        *)
            echo "usage: $0 [--target <triple>] [--out <folder>]" >&2
            exit 2
            ;;
    esac
done

# CARGO_TARGET_DIR, if set, is where cargo builds (scripts/update-test.sh keeps its builds apart).
TARGET_DIR=${CARGO_TARGET_DIR:-target}
if [ -n "$TARGET" ]; then
    cargo build --quiet --release -p flux-app --target "$TARGET"
    BUILD=$TARGET_DIR/$TARGET/release
else
    cargo build --quiet --release -p flux-app
    BUILD=$TARGET_DIR/release
fi
VERSION=${FLUX_BUILD_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
OUT=${OUT:-$BUILD}
mkdir -p "$OUT"
APP=$OUT/Flux.app
ICONSET=$OUT/flux.iconset

rm -rf "$APP" "$ICONSET"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BUILD/flux" "$APP/Contents/MacOS/flux"
swift scripts/app-icon.swift crates/flux-app/assets/brand/logo.svg "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/flux.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Flux</string>
    <key>CFBundleDisplayName</key><string>Flux</string>
    <key>CFBundleIdentifier</key><string>dev.flux.editor</string>
    <key>CFBundleExecutable</key><string>flux</string>
    <key>CFBundleIconFile</key><string>flux</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
    <!-- Folders are Flux's documents: macOS then keeps the recent projects for the Dock menu
         (even while Flux isn't running), a folder dropped on the Dock icon opens as a project,
         and Flux is among the Finder's "Open With" apps for folders. -->
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>Folder</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array><string>public.folder</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST

# The Keychain remembers «Always Allow» for the requirement of the app's signature. The linker's
# ad-hoc signature requires the binary's own hash, so every build is a new app to the Keychain and
# plugin secrets ask again; a certificate's requirement (identifier + certificate) stays the same.
# A self-signed one will do: Keychain Access → Certificate Assistant → Create a Certificate…, name
# "Flux Dev", Self Signed Root, Code Signing. FLUX_SIGN_IDENTITY picks another one; "-" is ad-hoc.
# Releases are signed with the same "Flux Dev" (CI imports it: scripts/ci-keychain.sh), so an
# update keeps the Keychain's answers.
# FLUX_SIGN_KEYCHAIN looks for it in that keychain alone (CI's temporary one).
IDENTITY=${FLUX_SIGN_IDENTITY:-Flux Dev}
KEYCHAIN=()
if [ -n "${FLUX_SIGN_KEYCHAIN:-}" ]; then
    KEYCHAIN=(--keychain "$FLUX_SIGN_KEYCHAIN")
fi
if [ "$IDENTITY" = "-" ] ||
    security find-identity -p codesigning ${FLUX_SIGN_KEYCHAIN:+"$FLUX_SIGN_KEYCHAIN"} |
    grep -qF "\"$IDENTITY\""; then
    if ! codesign --force --sign "$IDENTITY" ${KEYCHAIN[@]+"${KEYCHAIN[@]}"} --timestamp=none "$APP"; then
        if [ "${FLUX_SIGN_REQUIRED:-}" = 1 ]; then
            echo "error: codesign failed" >&2
            exit 1
        fi
        echo "warning: codesign failed — Flux.app keeps the linker's ad-hoc signature" >&2
    fi
elif [ "${FLUX_SIGN_REQUIRED:-}" = 1 ]; then
    echo "error: no \"$IDENTITY\" certificate to sign Flux.app with (FLUX_SIGN_REQUIRED)" >&2
    exit 1
else
    echo "note: no \"$IDENTITY\" certificate — Flux.app is signed ad-hoc (Keychain asks after every build)" >&2
fi
# Finder and the Dock cache icons: a fresh bundle date forces the icon to be re-read.
touch "$APP"
echo "$(cd "$OUT" && pwd)/Flux.app"
