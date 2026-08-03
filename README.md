# Pasteport

Clipboard history for macOS and Linux. Local-first, no account, no sync server,
no telemetry.

Pasteport remembers what you copy, lets you search it, and puts it back on the
clipboard when you need it. The engine is a small Rust core; the interface is
native on each platform.

> **Status: 0.1.0, early.** The engine, daemon, and CLI are built and tested.
> The two GUI layers are scaffolded and not yet shippable. See
> [Roadmap](#roadmap).

## Why another clipboard manager

Most good ones are macOS-only. Most cross-platform ones are Electron and idle at
150 MB. Pasteport keeps the parts that have to be native native, and shares
everything else:

```
                pasteport-core          storage, search, retention, policy
                      |
    +-----------------+-----------------+
    |                 |                 |
pasteport-clipboard   pasteport-license  pasteport-daemon
 NSPasteboard /        Ed25519, offline   watcher + Unix socket API
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
| File permissions | Data dir `0700`; database, WAL, config, license, and socket all `0600` |
| Temp files | `PRAGMA temp_store = MEMORY`, so SQLite never spills clip contents outside our own files |
| Network | There is no network code. No sync, no crash reporting, no update check |
| Deleted clips | Removed from the search index too, via FTS triggers |

## Install

### From source

Needs Rust 1.82 or newer.

```bash
git clone https://github.com/ChrisJohnson89/pasteport
cd pasteport
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

Start the service, then talk to it:

```bash
pasteportd &
```

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
| `pasteport status` | Daemon, license, and history stats |
| `pasteport board ...` | Manage pinboards |
| `pasteport license ...` | Install or remove a license key |

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

## Licensing and the source

Pasteport is **paid software with an open codebase**, which needs saying plainly
because the combination confuses people:

- The **source is AGPL-3.0**. Read it, audit it, patch it, run it.
- A **build you compile yourself is fully functional.** No feature gates, no
  crippled mode. `pasteport status` will call it a source build.
- The **paid product** is the signed and notarized binary, the installer, and
  support. That is what a license key unlocks.
- License keys are **Ed25519 signatures verified offline**. Nothing phones home,
  ever.
- The trial is 14 days and is a **courtesy timer, not DRM**. Delete the file and
  you get another 14 days. Locking that down would cost honest users more than
  it would recover.

Signing code lives behind a non-default `mint` feature, so distributed binaries
cannot mint keys even in principle.

See [docs/licensing.md](docs/licensing.md) for the key format and the release
signing process.

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
| `pasteport-license` | Offline key verification, trial handling |
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
- [x] Offline license verification

Next:

- [ ] SwiftUI menu bar app: global hotkey, search palette, paste-on-select
- [ ] GTK4 window with the same interaction model
- [ ] Global hotkey capture on both platforms
- [ ] Encrypted-at-rest option (SQLCipher)
- [ ] Signed release builds and installers

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
