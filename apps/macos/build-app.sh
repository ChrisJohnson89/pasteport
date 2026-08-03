#!/usr/bin/env bash
#
# Builds Pasteport.app.
#
# No Xcode project: swiftc plus a hand-assembled bundle. An .xcodeproj is a large
# generated file that nobody reviews, and everything it would do here fits in one
# readable script — compile Swift, link the Rust static library, lay out the
# bundle, sign it.
#
# Usage:
#   ./apps/macos/build-app.sh                 # build into target/app/
#   ./apps/macos/build-app.sh --install       # also copy into /Applications
#   ./apps/macos/build-app.sh --universal     # build a universal (arm64+x86_64) app
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP_SRC="$REPO_ROOT/apps/macos/Pasteport"
OUT_DIR="$REPO_ROOT/target/app"
APP="$OUT_DIR/Pasteport.app"

INSTALL=0
UNIVERSAL=0
for arg in "$@"; do
  case "$arg" in
    --install)   INSTALL=1 ;;
    --universal) UNIVERSAL=1 ;;
    -h|--help)   sed -n '3,16p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
: "${VERSION:?could not read version from Cargo.toml}"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

HOST_ARCH="$(uname -m)"

# macOS 14 (Sonoma) rather than 13: the settings window is opened with SwiftUI's
# SettingsLink, which is 14+. The macOS 13 alternative is the private
# showSettingsWindow: selector, and a private selector Apple already renamed once
# is a worse dependency than a one-version-newer floor.
DEPLOYMENT_TARGET="14.0"

# Rust must build against the same floor as the Swift binary. Without this, the
# clang that compiles bundled SQLite and the Rust std objects targets the host OS
# (26.x here), the linker warns that the objects are newer than what it is linking
# for, and the app would advertise a 14.0 minimum it does not actually honour.
export MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"

# ---------------------------------------------------------------- Rust halves
#
# The app needs two artifacts from cargo: the FFI static library it links
# against, and the pasteportd binary it ships and launches.

if [[ $UNIVERSAL -eq 1 ]]; then
  TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
  say "Building Rust for ${TARGETS[*]}"
  for t in "${TARGETS[@]}"; do
    if ! rustup target list --installed 2>/dev/null | grep -qx "$t"; then
      echo "missing Rust target $t. Install it with: rustup target add $t" >&2
      exit 1
    fi
    cargo build --release --target "$t" -p pasteport-ffi -p pasteport-daemon
  done

  mkdir -p "$OUT_DIR/lib"
  FFI_LIB="$OUT_DIR/lib/libpasteport_ffi.a"
  DAEMON_BIN="$OUT_DIR/lib/pasteportd"
  lipo -create -output "$FFI_LIB" \
    "$REPO_ROOT/target/aarch64-apple-darwin/release/libpasteport_ffi.a" \
    "$REPO_ROOT/target/x86_64-apple-darwin/release/libpasteport_ffi.a"
  lipo -create -output "$DAEMON_BIN" \
    "$REPO_ROOT/target/aarch64-apple-darwin/release/pasteportd" \
    "$REPO_ROOT/target/x86_64-apple-darwin/release/pasteportd"
  UNIVERSAL_SWIFT=1
else
  say "Building Rust for the host architecture"
  cargo build --release -p pasteport-ffi -p pasteport-daemon
  FFI_LIB="$REPO_ROOT/target/release/libpasteport_ffi.a"
  DAEMON_BIN="$REPO_ROOT/target/release/pasteportd"
  UNIVERSAL_SWIFT=0
fi

[[ -f "$FFI_LIB" ]]    || { echo "missing $FFI_LIB" >&2; exit 1; }
[[ -f "$DAEMON_BIN" ]] || { echo "missing $DAEMON_BIN" >&2; exit 1; }

# ------------------------------------------------------------- bundle layout
say "Laying out $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# ---------------------------------------------------------------------- icon
say "Rendering the icon"
ICONSET="$OUT_DIR/Pasteport.iconset"
swift "$REPO_ROOT/apps/macos/make-icon.swift" "$ICONSET" >/dev/null
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

# ------------------------------------------------------------------ Info.plist
say "Writing Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>                  <string>Pasteport</string>
    <key>CFBundleDisplayName</key>           <string>Pasteport</string>
    <key>CFBundleIdentifier</key>            <string>com.pasteport.Pasteport</string>
    <key>CFBundleExecutable</key>            <string>Pasteport</string>
    <key>CFBundleIconFile</key>              <string>AppIcon</string>
    <key>CFBundleShortVersionString</key>    <string>$VERSION</string>
    <key>CFBundleVersion</key>               <string>$VERSION</string>
    <key>CFBundlePackageType</key>           <string>APPL</string>
    <key>CFBundleSignature</key>             <string>????</string>
    <key>CFBundleInfoDictionaryVersion</key> <string>6.0</string>
    <key>LSMinimumSystemVersion</key>        <string>14.0</string>
    <!-- Menu bar plus a window: not LSUIElement, so double-clicking the app in
         Applications visibly opens something. -->
    <key>LSUIElement</key>                   <false/>
    <key>NSHumanReadableCopyright</key>
    <string>Pasteport is free software under the AGPL-3.0-or-later.</string>
    <key>NSSupportsAutomaticTermination</key> <false/>
    <key>NSSupportsSuddenTermination</key>    <false/>
    <key>NSHighResolutionCapable</key>        <true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

# ------------------------------------------------------------------ compile
say "Compiling Swift"
SWIFT_SOURCES=(
  "$APP_SRC/PasteportEngine.swift"
  "$APP_SRC/DaemonController.swift"
  "$APP_SRC/HistoryView.swift"
  "$APP_SRC/PasteportApp.swift"
)

# One swiftc invocation for the whole module. The bridging header is how Swift
# sees the Rust C ABI; -parse-as-library is required because @main provides the
# entry point rather than top-level code.
LINK_LOG="$OUT_DIR/link.log"
: > "$LINK_LOG"

compile_arch() {
  local out="$1"; shift
  # Tee the linker output: it is the only place a deployment-target mismatch
  # between the Rust objects and the Swift binary shows up, and that mismatch is
  # worth reporting rather than scrolling past.
  swiftc \
    -O \
    -parse-as-library \
    -swift-version 5 \
    -sdk "$(xcrun --show-sdk-path --sdk macosx)" \
    -import-objc-header "$APP_SRC/pasteport.h" \
    -framework AppKit \
    -framework SwiftUI \
    -framework Foundation \
    "$@" \
    "${SWIFT_SOURCES[@]}" \
    "$FFI_LIB" \
    -o "$out" 2>&1 | tee -a "$LINK_LOG"
  # tee swallows swiftc's status, so fail explicitly when the output is missing.
  [[ -f "$out" ]] || { echo "swiftc did not produce $out" >&2; exit 1; }
}

if [[ $UNIVERSAL_SWIFT -eq 1 ]]; then
  compile_arch "$OUT_DIR/Pasteport-arm64" -target "arm64-apple-macosx$DEPLOYMENT_TARGET"
  compile_arch "$OUT_DIR/Pasteport-x86_64" -target "x86_64-apple-macosx$DEPLOYMENT_TARGET"
  lipo -create -output "$APP/Contents/MacOS/Pasteport" \
    "$OUT_DIR/Pasteport-arm64" "$OUT_DIR/Pasteport-x86_64"
  rm -f "$OUT_DIR/Pasteport-arm64" "$OUT_DIR/Pasteport-x86_64"
else
  compile_arch "$APP/Contents/MacOS/Pasteport" \
    -target "$HOST_ARCH-apple-macosx$DEPLOYMENT_TARGET"
fi

# ------------------------------------------------- embed the service binary
say "Embedding pasteportd"
cp "$DAEMON_BIN" "$APP/Contents/MacOS/pasteportd"
chmod +x "$APP/Contents/MacOS/pasteportd"

# ------------------------------------------------------------------ signing
#
# Ad-hoc signature. Enough for macOS to run it locally and to give the bundle a
# stable identity; a Developer ID and notarization are what a public download
# would need, and that is a release-pipeline job rather than a build-script one.
say "Signing (ad-hoc)"
codesign --force --sign - --timestamp=none "$APP/Contents/MacOS/pasteportd"
codesign --force --sign - --timestamp=none --options runtime "$APP" 2>/dev/null \
  || codesign --force --sign - --timestamp=none "$APP"

codesign --verify --deep --strict "$APP" && say "Signature verifies"

# ------------------------------------------------------------------- install
if [[ $INSTALL -eq 1 ]]; then
  DEST="/Applications/Pasteport.app"
  say "Installing to $DEST"
  # Quit a running copy first: replacing the bundle under a live process leaves
  # it running from a deleted image.
  osascript -e 'tell application "Pasteport" to quit' 2>/dev/null || true
  pkill -x Pasteport 2>/dev/null || true
  sleep 1
  rm -rf "$DEST"
  cp -R "$APP" "$DEST"
  say "Installed. Open it from Applications, or: open -a Pasteport"
else
  say "Built $APP"
  say "Install with: $0 --install"
fi

# ------------------------------------------------- deployment target honesty
#
# Rust ships a prebuilt standard library, and some distributions (Homebrew's, for
# one) build it against the host OS rather than an old floor. When that happens
# the app binary is stamped with our minimum but contains objects compiled for a
# newer one. It runs fine on this machine; it is not a claim worth making to
# anyone else. Say so rather than let the warnings scroll past.
if grep -q "was built for newer" "$LINK_LOG" 2>/dev/null; then
  NEWEST="$(grep -o "version ([0-9.]*)" "$LINK_LOG" | grep -o "[0-9.]*" | sort -V | tail -1)"
  printf '\n'
  printf '\033[1;33mNOTE\033[0m %s\n' "This build's Rust standard library was compiled for macOS $NEWEST,"
  printf '     %s\n' "but the app is stamped with a minimum of macOS $DEPLOYMENT_TARGET."
  printf '     %s\n' "It works on this machine. Do not publish it as supporting macOS $DEPLOYMENT_TARGET."
  printf '     %s\n' "For a distributable build, use a rustup toolchain, whose std is built"
  printf '     %s\n' "against an old floor:  rustup default stable && $0 --universal"
fi

printf '\n'
say "Bundle contents:"
find "$APP" -type f | sed "s|$APP|Pasteport.app|" | sort
printf '\n'
say "Architectures: $(lipo -archs "$APP/Contents/MacOS/Pasteport")"
say "Size: $(du -sh "$APP" | cut -f1)"
