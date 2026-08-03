# Pasteport for macOS

SwiftUI menu bar app. It links the Rust engine as a static library and talks to
`pasteportd` over the local socket.

> **Status: scaffolded, not yet buildable as an app bundle.** The Swift sources
> and the FFI layer are complete and the Rust side is tested. What is missing is
> the Xcode project that ties them together, plus the global hotkey. See
> [Remaining work](#remaining-work).

## Layout

| File | Role |
|---|---|
| `Pasteport/pasteport.h` | C ABI exposed by `pasteport-ffi`, hand-written and checked in |
| `Pasteport/PasteportEngine.swift` | `actor` wrapping the C ABI; Codable mirrors of the protocol types |
| `Pasteport/PasteportApp.swift` | `MenuBarExtra` entry point and the observable model |
| `Pasteport/HistoryView.swift` | Search panel, result rows, settings form |

The FFI surface is four functions on purpose. The protocol is modelled in Swift
as `Codable`, so adding a request never means touching the header or the Rust
side.

## Building the static library

```bash
cargo build --release -p pasteport-ffi --target aarch64-apple-darwin
```

```bash
cargo build --release -p pasteport-ffi --target x86_64-apple-darwin
```

Then combine them into a universal library:

```bash
lipo -create \
  target/aarch64-apple-darwin/release/libpasteport_ffi.a \
  target/x86_64-apple-darwin/release/libpasteport_ffi.a \
  -output target/libpasteport_ffi.a
```

## Wiring it into Xcode

Once the project exists, four settings matter:

1. **Bridging header** — point `SWIFT_OBJC_BRIDGING_HEADER` at
   `Pasteport/pasteport.h`, or add a module map if you prefer a proper module.
2. **Link the library** — add `libpasteport_ffi.a` to *Link Binary With
   Libraries*, and its directory to `LIBRARY_SEARCH_PATHS`.
3. **System libraries** — the Rust static library needs `libSystem` (implicit)
   and AppKit, which the app already links.
4. **`LSUIElement`** — set to `true` in `Info.plist`. A menu bar utility should
   not own a dock icon.

A build phase that runs the `cargo build` above before compiling Swift keeps the
two halves in sync.

## Interaction model

Deliberately the same as the GTK app, so muscle memory transfers:

| Key | Action |
|---|---|
| type | filter as you go, debounced 120 ms |
| Return | copy the highlighted row, or the top result if nothing is highlighted |
| double click | copy |
| Escape | close the panel |
| right click | copy / pin / delete |

## Remaining work

- [ ] Xcode project or an SPM package with the build phase above
- [ ] Global hotkey. Needs `Carbon.RegisterEventHotKey` or a
      `CGEventTap`, and the latter requires Accessibility permission — the
      trust prompt needs designing, not just calling
- [ ] Paste-on-select: copying is done, synthesising ⌘V into the previously
      focused app is not
- [ ] Image previews in rows. `get_bytes` already returns the payload
- [ ] Launch-at-login for `pasteportd` via `SMAppService`
- [ ] Editing retention and the ignore list in Settings rather than `config.toml`
- [ ] Signing, notarization, and a DMG

## Running against a test daemon

```bash
pasteportd --data-dir /tmp/pp-dev --log debug
```

The app reads `PASTEPORT_DATA_DIR` the same way the CLI does, so pointing both at
a scratch directory keeps development away from your real history.
