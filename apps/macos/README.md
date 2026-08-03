# Pasteport for macOS

A double-clickable SwiftUI app that links the Rust engine as a static library and
runs the background service out of its own bundle.

## Build and install

```bash
./apps/macos/build-app.sh --install
```

That produces `target/app/Pasteport.app` and copies it to `/Applications`. Drop
`--install` to build without installing. `--universal` builds arm64 + x86_64.

Requirements: Rust 1.82+, Xcode command line tools, macOS 14+.

## Build a DMG

```bash
./apps/macos/make-dmg.sh --universal
```

Writes `target/dmg/Pasteport-<version>.dmg` with the app and an Applications
symlink, then mounts it and checks the bundle inside still verifies and carries
the expected version. A DMG that turns out to be empty or to hold a broken
signature is worth catching here rather than in somebody's Downloads folder.

Releases are automatic: bump `version` in the workspace `Cargo.toml`, merge to
main, and [`release.yml`](../../.github/workflows/release.yml) publishes a
GitHub release with the DMG attached. It skips if that version is already
released, so pushing twice cannot duplicate or overwrite anything.

## Why there is no Xcode project

An `.xcodeproj` is a large generated file that nobody reviews and that conflicts
on every merge. Everything it would do here is four steps — compile Swift, link
the static library, lay out the bundle, sign it — and those fit in
[`build-app.sh`](build-app.sh) where they can be read.

The tradeoff is no Xcode previews and no debugger integration. If that starts
costing more than the project file would, generate one; nothing here depends on
its absence.

## Layout

| File | Role |
|---|---|
| `Pasteport/pasteport.h` | C ABI exposed by `pasteport-ffi`, hand-written and checked in |
| `Pasteport/PasteportEngine.swift` | `actor` wrapping the C ABI; Codable mirrors of the protocol types |
| `Pasteport/DaemonController.swift` | Finds, starts, and stops the bundled `pasteportd` |
| `Pasteport/PasteportApp.swift` | Scenes, app delegate, and the observable model |
| `Pasteport/HistoryView.swift` | Search panel, rows, settings |
| `make-icon.swift` | Draws the icon with CoreGraphics and writes an `.iconset` |
| `build-app.sh` | Everything above, assembled and signed |

The FFI surface is four functions on purpose. The protocol is modelled in Swift
as `Codable`, so adding a request never touches the header or the Rust side.

## How it starts the service

`DaemonController` looks for `pasteportd` beside the app binary in
`Contents/MacOS/`, and starts it if nothing is already listening on the socket.

It is a **child process, not a LaunchAgent**. A plist in `~/Library/LaunchAgents`
is state installed outside the bundle that survives dragging the app to the
trash, and leaving a service running after someone has deleted the app is rude.
A child process means uninstalling is still just "delete the app".

Two consequences worth knowing:

- If a `pasteportd` is already running — because you started one in a terminal —
  the app attaches to it instead of starting a second one, and does **not** stop
  it on quit. It only stops what it started.
- Capture stops when the app quits. That is the right default for something with
  no installer, but it does mean this is not a background-forever service yet.
  Launch-at-login via `SMAppService` is on the roadmap.

## The icon is generated

`make-icon.swift` draws it: a rounded indigo tile, a clipboard, and a clock badge
for history. Rendered at all ten sizes `iconutil` wants, so nothing is upscaled
from a single PNG.

Generated rather than committed because it stays reviewable — a colour or radius
change is a readable diff instead of a new binary — and because it forced the
small sizes to be checked. The text lines and the badge are dropped below 32pt,
where they would turn to mush.

```bash
swift apps/macos/make-icon.swift /tmp/Pasteport.iconset
```

## Deployment target and the Rust standard library

The app is stamped `LSMinimumSystemVersion` 14.0. Whether that is *true* depends
on your Rust toolchain:

- **rustup** builds `std` against an old floor, so the claim holds.
- **Homebrew's rust** builds `std` for the host OS. The linker then warns that
  the Rust objects were built for a newer macOS than the app is being linked
  for, and the 14.0 claim is not one you should publish.

`build-app.sh` detects this and prints a note rather than letting the warnings
scroll past. For anything you intend to distribute, use a rustup toolchain.

## Verified

On macOS 26.5, built with the Homebrew toolchain:

- Installs to `/Applications`, launches from Finder, ad-hoc signature verifies
- Starts the bundled `pasteportd` as a child process on first launch
- Creates `~/Library/Application Support/Pasteport/` with `0700`
- Window renders at 520×560; captures clips live while open, with correct kind
  icons, relative timestamps, and use counts
- New and newly-pinned clips stay visible at the top of the list. SwiftUI keeps a
  `List`'s scroll offset anchored to the old content when rows are inserted at the
  front, which pushed the newest clip above the visible area — the footer counted
  a pinned clip the list was not showing. `clipList` re-anchors on change.
- Packages into a DMG that mounts, verifies, and reports the right version
- No crash reports

**Not visually verified:** the menu bar item. `MenuBarExtra` is declared and the
app builds, but an `NSStatusItem` is not a normal window so it cannot be
confirmed the way the main window was — and a menu bar manager like Bartender
will hide it regardless. Look for the clipboard icon in the menu bar.

## Remaining work

- [ ] Global hotkey. Needs `RegisterEventHotKey` or a `CGEventTap`; the latter
      wants Accessibility permission, and that trust prompt needs designing
- [ ] Paste-on-select: copying works, synthesising ⌘V into the previously focused
      app does not
- [ ] Image previews in rows. `get_bytes` already returns the payload
- [ ] Launch at login via `SMAppService`, so capture survives quitting the app
- [ ] Editing retention and the ignore list in Settings rather than `config.toml`
- [ ] Developer ID signing and notarization, so the DMG opens without the
      right-click dance

## Development against a scratch database

```bash
PASTEPORT_DATA_DIR=/tmp/pp-dev open -a Pasteport
```

The app, the service, and the CLI all read that variable, so pointing them at a
throwaway directory keeps development away from your real history.
