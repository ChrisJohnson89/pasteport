# Pasteport

[![CI](https://github.com/ChrisJohnson89/pasteport/actions/workflows/ci.yml/badge.svg)](https://github.com/ChrisJohnson89/pasteport/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![Free](https://img.shields.io/badge/price-free-brightgreen.svg)](#free-and-open-source)

**Free** clipboard history for macOS and Linux. Local-first, no account, no sync
server, no telemetry, no paid tier.

Pasteport remembers what you copy, lets you search it, and puts it back on the
clipboard when you need it. The engine is a small Rust core; the interface is
native on each platform.

> **Status: 0.1.0, early.** There is a working macOS app you can double-click,
> plus a daemon and CLI. The GTK4 front end for Linux builds but has had less
> use. No global hotkey yet. See [Roadmap](#roadmap).

## Why another clipboard manager

Most good ones are macOS-only. Most cross-platform ones are Electron and idle at
150 MB. Pasteport keeps the parts that have to be native native, and shares
everything else:

```
                pasteport-core          storage, search, retention, policy
                      |
    +-----------------+-----------------+
    |                 |                 |
pasteport-clipboard                      pasteport-daemon
 NSPasteboard /                          watcher + Unix socket API
 wl-clipboard / xclip                          |
                                    +----------+----------+
                                    |          |          |
                                  CLI      SwiftUI     GTK4
```

One engine, one protocol, three front ends. A bug fixed in search is fixed
everywhere.

## What it does

- **History with dedup.** Copy the same thing twice and you get one entry with a
  bumped timestamp, not two rows.
- **Typed clips.** Text, rich text, links, colors, images, and file references
  are detected and filterable.
- **Full-text search.** SQLite FTS5 when available, substring matching when not.
  Your search query is never interpreted as query syntax, so pasting `a AND "b`
  searches for those characters.
- **Pinboards.** Named collections for snippets you reuse. Pinned clips and
  pinboard members are never pruned.
- **Retention you control.** Age limit, count limit, size limit, all in one
  config file.

## Privacy, concretely

Clipboard history is one of the most sensitive files on a machine. What Pasteport
does about that:

| Concern | What happens |
|---|---|
| Password managers | 1Password, Bitwarden, KeePassXC and others are on the ignore list by default |
| `org.nspasteboard.ConcealedType` | Honoured. Concealed items are detected **before** the payload is read, so a password never enters the process |
| File permissions | Data dir `0700`; database, WAL, config, and socket all `0600` |
| Temp files | `PRAGMA temp_store = MEMORY`, so SQLite never spills clip contents outside our own files |
| Network | There is no network code. No sync, no crash reporting, no update check |
| Deleted clips | Removed from the search index too, via FTS triggers |

## Install

### macOS app

Builds `Pasteport.app` and puts it in `/Applications`, ready to double-click:

```bash
git clone https://github.com/ChrisJohnson89/pasteport
```

```bash
cd pasteport && ./apps/macos/build-app.sh --install
```

Then open it from Applications, or:

```bash
open -a Pasteport
```

The app is self-contained. It ships the background service inside its own bundle
and starts it on launch, so there is nothing else to install and nothing to run
in a terminal. Quitting the app stops the service again.

Uninstalling is dragging the app to the trash — no LaunchAgent plist, no
receipts, nothing installed outside the bundle. History lives in
`~/Library/Application Support/Pasteport/` if you want that gone too.

Needs Rust 1.82+, Xcode command line tools, and macOS 14 or newer.

> The build is signed ad-hoc rather than with a Developer ID, because it is built
> on your own machine. A signed and notarized download is on the roadmap.

### CLI and service only

For Linux, or if you only want the terminal side:

```bash
cargo build --release
```

Binaries land in `target/release/`: `pasteportd` (the service) and `pasteport`
(the CLI).

**Linux** also needs a clipboard helper:

```bash
sudo apt install wl-clipboard xclip    # Debian/Ubuntu
sudo dnf install wl-clipboard xclip    # Fedora
sudo pacman -S wl-clipboard xclip      # Arch
```

Pasteport picks `wl-clipboard` under Wayland and `xclip` (or `xsel`) under X11.

## Use

### The app

Double-click Pasteport. A window opens with a search field and your history; the
same panel is on the menu bar icon. Type to filter, Return to copy the highlighted
clip back to the clipboard, right-click for pin and delete.

The window refreshes itself, so new clips appear as you copy them.

### The CLI

The app's service and the CLI talk to the same database, so these work whether
you started the app or ran `pasteportd` yourself:

```bash
pasteport list
```

```bash
pasteport search "api key"
```

```bash
pasteport copy 42
```

Full command list:

| Command | What it does |
|---|---|
| `pasteport list [-n N] [--kind link]` | Recent clips, pinned first |
| `pasteport search <words>` | Full-text search |
| `pasteport get <id>` | Print one clip's text, pipeable |
| `pasteport copy <id>` | Put a clip back on the clipboard |
| `pasteport pin <id>` / `unpin <id>` | Protect from pruning |
| `pasteport rm <id>` | Delete one clip |
| `pasteport clear [--all]` | Delete history; asks first |
| `pasteport capture` | Store the clipboard right now |
| `pasteport prune` | Apply retention immediately |
| `pasteport status` | Daemon and history stats |
| `pasteport board ...` | Manage pinboards |

Add `--json` to any command for machine-readable output. Exit status is non-zero
on failure, in both modes.

### Config

Written on first run to `config.toml` in the data directory:

- macOS: `~/Library/Application Support/Pasteport/`
- Linux: `~/.local/share/pasteport/`

```toml
poll_interval_ms = 400
max_items = 10000
retention_days = 90
max_clip_bytes = 8388608
capture_images = true
ignored_apps = ["1password", "bitwarden", "keepassxc"]
```

`PASTEPORT_DATA_DIR` overrides the location, which is handy for testing:

```bash
pasteportd --data-dir /tmp/pp-test --log debug
```

`pasteportd --private` keeps history in memory only and writes nothing to disk.

## Free and open source

Pasteport is free. Not freemium, not trial-then-pay, not free-with-a-pro-tier:

- **No payment, ever.** There is no license key, no trial timer, no entitlement
  check. The code that used to enforce one has been deleted, not disabled.
- **No account.** Nothing to sign up for.
- **No telemetry.** There is no network code in the product at all — no sync, no
  crash reporting, no update check. You can verify that by grepping for it.
- **AGPL-3.0.** Read it, patch it, fork it, ship your own build.

If you find it useful, a star on the repo is plenty.

## Development

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Tests that touch the real system clipboard are `#[ignore]`d so a test run never
fights your clipboard:

```bash
cargo test -- --ignored --test-threads=1
```

Layout:

| Crate | Purpose |
|---|---|
| `pasteport-core` | Clips, SQLite store, search, retention. No platform code |
| `pasteport-clipboard` | `ClipboardBackend` trait plus macOS and Linux implementations |
| `pasteport-daemon` | Watcher, Unix socket server, `pasteportd` |
| `pasteport-cli` | The `pasteport` command |
| `pasteport-ffi` | C ABI so the SwiftUI app can reuse the engine |
| `pasteport-gtk` | GTK4 front end. Excluded from the default workspace: needs system GTK4 |

`docs/architecture.md` covers the design decisions and the reasoning behind
them.

## Roadmap

Done:

- [x] Storage engine with dedup, FTS search, pinboards, retention
- [x] macOS `NSPasteboard` backend with concealed-type handling
- [x] Linux Wayland and X11 backends
- [x] Daemon, Unix socket protocol, CLI
- [x] Double-clickable macOS app: window plus a menu bar item, self-starting service
- [x] Generated app icon, no binary blobs in the repo

Next:

- [ ] Global hotkey on both platforms, and paste-on-select
- [ ] Signed and notarized releases, plus a DMG
- [ ] Image previews in the list
- [ ] Encrypted-at-rest option (SQLCipher)
- [ ] Signed release builds and installers

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
