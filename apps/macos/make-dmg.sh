#!/usr/bin/env bash
#
# Packages Pasteport.app into a distributable .dmg.
#
# Produces target/dmg/Pasteport-<version>.dmg containing the app and a symlink to
# /Applications, which is the drag-to-install layout people expect on macOS.
#
# Usage:
#   ./apps/macos/make-dmg.sh              # package, building the app if needed
#   ./apps/macos/make-dmg.sh --universal  # build a universal app first
#   ./apps/macos/make-dmg.sh --reuse-app  # package target/app/Pasteport.app as-is
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP="$REPO_ROOT/target/app/Pasteport.app"
DMG_DIR="$REPO_ROOT/target/dmg"

UNIVERSAL=0
REUSE_APP=0
for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    --reuse-app) REUSE_APP=1 ;;
    -h|--help)   sed -n '3,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
: "${VERSION:?could not read version from Cargo.toml}"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# ------------------------------------------------------------------ the app
if [[ $REUSE_APP -eq 1 ]]; then
  [[ -d "$APP" ]] || { echo "no app at $APP (drop --reuse-app to build it)" >&2; exit 1; }
  say "Reusing $APP"
else
  say "Building the app"
  if [[ $UNIVERSAL -eq 1 ]]; then
    "$REPO_ROOT/apps/macos/build-app.sh" --universal
  else
    "$REPO_ROOT/apps/macos/build-app.sh"
  fi
fi

# Refuse to ship something unsigned. An unsigned bundle is not merely untidy:
# Gatekeeper treats it differently, and a broken signature is the kind of thing
# that is invisible until somebody else downloads it.
say "Verifying the app signature"
codesign --verify --deep --strict "$APP"

# ------------------------------------------------------------------ staging
#
# hdiutil images the directory it is given, so the layout of the mounted volume
# is exactly the layout of this staging directory.
STAGE="$DMG_DIR/stage"
say "Staging"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/Pasteport.app"
ln -s /Applications "$STAGE/Applications"

# A short read-me visible in the window, since this build is not notarized and
# first-launch will need one extra step.
cat > "$STAGE/README.txt" <<'TXT'
Pasteport — free clipboard history for macOS

To install: drag Pasteport to the Applications folder beside it.

First launch
------------
macOS will block the first launch. This build carries only an ad-hoc signature,
not an Apple Developer certificate, so Gatekeeper treats it as unidentified.

To open it:
    1. Double-click Pasteport. macOS blocks it.
    2. Open System Settings > Privacy & Security.
    3. Scroll down and click "Open Anyway" next to the Pasteport message.

macOS remembers the choice, so this is once only.

Note: Control-clicking and choosing Open no longer bypasses this. Apple removed
that path in macOS Sequoia; System Settings is the way now.

Or, from a terminal:
    xattr -dr com.apple.quarantine /Applications/Pasteport.app

Pasteport is free software under the AGPL-3.0-or-later.
Source, issues, and the CLI: https://github.com/ChrisJohnson89/pasteport
TXT

# ------------------------------------------------------------------- image
DMG="$DMG_DIR/Pasteport-$VERSION.dmg"
say "Creating $(basename "$DMG")"
rm -f "$DMG"

# UDZO is zlib-compressed and read-only, which is what a download should be.
# The volume name is what appears in Finder's sidebar when mounted.
hdiutil create \
  -volname "Pasteport $VERSION" \
  -srcfolder "$STAGE" \
  -fs HFS+ \
  -format UDZO \
  -imagekey zlib-level=9 \
  -ov \
  -quiet \
  "$DMG"

# The staging directory has done its job. Remove it rather than leaving a copy of
# the app and an `Applications` symlink lying around: anything that later globs
# target/dmg/* and follows symlinks would walk the whole of /Applications. That
# is not hypothetical — it OOM-killed the release workflow's artifact upload.
rm -rf "$STAGE"

# ------------------------------------------------------------------- verify
#
# Mount it and check the app is actually in there and still verifies. Building a
# DMG that turns out to be empty or to contain a broken bundle is a failure worth
# catching here rather than in somebody's Downloads folder.
say "Verifying the image"
hdiutil verify -quiet "$DMG"

MOUNT="$(mktemp -d)"
hdiutil attach "$DMG" -mountpoint "$MOUNT" -nobrowse -quiet -readonly
cleanup() { hdiutil detach "$MOUNT" -quiet 2>/dev/null || true; rmdir "$MOUNT" 2>/dev/null || true; }
trap cleanup EXIT

[[ -d "$MOUNT/Pasteport.app" ]] || { echo "the image does not contain Pasteport.app" >&2; exit 1; }
[[ -L "$MOUNT/Applications" ]]  || { echo "the image is missing the Applications symlink" >&2; exit 1; }
codesign --verify --deep --strict "$MOUNT/Pasteport.app"
MOUNTED_VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
  "$MOUNT/Pasteport.app/Contents/Info.plist")"
[[ "$MOUNTED_VERSION" == "$VERSION" ]] \
  || { echo "version mismatch: image has $MOUNTED_VERSION, expected $VERSION" >&2; exit 1; }

ARCHS="$(lipo -archs "$MOUNT/Pasteport.app/Contents/MacOS/Pasteport")"
cleanup
trap - EXIT

printf '\n'
say "Built $DMG"
say "Version: $VERSION   Architectures: $ARCHS   Size: $(du -h "$DMG" | cut -f1)"
say "Checksum: $(shasum -a 256 "$DMG" | cut -d' ' -f1)"
