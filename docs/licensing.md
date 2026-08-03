# Licensing

Pasteport is paid software with an open codebase. This document covers the key
format, how to generate keys, and what the trial does and does not enforce.

## The model, stated plainly

| | |
|---|---|
| Source | AGPL-3.0-or-later. Read it, patch it, redistribute it under the same terms |
| A build you compile | Fully functional. No feature gates |
| A build we ship | Signed, notarized, and gated on a license key after 14 days |
| Verification | Offline, Ed25519. No network code exists in the product |
| Trial | 14 days, courtesy timer, resettable by deleting a file |

If someone compiles from source rather than buying, that is a supported path, not
piracy. The paid artifact is the signed binary plus support.

## Why the trial is not locked down

The trial record is a JSON file in the data directory. Deleting it starts a fresh
14 days. This is known and intended.

Every technique for preventing it — hardware fingerprinting, hidden state
outside the data directory, a server check — has the same profile: it costs
honest users something real (a reinstall reads as piracy, a laptop swap needs a
support ticket, an offline machine cannot start), and it delays a determined
person by an afternoon. On an AGPL codebase where the check is visible in
`trial.rs`, it would delay them by less.

So the trial is a reminder, not a lock. The things it will not do are also
deliberate: it does not phone home, it does not hash your hardware, and a wrong
system clock cannot lock you out (`is_active_at` treats a backwards clock as
still-in-trial).

## Key format

```
PP1.<base64url(payload)>.<base64url(signature)>
```

Three dot-separated segments, no padding:

- `PP1` — format version. A future scheme can ship as `PP2` without breaking old
  keys.
- payload — the JSON below, exactly as signed.
- signature — 64-byte Ed25519 signature over the payload bytes.

Payload:

```json
{
  "v": 1,
  "id": "lic_a1b2c3",
  "email": "buyer@example.com",
  "plan": "personal",
  "seats": 1,
  "issued_at": 1770000000,
  "expires_at": null
}
```

| Field | Meaning |
|---|---|
| `v` | Payload version. A build rejects anything it does not understand rather than guessing |
| `id` | Opaque id for support lookups |
| `email` | Licensee, shown in the about box |
| `plan` | `personal`, `team`, or `lifetime` |
| `seats` | Seat count for `team`, otherwise 1 |
| `issued_at` | Unix seconds |
| `expires_at` | Unix seconds, or `null` for perpetual |

Whitespace and line breaks are stripped before parsing, so a key that picked up
newlines in an email still works.

### Verification order

`License::verify` checks the signature **before** deserializing the payload into
anything meaningful, and before any policy check. A tampered payload fails at the
signature, so no untrusted field ever reaches the expiry logic.

Tests in `license.rs` cover the cases that matter: a re-encoded payload keeping
the original signature, a key signed by a different keypair, a truncated
signature, and six shapes of malformed input.

## Generating keys

Signing lives behind the `mint` feature, which release builds do not enable. A
shipped binary contains no signing code.

### Create a signing keypair

```bash
cargo run -p pasteport-license --features mint --bin pasteport-mint -- keygen
```

Output:

```
seed (SECRET, keep offline):  <base64url>
public key (compile into builds):  <base64url>
```

The seed is the private key. It should live in a password manager or an offline
machine, never in this repository. `.gitignore` covers `*.pem`, `signing-key*`,
and `.env` as a backstop, but the real protection is not putting it here.

### Sign a license

```bash
PASTEPORT_SIGNING_SEED=<base64url-seed> \
cargo run -p pasteport-license --features mint --bin pasteport-mint -- sign \
  --email buyer@example.com \
  --plan personal \
  --days 365
```

Omit `--days` for a perpetual key. Use `--plan team --seats 10` for team keys.

### Release builds

The public key is baked in at compile time:

```bash
PASTEPORT_LICENSE_PUBKEY=<base64url-public-key> cargo build --release
```

Without that variable, `EMBEDDED_PUBKEY_B64` is `None`, licensing reports
`Status::SelfBuilt`, and the build is unlicensed but fully functional. That is
what every contributor's `cargo build` produces.

An invalid value is treated as *no* key rather than as a key that matches
nothing, and it logs an error — a typo in the release pipeline must not silently
unlock every build.

## Statuses

`pasteport status` reports one of:

| Status | Functional | Meaning |
|---|---|---|
| `Licensed` | yes | Valid key installed |
| `Trial` | yes | Inside the 14 days. Prompts at 3 days left |
| `Expired` | no | Key verified but its term ended |
| `TrialExpired` | no | 14 days up, no key |
| `Invalid` | no | A license file exists but does not verify |
| `SelfBuilt` | yes | No verifying key compiled in |

A valid key always supersedes the trial, expired or not, because the message the
user sees should reflect that they paid.

## What still works unlicensed

`Ping`, `Status`, `LicenseInstall`, and `LicenseRemove` are never gated. Someone
whose trial lapsed must be able to start the daemon, see why it is refusing, and
paste in a key. Gating the very command needed to fix the problem is a support
burden with no upside.

Everything else returns:

```
Trial expired; a license is required. Run `pasteport license install <key>` to continue.
```

## Installing a key

```bash
pasteport license install PP1.eyJ2Ijox...
```

The key is verified before it is written, so a bad paste never replaces a working
license. Stored at `license.key` in the data directory, mode `0600`.

```bash
pasteport license status
```

```bash
pasteport license remove
```

Removing falls back to the trial clock, which may already have expired.
