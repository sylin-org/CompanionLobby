# ADR 0001: Seven nouns, two contracts, one process

Status: accepted. Of the defaults listed at the end, 1 and 3 are confirmed by
the owner; the others await the owner's confirmation.

## Problem

Companion Lobby is ssh-agent for companions. A mind, run by an MCP host,
attaches as a companion and acts on services with accounts the sponsor signed
in to. The lobby holds the account material, decides what each attachment may
reach, and keeps an attributed record of everything that happened.

## Decision

### The seven nouns

| Noun | Meaning | Carried by |
| --- | --- | --- |
| Sponsor | The person who owns the companions and decides what they may reach. | `Sponsor` |
| Companion | A named identity: moniker, display name, portrait, sponsor. | `Companion`, `CompanionId` |
| Mind | A model or agent run by an MCP host, known to the lobby only through its attachment. One attachment is one companion. | `Attachment`, `AttachmentId` |
| Credential | An account the sponsor signed in to for a companion. The record never holds material. | `Credential`, `CredentialId` |
| Service | Something a companion reaches, spoken to by a service adapter in its own words. | `ServiceName` |
| Connection | The sponsor's decision that a companion may use one credential to reach a service at an address. | `Connection`, `ConnectionId` |
| Journal | Attribution: who did what, when, through which connection, with which outcome, named as the sponsor read it then. | `JournalEntry` |

These words are the whole product vocabulary. The mind-facing surface is
exactly `reach`, `<service>_<tool>` with an optional `at` argument for services
with many addresses, and later `attention`. Every outcome, from every adapter
and from the daemon, is one value of a single `Outcome` enum that cannot carry
provider text.

### Two contracts

`lobby-contracts` declares the nouns and two synchronous, `Send + Sync`
contracts:

- `CredentialAdapter`: kind, begin and complete sign-in (signing an account
  in again from its own material), let go of a sign-in the lobby stopped
  waiting for, authorize an ask (a proof for an audience and method) against
  a `MaterialSlot`, renew the material when it is due, and revoke at the
  provider. Each ask shape joins the contract with the first adapter that
  answers it; a proof of one request joins with the first service adapter
  that asks for it.
- `ServiceAdapter`: name, describe (the provider kinds it accepts and the
  addresses it may reach), establish a connection at an `Address` through an
  `Authority`, catalog, and call.

The daemon implements `MaterialSlot` and `Authority`. Material is a
sealed blob only its own adapter decodes; a service adapter never holds it,
and asks the `Authority` for an `Authorization` in the shape it needs, which
the credential adapter mints on demand. A connection keeps only what the
service issued. Through the slot an adapter reads its material, marks it
rotating durably before it spends a provider's single-use secret, clears
the mark when nothing was spent, and commits a `MaterialUpdate`, which lands
only on top of the material it was made from, once, and never with more
permissions than the credential holds; a renewal that ends with its mark in
place leaves the credential needing sign-in, and its old secret is never
offered again. The contracts grow as the slices that need more arrive:
portraits read from a provider, the tools a connection's authority allows,
how a service knows the companion through a connection, and pushed events
each join with their first user. The nouns themselves are
declared with the crate: `Companion` with its moniker and display name,
`Attachment`, `JournalEntry` with its kinds, its initiator, the
`ConnectionId` it went through and the names of what it concerns as of the
moment it was written, `Outcome`, `ToolDescriptor`, and `ToolResult`, whose
text marks quoted third-party passages and renders within a fixed budget.
Beyond them, a type enters the contracts crate only when both synthetic
adapters in the test kit need it in the same change; there is no shared
vocabulary of service nouns, and service adapters may duplicate each other.

### Crates and dependency direction

| Crate | Path | Depends on |
| --- | --- | --- |
| `lobby-contracts` | `crates/contracts` | `serde_json` only, for the JSON values that tool descriptors, arguments and results carry |
| `lobby-outbound` | `crates/outbound` | contracts, `ureq` |
| `lobby-testkit` (unpublished) | `crates/testkit` | contracts, outbound |
| `lobby-credential-atproto` | `crates/credentials/atproto` | contracts, outbound |
| `lobby-service-tangent` | `crates/services/tangent` | contracts, outbound |
| `lobby-service-bluesky` | `crates/services/bluesky` | contracts, outbound |
| `lobby-daemon` (lib + `companion-lobby` binary) | `crates/daemon` | contracts, outbound, the adapters; the test kit only under the `synthetic` feature |
| `lobby-conformance` (unpublished) | `crates/conformance` | contracts, daemon, testkit |

Adapters and the daemon use the test kit as a dev-dependency. The daemon's
test build also turns on its own `synthetic` feature, which builds a second,
test-only command, `companion-lobby-synthetic`: the same daemon and shim,
with a registry of its own composed from the test kit's synthetic adapters,
so the conformance suite's host scenarios run them through a real binary.
The product registry never names them, and `companion-lobby`, even built
with every test feature on, contains neither. A second service
adapter on the same atproto credential, `lobby-service-bluesky`, followed
the first once it passed the conformance suite.

### One process, an event-driven monolith

1. One command pipeline: every change validates, applies, commits, journals,
   then emits events.
2. One event pipeline: small reactive handlers turn events into commands and
   refresh projections.
3. Projections hold no truth. The state file is truth; the journal is
   attribution.
4. The async runtime and the HTTP server live only in the daemon crate.
5. Adapters are blocking and run behind `spawn_blocking` with no lock held.

One binary, `companion-lobby`, has the subcommands `run` (the default),
`attach <companion>`, `open` and `stop`, so a host configuration names one
path and the shim can start the daemon from its own executable.

### Attachment secret and same-machine trust

- The daemon listens on 127.0.0.1 only. The Host header is checked before a
  body is read.
- Attaching presents a per-install attach secret, kept in the data directory
  under an owner-only ACL and sealed with DPAPI in the user's scope on
  Windows (mode 0600 elsewhere). The daemon replaces a secret that does not
  unseal as this user's or that anyone but its owner may read, and refuses to
  narrow a widened directory whose files carry access of their own. The daemon
  answers each attach with a token for that one attachment, held only in its
  memory, and every later frame and the detach present that token, never the
  secret. `/mcp` refuses browser-origin evidence. This keeps journal
  attribution honest.
- Before sending the secret, the shim verifies that the listener's pid is the
  pid the lock holder recorded in `daemon.json` and that the store
  fingerprint matches, and then that the far end of the very connection it
  writes on belongs to that pid, so a process squatting on the port never
  receives it, even one that takes the port between the check and the
  connection; a process that takes the port later receives only tokens no
  daemon accepts. A stopping daemon keeps the port bound until it has
  drained.
- The manager page and its operator API run under same-machine trust for v1:
  any local user who can reach the loopback port can drive it, as with
  ssh-agent. Mutations require same-origin browser evidence and JSON. An
  operator cookie bootstrapped from `companion-lobby open` is deferred, not
  rejected.

### Data directory and secrets at rest

- Windows: the Local AppData known folder resolved with `SHGetKnownFolderPath`,
  never from environment variables, plus `CompanionLobby`. macOS:
  `~/Library/Application Support/CompanionLobby`. Elsewhere:
  `$XDG_STATE_HOME/companion-lobby`. `LOBBY_DATA_DIR` overrides.
- Any path whose final component is `.companion-lobby` is refused outright and
  never read.
- The directory has an owner-only ACL. Credential material and service-issued
  data are sealed per file with DPAPI in user scope on Windows and mode 0600
  elsewhere. Sealed blobs do not survive a profile reset or a move to another
  machine; the sponsor signs in again.
- The only environment variables read are `LOBBY_DATA_DIR`, `LOBBY_PORT`,
  `LOBBY_NO_BROWSER` and `LOBBY_AUTHSERVER`.

### State file and journal

- `lobby.json` is the truth, at schema 1: camelCase, unknown fields refused,
  atomic writes, one commit with rollback. A file the daemon does not
  understand is refused and never rewritten.
- Forward schema policy: each release reads its own schema and the predecessors
  it documents, writes only its own schema, and refuses a newer one. There is
  no decoder for any file the product itself never wrote.
- The journal is append-only JSON lines. An `invoked` entry precedes every
  external call (no entry, no call); a `settled` entry follows it. Every
  command writes a `pending` entry before it changes anything and an entry
  with its outcome after; a failure of the second never reports a landed
  change as an error. Entries hold no content and no secrets. Each carries
  the names the sponsor read when it was written (the companion's display
  name, the account's label and its kind's title, the connection's label and
  its service's title), so its sentence reads the same after a rename and a
  removed companion keeps its history by name.

### The ship line

v0.1.0 is the Tangent service adapter (reading, creating a post and starting a
topic; edit, delete, membership, watch and read position included; moderation
and management tools last within the slice) reached through Claude Desktop,
Claude Code and Codex on Windows with the owner's real atproto account.
Portraits the sponsor uploads exist. The Bluesky service adapter is built
on the same atproto credential as Tangent's and passes the conformance
suite; it is reached by real users only once the owner's live check in its
documentation passes. The `attention` tool, portraits read from an
account's profile and the Windows tray come after v0.1.0.

## Consequences

- Provider and service words stay inside their adapters and their own docs;
  the vocabulary lint in `tools/` enforces this and the forbidden names.
- Conformance suites for both contracts run through the real daemon and the
  real binary against local fakes; no test reaches the live network.
- The workspace denies `unsafe_code`, except one Windows module in the daemon
  (`win32.rs`) for the calls the standard library has no safe form of: the
  standard handles' inherit flag, the data directory's DACL and the exclusive
  listening socket. Clippy warnings fail the build.

## Defaults

Each item below is the recommended answer, taken as the default until the
owner confirms or changes it. Items marked confirmed are the owner's
decision.

1. **Reference material stays local.** Confirmed by the owner: the reference
   material was never published. The read-only reference branch and its
   tags are never pushed; main's pushed history carries none of what they
   hold.
2. **Linear main.** Main stays linear: the first greenfield commit sits on a
   tree-clearing commit, and nothing is force-pushed.
3. **Grants held by earlier builds.** Confirmed by the owner. The owner
   revokes the app authorizations for both accounts in the authorization
   server's connected-apps settings, signs in again through the lobby, and
   then deletes the files in `~/.companion-lobby` by hand. The lobby never reads that directory; no
   one-off tool is built for it.
4. **Attachment and operator trust.** A per-install attach secret (owner-only
   ACL, DPAPI-sealed) on every attach, and same-machine trust for the operator
   page in v1.
5. **Data directory.** `%LOCALAPPDATA%\CompanionLobby` through
   `SHGetKnownFolderPath`, `LOBBY_DATA_DIR` as the override, and a hard refusal
   of any path ending `.companion-lobby`.
6. **Credential ownership.** A companion holds at most one account per
   provider; an account belongs to exactly one companion; removing a
   credential frees the account for another companion; revoking it while
   keeping the record keeps the account reserved to that companion.
7. **MCP hosts for the ship line.** Claude Desktop, Claude Code and Codex on
   Windows, each recorded on day 1 of slice 1; the protocol revision they
   speak first is built first and the other is added later. Every host
   attaches through the shim; none attaches over HTTP directly.
8. **Ship line.** v0.1.0 as described above; sponsor portraits exist, and
   Bluesky, the `attention` tool, profile portraits and the tray come after.
9. **Tangent connection.** A proof signs the account in as a browser's
   sign-in does, and the connection keeps the Space's cookies for itself; what
   the companion may do there is what the Space's pages offer it. The derived
   did:web audience is built first; the did:plc DID-document proof path is
   required for the ship line only if the owner's live Tangent uses it.
10. **Journal retention.** Five rotated generations of 1 MiB each, with no
    age-based deletion.
11. **Portrait source.** Sponsor upload in the manager, plus the account's
    profile avatar read by the credential adapter as an opt-in.
12. **Discovery document.** Port 5219 and the existing fields kept compatible
    (`product` `companion-lobby`, `discoveryVersion` 1, `managerOrigin`), with
    new fields only added and cross-origin headers on that one document only,
    so the Tangent page's detection script keeps working; re-verified under
    Chrome's Local Network Access prompt before the ship line.
13. **License.** MIT, "Copyright (c) 2026 Leo Botinelly".
14. **Coordinated change in the Tangent repository.** Its uncommitted work is
    snapshotted on a local branch first, the change is prepared on a branch
    during slice 6 and merged after v0.1.0, and that repository's agent guide
    is rewritten on the same branch. The owner merges it.
