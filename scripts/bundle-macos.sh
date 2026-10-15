#!/bin/bash
# Flux.app for macOS: a release build, an icon made from the logo, Info.plist.
#
#   scripts/bundle-macos.sh            → target/release/Flux.app
#   open target/release/Flux.app       # launch as an app (icon in the Dock and Finder)
#   open target/release/Flux.app --args ~/dev/project
#
# Signed with the developer's own certificate, if there is one (see the end); not notarized: that
# is 10.7 (Roadmap). The icon is generated from crates/flux-app/assets/brand/logo.svg
# (scripts/app-icon.swift + iconutil).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

cargo build --quiet --release -p flux-app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=target/release/Flux.app
ICONSET=target/release/flux.iconset

rm -rf "$APP" "$ICONSET"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/flux "$APP/Contents/MacOS/flux"
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
</dict>
</plist>
PLIST

# The Keychain remembers «Always Allow» for the requirement of the app's signature. The linker's
# ad-hoc signature requires the binary's own hash, so every build is a new app to the Keychain and
# plugin secrets ask again; a certificate's requirement (identifier + certificate) stays the same.
# A self-signed one will do: Keychain Access → Certificate Assistant → Create a Certificate…, name
# "Flux Dev", Self Signed Root, Code Signing. FLUX_SIGN_IDENTITY picks another one; "-" is ad-hoc.
IDENTITY=${FLUX_SIGN_IDENTITY:-Flux Dev}
if [ "$IDENTITY" = "-" ] || security find-identity -p codesigning | grep -qF "\"$IDENTITY\""; then
    codesign --force --sign "$IDENTITY" --timestamp=none "$APP" ||
        echo "warning: codesign failed — Flux.app keeps the linker's ad-hoc signature" >&2
else
    echo "note: no \"$IDENTITY\" certificate — Flux.app is signed ad-hoc (Keychain asks after every build)" >&2
fi
# Finder and the Dock cache icons: a fresh bundle date forces the icon to be re-read.
touch "$APP"
echo "$ROOT/$APP"
