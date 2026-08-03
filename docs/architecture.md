# Architecture

Why Pasteport is shaped the way it is. Where a decision could reasonably have
gone the other way, the tradeoff is written down.

## The shape

```
                    ┌──────────────────┐
                    │  pasteport-core  │   clips, SQLite, search, retention
                    └────────┬─────────┘   no platform code, no I/O policy
                             │
        ┌────────────────────┼────────────────────┐
        │                    │                    │
┌───────▼─────────┐ ┌────────▼────────┐ ┌─────────▼────────┐
│ pasteport-      │ │ pasteport-      │ │ pasteport-daemon │
│ clipboard       │ │ license         │ │                  │
│                 │ │                 │ │ watcher thread   │
│ NSPasteboard    │ │ Ed25519 verify  │ │ Unix socket API  │
│ wl-clipboard    │ │ trial clock     │ │                  │
│ xclip / xsel    │ │                 │ │                  │
└─────────────────┘ └─────────────────┘ └────────┬─────────┘
                                                 │ newline-delimited JSON
                         ┌───────────────────────┼───────────────────────┐
                         │                       │                       │
                  ┌──────▼──────┐        ┌───────▼───────┐      ┌────────▼───────┐
                  │ pasteport   │        │ pasteport-ffi │      │ pasteport-gtk  │
                  │ (CLI)       │        │ → SwiftUI app │      │ (GTK4, Linux)  │
                  └─────────────┘        └───────────────┘      └────────────────┘
```

The rule: **the engine never knows which platform it is on, and the front ends
never know how anything is stored.** Everything crosses at the socket.

## Decisions

### One daemon, many clients

The watcher has to be a long-lived process: clipboard change detection is
polling on both platforms, and history has to be captured whether or not a
window is open. Given that process exists anyway, it also owns the database.

The alternative — each UI opening its own SQLite connection — would need
cross-process write coordination and would let two front ends disagree about
what the history contains. One writer, many readers over a socket, is simpler
and never inconsistent.

Cost: the UI cannot show history if the daemon is down. Accepted; the UI's job
is to say so clearly.

### Newline-delimited JSON over a Unix socket

Not gRPC, not a REST server on localhost, not shared memory.

- Every client is on the same machine and owned by the same user.
- Message rate is a handful per keystroke, and payloads are small — image blobs
  are fetched explicitly with `GetBytes`, not pushed in list responses.
- A developer can debug it with `nc`.

Access control is filesystem ownership. The socket lives inside the `0700` data
directory at `0600`, so reaching it already requires being the user who owns the
history. There is no auth token because there is nothing an authenticated peer
could learn that it could not learn by reading the database file directly.

There is **no TCP listener**, deliberately. A clipboard history server on
localhost is a browser-reachable data exfiltration primitive.

### Change detection differs per platform, and that is fine

macOS gives us `NSPasteboard.changeCount`: a monotonically increasing integer.
Idle polling is one integer read every 400 ms, which costs nothing measurable.

Linux gives us nothing equivalent. The backend reads the clipboard and hashes
it. That is genuinely more expensive — one subprocess per poll — and it is the
main reason the Linux poll interval is worth tuning.

Hiding this difference behind `ClipboardBackend::poll()` returning
`Option<NewClip>` means the daemon does not care. Each backend owns its own
change state and only speaks up when something actually changed.

### Linux shells out instead of linking

`wl-paste`, `xclip`, and `xsel` instead of `libwayland` and `libX11`.

Arguments for linking: no subprocess overhead, no runtime dependency, richer
access to selection ownership and window focus.

Arguments for shelling out, which won:

- The build stays free of system headers. `cargo build` works on a fresh box.
- X11 clipboard ownership is genuinely hard — the selection is owned by a live
  process, and getting that wrong means clipboard contents vanish when your
  process exits. `xclip` has been getting it right for twenty years.
- Wayland and X11 need two separate implementations either way.

The cost is real and named: one short-lived process per poll on Linux, and a
runtime dependency the user may have to install. The error message for a missing
helper lists the install command for the three big distro families rather than
just failing.

If the subprocess cost ever matters, the trait boundary is exactly where a
native backend would slot in without touching anything else.

### Concealed content is checked before it is read

The ordering here is the point. `read_pasteboard` asks for the *list of types*
first, checks it against `CONCEALED_MARKERS`, and returns early — before calling
`stringForType:`.

A password from a manager that sets `org.nspasteboard.ConcealedType` therefore
never enters Pasteport's address space at all. Reading it and then discarding it
would be functionally similar and meaningfully worse: the secret would sit in
process memory, in a heap allocation, possibly in a core dump.

The store checks `concealed` before it checks `is_empty` for the same reason: a
concealed clip carries no payload, and the reason it was skipped should be
"concealed", not "empty".

### Search never trusts the query as syntax

FTS5 has an expression language. A clipboard manager's search box receives
whatever the user pasted, which routinely includes quotes, asterisks, and the
word `AND`.

So `search()` tokenizes the query into literal alphanumeric words, quotes each
one, and joins them with `AND` itself. The last token gets a `*` for
as-you-type prefix matching. A query of `alpha*(` searches for the word `alpha`
rather than raising a syntax error.

FTS5 is also a compile-time SQLite option, so the store probes for it at open
time and falls back to an escaped `LIKE` scan. Slower, still correct, and the
`stats.full_text_search` flag tells the user which one they got.

### Deduplication is content-addressed, and ignores provenance

`NewClip::digest()` hashes mime, kind, text, and bytes — but not the source app
or any timestamp. Copying the same snippet from your editor and then from your
browser produces one row with `use_count = 2`, which is what a person means by
"I have copied this before".

Dedupe preserves the original `created_at` and only bumps `last_used_at`. "When
did I first see this" and "when did I last use it" are different questions and
the history answers both.

### Retention prunes on insert, not on a timer

The only moment the history can exceed its limits is immediately after something
was added, so that is when `prune` runs. No background timer, no scheduled job,
nothing to get out of sync.

Pinned clips and pinboard members are excluded from every prune path — age,
count, and `clear` without `--all`. If a user marked something as worth keeping,
no automatic policy overrides that.

### Licensing is verification-only in shipped builds

Key signing lives behind the non-default `mint` feature. A release binary
contains no signing code, so a leaked binary cannot be turned into a key
generator.

The verifying public key is baked in at build time via
`option_env!("PASTEPORT_LICENSE_PUBKEY")`. When it is absent — which is what
happens when a contributor runs `cargo build` — licensing reports
`Status::SelfBuilt` and the app is fully functional.

That last part is a deliberate product decision, not an oversight. The code is
AGPL. Anyone can compile it. Shipping a source tree that builds into a crippled
binary would inconvenience contributors and stop nobody. The paid artifact is
the signed, notarized build.

## Threading

Three threads in the daemon:

| Thread | Job |
|---|---|
| main | `accept()` loop, one request at a time |
| `pasteport-watcher` | polls the clipboard, ingests clips |
| `pasteport-signals` | watches for `SIGINT`/`SIGTERM`, unblocks `accept()` |

The store is behind `Arc<Mutex<Store>>`. Requests are sub-millisecond SQLite
reads, so serializing them costs less than any scheme for avoiding the lock
would.

The watcher and the request handler hold **separate clipboard handles** to the
same system clipboard. A `Copy` request writing to the clipboard therefore never
waits on a poll in progress, and vice versa. The write does bump the platform's
change counter, so the watcher observes it and dedupe bumps the existing row —
which is exactly the desired "pasting from history moves it to the top".

`copy_to_clipboard` drops the store lock before writing to the clipboard, since
Linux helper subprocesses can take milliseconds and the watcher should not stall
behind them.

Signal handling is the awkward one. `accept()` blocks, and a signal handler may
only touch an atomic. So the handler flips a static `AtomicBool`, and the signal
thread polls it, sets the service's shutdown flag, then dials its own socket to
wake the accept loop.

## Testing

Tests live next to what they test. Three groups:

1. **Unit** — kind inference, dedup digests, the trial clock, byte formatting.
2. **Integration through a real socket** — `server.rs` spawns a daemon on a temp
   socket and drives it with a real client, including malformed input, stale
   socket recovery, and the refusal to steal a live socket.
3. **`#[ignore]`d, touches the real clipboard** — run with
   `cargo test -- --ignored --test-threads=1`. Never in a normal run: a test
   suite that hijacks the developer's clipboard is a hostile test suite.

The fake backends (`FakeClipboard`, `BrokenClipboard`, `NullClipboard`) are what
make the daemon's behaviour testable without a display server. `BrokenClipboard`
exists specifically to prove that a failed clipboard write is reported as an
error and does **not** mark the clip as used.

## Known limitations

- **No global hotkey yet.** The pieces that need it are the GUI layers, which
  are not finished.
- **No source app on Linux.** There is no portable way to ask who owns the
  focused window. `source_app` is `None` there, so the ignore list falls back to
  the concealed-type markers, which KeePassXC and friends do set.
- **`xsel` cannot carry images.** The backend returns
  `UnsupportedContent` rather than writing something wrong.
- **History is not encrypted at rest.** It is `0600` in the user's data
  directory, which matches how a browser stores its history, but full-disk
  encryption is doing the real work. SQLCipher is on the roadmap.
- **The trial timer is trivially resettable.** By design. See
  [licensing.md](licensing.md).
