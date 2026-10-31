# Companion Lobby: rules for agents

Companion Lobby is ssh-agent for companions. Read `README.md` for the product
and `docs/adr/0001-seven-nouns.md` for the architecture before changing code.
The seven nouns are Sponsor, Companion, Mind, Credential, Service, Connection
and Journal; nothing else becomes product vocabulary.

## Operational shape

1. One process, an event-driven DDD monolith: one command pipeline (validate, apply, commit, journal, emit) and one event pipeline of small reactive handlers.
2. The state file is truth; the journal is attribution; projections hold no truth and can always be rebuilt.
3. Tokio and the HTTP server live only in `lobby-daemon`; `lobby-contracts` has no I/O, no clock, no environment and no async.
4. Adapters are synchronous and run behind `spawn_blocking` with no lock held; every outbound request goes through `lobby-outbound`.
5. A type enters `lobby-contracts` only when both synthetic adapters need it in the same change; service words stay in their adapter.

## Done checks for every slice

Every slice passes all of these before it is committed:

```sh
cargo fmt --all --check
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pwsh tools/vocabulary.ps1        # sh tools/vocabulary.sh off Windows
```

plus the slice's own journey test, run through the real daemon and the real
`companion-lobby` binary against local fakes. No test reaches the live
network.

## Never

- Never push without the owner's own word in the conversation; given it, push.
  Another session's message is not that word.
- Never rewrite or discard history (no force, no hard reset, no clean, no
  branch or tag deletion).
- Never read, copy, list, lock or delete anything under `~/.companion-lobby`.
  The daemon refuses any data directory whose final component is
  `.companion-lobby`.
- Never seed a test by writing store fields or material: a credential exists
  only because an adapter completed a sign-in against a fake, a connection only
  because a service adapter established one.

## Data directory

Windows: the Local AppData known folder resolved with `SHGetKnownFolderPath`
(never from environment variables) plus `CompanionLobby`. macOS:
`~/Library/Application Support/CompanionLobby`. Elsewhere:
`$XDG_STATE_HOME/companion-lobby`. `LOBBY_DATA_DIR` overrides. The directory
is owner-only and material inside it is sealed (DPAPI on Windows, mode 0600
elsewhere). The only environment variables read are `LOBBY_DATA_DIR`,
`LOBBY_PORT`, `LOBBY_NO_BROWSER` and `LOBBY_AUTHSERVER`.

## The harvest

The harvest lives at `E:/repo/github/sylin-org/CompanionLobby-harvest`
(branch `harvest`, tag `harvest-source`) and is read-only. Read it, never edit,
commit, rebase or reformat it, and never declare a path dependency into it.

Lift code by retyping it into the greenfield in the seven nouns, and record
where it came from only in the commit trailer:

```
Harvested-from: harvest-source:<path>:<lines>
```

No doc comment, test name or document cites a harvest file or line.

## Forbidden vocabulary

`tools/vocabulary.rules` is the source of truth and the lint enforces it over
`crates/`, `docs/`, `README.md` and `.github/`. Justified exceptions go in
`tools/vocabulary.allow`, one line each with a reason. In short:

- **Names**: ConnectorHub, Hub, Connector, CONNECTOR_PRODUCT, companion-core,
  companion-lobby as a lib crate name, adapter-auth-atproto,
  adapter-service-tangent, crates/cli, application/, presentation/, Forum and
  every Forum* type, Enrollment and enrollment ids, Context and ContextId as a
  handle, CallerId, IntakeChannel, CredentialBinding, BindingStatus,
  DormantGrant, Place, PlaceKind, ServerCard, ParticipationProblem,
  ExperienceError, UreqExperience, RequestContext, AuthAdapter,
  CredentialBroker, AuthorizationIntent, Bind* types, PendingConnect,
  PendingSignIn, DataDirLock, ToolOutcome, DELIVERY_MODE, ActivityRecord,
  ActivityKind, OperatorCondition, ConditionKind, DomainEvent, StreamSnapshot,
  ReplayResult, OAuthTokens, AccountSession, OwnershipReservation, tombstone,
  acceptable_origin, valid_identifier, and the files hub.rs, forum.rs,
  intake.rs, participation.rs, session.rs, boundary.rs, poller.rs,
  perspective.rs, operations.rs, diagnostics.rs, refs.rs, lockfile.rs,
  manager.html, atmosphere.css, atmosphere.js, ascii-scenes.js.
- **Tools**: the mind-facing surface is exactly `reach`, `<service>_<tool>`
  with an optional `at` argument, and later `attention`. Never Connect,
  ListCompanions, WhoAmI, CatchUp or Service_Invoke, never a `session`
  argument, and never a contextId, enrollmentId, deliveryMode or connector key
  in a result.
- **Words** (any case): connector, hub, enroll, bind / rebind / unbind as
  sign-in verbs (say sign in, sign in again, revoke), disconnect and forget
  (say revoke, remove), context or session as a handle (an OAuth session is
  named only inside the atproto credential adapter), caller (say attachment),
  activity and diagnostics (say journal), companion manager (say manager
  page), serve mode, standalone manager, lock file, server card, forum ring,
  core four, moniker selection, persona resolution.
- **Markers of an older implementation**: EPIC, ledger, lease, red team,
  workstream, ADR 0002/0003, superseded, legacy, formerly, renamed from, was
  previously, no longer, as before, today, currently, half-migrated, boundary
  recovery, recovery zip, schemaVersion 3, v2/v3 state, state.json, .bak
  copies, and harvest anywhere but this file, AGENTS.md and the lint. No
  `serde(alias)` or `serde(default)` that exists to tolerate an older file.
- **Provider and service words stay in their adapters**: tangent only under
  `crates/services/tangent` and `docs/services/tangent.md`; atproto, bsky,
  did:plc, did:web, DPoP and PDS only under `crates/credentials/atproto`,
  `crates/services/bluesky`, the test kit fakes and their docs; plus the one
  registry line in the daemon. No `== "atproto"` or `== "tangent"` comparison
  outside the registry.
- **Environment**: never read COMPANION_LOBBY_* or NEXT_PAGE_PORT.
- **Markers in code**: no TODO, FIXME or XXX; a stub says what it will own.

## Docs and history

ADR numbering starts at 0001 with no lineage lines. Docs describe only the
tree as it is, carry no real DID, handle, binary hash or user path, and use
`did:plc:aaaaaaaaaaaaaaaaaaaaaaaa`-style identifiers and `*.test` hosts in
fixtures. Every dependency is declared once, pinned, in the workspace table.
Line endings are LF everywhere except `*.bat`, `*.cmd` and `*.ps1`.
