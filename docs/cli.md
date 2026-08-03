# CLI and configuration

The `pasteport` command talks to the same service the macOS app uses, so it works
whether you launched the app or ran `pasteportd` yourself.

## Running the service

The macOS app starts and stops the service itself. Everywhere else, run it:

```bash
pasteportd
```

| Flag | Effect |
|---|---|
| `--data-dir DIR` | Use a different data directory |
| `--socket PATH` | Use a different control socket |
| `--poll-interval MS` | Override the poll interval for this run only |
| `--private` | Keep history in memory; write nothing to disk |
| `--capture-on-start` | Store whatever is already on the clipboard at startup |
| `--log LEVEL` | `error`, `warn`, `info`, `debug`, `trace` |

`RUST_LOG` wins over `--log` when set.

## Commands

| Command | What it does |
|---|---|
| `pasteport list [-n N] [--offset N] [--kind KIND]` | Recent clips, pinned first |
| `pasteport search <words> [-n N]` | Full-text search |
| `pasteport get <id>` | Print one clip's text, pipeable |
| `pasteport copy <id>` | Put a clip back on the clipboard |
| `pasteport pin <id>` / `unpin <id>` | Protect from pruning |
| `pasteport rm <id>` | Delete one clip |
| `pasteport clear [--all] [-y]` | Delete history; asks first |
| `pasteport capture` | Store the clipboard right now |
| `pasteport prune` | Apply the retention policy immediately |
| `pasteport status` | Service and history statistics |

Aliases: `ls` for `list`, `s` for `search`, `cp` for `copy`, `rm` for `remove`.

`--kind` accepts `text`, `rich-text`, `link`, `color`, `image`, `file`.

### Pinboards

Named collections for snippets you reuse. Members are never pruned.

```bash
pasteport board create snippets
```

```bash
pasteport board add snippets 42
```

| Command | What it does |
|---|---|
| `pasteport board list` | List pinboards |
| `pasteport board create <name>` | Create one |
| `pasteport board delete <name>` | Delete it; the clips themselves are kept |
| `pasteport board add <name> <id>` | Add a clip |
| `pasteport board remove <name> <id>` | Remove a clip |
| `pasteport board show <name> [-n N]` | List its clips |

### Scripting

`--json` works on every command and prints the raw protocol response:

```bash
pasteport --json list -n 5
```

Exit status is non-zero on failure in both text and JSON modes, so `&&` chains
behave.

## Configuration

Written on first run to `config.toml` in the data directory:

- macOS: `~/Library/Application Support/Pasteport/`
- Linux: `$XDG_DATA_HOME/pasteport` (default `~/.local/share/pasteport`)

```toml
# How often to check for a new clipboard generation.
poll_interval_ms = 400

# Hard cap on stored unpinned clips. Oldest go first.
max_items = 10000

# Unpinned clips older than this are pruned. Zero disables age pruning.
retention_days = 90

# Clips larger than this are dropped rather than stored.
max_clip_bytes = 8388608

# Store image clips at all. Off keeps the database small.
capture_images = true

# Skip clips from these apps. Matched case-insensitively, on substring, against
# both the app name and its bundle id — so "1password" covers every 1Password
# process. Password managers are here by default.
ignored_apps = ["1password", "bitwarden", "keepassxc", "lastpass", "dashlane"]
```

A malformed config is an error rather than a silent reset to defaults, so a typo
never quietly wipes your ignore list.

## Environment variables

| Variable | Effect |
|---|---|
| `PASTEPORT_DATA_DIR` | Override the data directory. Read by the service, the CLI, and the app |
| `PASTEPORT_SOCKET` | Override the control socket path |
| `RUST_LOG` | Standard `tracing` filter, e.g. `pasteport=debug` |

Pointing everything at a scratch directory keeps experiments away from your real
history:

```bash
pasteportd --data-dir /tmp/pp-test --log debug
```

## Files in the data directory

| File | Contents |
|---|---|
| `history.sqlite3` | The clips, plus the FTS index. Mode `0600` |
| `config.toml` | The settings above. Mode `0600` |
| `daemon.sock` | Control socket. Mode `0600` |

The directory itself is `0700`. If it is deep enough that the socket path would
exceed the platform's `sun_path` limit, the socket moves to a short path in the
per-user runtime directory instead.

## Uninstalling

Delete the app, and if you want the history gone too:

```bash
rm -rf ~/Library/Application\ Support/Pasteport
```

Nothing is installed outside the app bundle — no LaunchAgent, no receipts.
