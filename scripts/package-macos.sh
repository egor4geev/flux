#!/bin/bash
# The downloads of a release (part 10.8), from a Flux.app made by scripts/bundle-macos.sh:
#
#   scripts/package-macos.sh <Flux.app> <folder>
#     → <folder>/Flux-<version>-macos-<arch>.zip   the update archive Flux downloads itself
#     → <folder>/Flux-<version>-macos-<arch>.dmg   the first install: Flux and Applications
#
# The version is the bundle's (CFBundleShortVersionString), the processor its binary's (aarch64 or
# x86_64). The zip is made by ditto, which keeps what the signature covers (extended attributes,
# symlinks, modes); the disk image is compressed (ULMO, readable since macOS 10.15) and opens as
# "Flux <version>" with Flux next to a shortcut to Applications. Prints the two paths.
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <Flux.app> <folder>" >&2
    exit 2
fi
APP=$(cd "$1" && pwd)
mkdir -p "$2"
OUT=$(cd "$2" && pwd)

VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
EXECUTABLE=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")
case "$(lipo -archs "$APP/Contents/MacOS/$EXECUTABLE")" in
    arm64) ARCH=aarch64 ;;
    x86_64) ARCH=x86_64 ;;
    *)
        echo "error: $APP is neither arm64 nor x86_64 alone: $(lipo -archs "$APP/Contents/MacOS/$EXECUTABLE")" >&2
        exit 1
        ;;
esac
NAME=Flux-$VERSION-macos-$ARCH

# A broken signature would only show on the user's Mac.
codesign --verify --deep --strict "$APP"

ZIP=$OUT/$NAME.zip
rm -f "$ZIP"
ditto -c -k --keepParent --sequesterRsrc "$APP" "$ZIP"

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/flux-dmg.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
ditto "$APP" "$STAGE/Flux.app"
ln -s /Applications "$STAGE/Applications"
DMG=$OUT/$NAME.dmg
rm -f "$DMG"
hdiutil create -quiet -volname "Flux $VERSION" -srcfolder "$STAGE" -fs HFS+ -format ULMO -ov "$DMG"

echo "$ZIP"
echo "$DMG"
