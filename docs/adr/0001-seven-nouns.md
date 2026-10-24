# ADR 0001: Seven nouns, two contracts, one process

Status: accepted, with the defaults listed at the end awaiting the owner's
confirmation.

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
| Journal | Attribution: who did what, when, through which connection, with which outcome. | `JournalEntry` |

These words are the whole product vocabulary. The mind-facing surface is
exactly `reach`, `<service>_<tool>` with an optional `at` argument for services
with many addresses, and later `attention`. Every outcome, from every adapter
and from the daemon, is one value of a single `Outcome` enum that cannot carry
provider text.

### Two contracts

`lobby-contracts` declares the nouns and two synchronous, `Send + Sync`
contracts:

- `CredentialAdapter`: kind, describe, begin and complete sign-in, authorize an
  ask against a `MaterialSlot`, revoke, portrait.
- `ServiceAdapter`: name, describe, establish a connection through an
  `Authority`, catalog, available tools for the connection's authority, call,
  and an optional push stream into an `Ingress`.

The daemon implements `MaterialSlot`, `Authority` and `Ingress`. Material is a
sealed blob only its own adapter decodes. A type enters the contracts crate
only when both synthetic adapters in the test kit need it in the same change;
there is no shared vocabulary of service nouns, and service adapters may
duplicate each other.

### Crates and dependency direction

| Crate | Path | Depends on |
| --- | --- | --- |
| `lobby-contracts` | `crates/contracts` | nothing |
| `lobby-outbound` | `crates/outbound` | contracts |
| `lobby-testkit` (unpublished) | `crates/testkit` | contracts |
| `lobby-credential-atproto` | `crates/credentials/atproto` | contracts, outbound |
| `lobby-service-tangent` | `crates/services/tangent` | contracts, outbound |
| `lobby-daemon` (lib + `companion-lobby` binary) | `crates/daemon` | contracts, outbound, the adapters |
| `lobby-conformance` (unpublished) | `crates/conformance` | contracts, daemon, testkit |

Adapters and the daemon use the test kit as a dev-dependency. A second service
adapter on the same atproto credential, `lobby-service-bluesky`, is created
only after the first service adapter passes the conformance suite.

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
- Every attachment presents a per-install attach secret, kept in the data
  directory under an owner-only ACL and sealed with DPAPI on Windows. `/mcp`
  refuses browser-origin evidence. This keeps journal attribution honest.
- Before sending the secret, the shim verifies that the listener's pid is the
  pid recorded in `daemon.lock` and that the store fingerprint matches, so a
  process squatting on the port never receives it.
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
  external call (no entry, no call); a `settled` entry follows it. Entries hold
  no content and no secrets.

### The ship line

v0.1.0 is the Tangent service adapter (reading, creating a post and starting a
topic; edit, delete, membership, watch and read position included; moderation
and management tools last within the slice) reached through Claude Desktop,
Claude Code and Codex on Windows with the owner's real atproto account. The
Bluesky service adapter, the `attention` tool, portraits and the Windows tray
come after v0.1.0.

## Consequences

- Provider and service words stay inside their adapters and their own docs;
  the vocabulary lint in `tools/` enforces this and the forbidden names.
- Conformance suites for both contracts run through the real daemon and the
  real binary against local fakes; no test reaches the live network.
- The workspace denies `unsafe_code`; clippy warnings fail the build.

## Defaults awaiting the owner's confirmation

Each item below is the recommended answer, taken as the default until the
owner confirms or changes it.

1. **Reference material stays local.** The read-only reference branch and its
   tags are never pushed; main's pushed history carries none of what they
   hold.
2. **Linear main.** Main stays linear: the first greenfield commit sits on a
   tree-clearing commit, and nothing is force-pushed.
3. **Grants held by earlier builds.** The owner revokes the app authorizations
   for both accounts in the authorization server's connected-apps settings,
   signs in again through the lobby, and then deletes the files in
   `~/.companion-lobby` by hand. The lobby never reads that directory; no
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
8. **Ship line.** v0.1.0 as described above; Bluesky, the `attention` tool,
   portraits and the tray come after.
9. **Tangent connection defaults.** Request the server's default grants
   (welcome, read, post) with a 7-day lifetime; the `manage` grant and the
   lifetime are per-connection sponsor choices in the manager. The derived
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
