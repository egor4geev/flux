#!/bin/bash
# Приложение flux.app для macOS: release-сборка, иконка из логотипа, Info.plist.
#
#   scripts/bundle-macos.sh            → target/release/flux.app
#   open target/release/flux.app       # запуск как приложение (иконка в Dock, Finder)
#   open target/release/flux.app --args ~/dev/project
#
# Без подписи и нотаризации — это этап 7 (Roadmap). Иконка генерируется из
# crates/flux-app/assets/brand/logo.svg (scripts/app-icon.swift + iconutil).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

cargo build --quiet --release -p flux-app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=target/release/flux.app
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
    <key>CFBundleName</key><string>flux</string>
    <key>CFBundleDisplayName</key><string>flux</string>
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
# Finder и Dock кешируют значки: свежая дата пакета заставляет перечитать иконку.
touch "$APP"
echo "$ROOT/$APP"
