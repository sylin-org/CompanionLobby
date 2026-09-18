Finish the CompanionLobby migration: restore a compiling, fully working connector with the `companion-lobby` identity, keeping the current tool set and wire protocol unchanged (no ADR-0012 realignment, no R6.2 renames in this pass).

## Step 0 — Baseline
- `git add -A && git commit` the current untracked state as a snapshot (repo has zero commits; makes everything reversible). Add a `.gitignore` for `/target`.

## Step 1 — Corrected trait layer in `companion-core`
Rewrite `crates/core/src/traits.rs` to the proven protocol model (the current signatures assume an imaginary per-request-PDS-token auth model):
- `AuthAdapter`: `name()` + `get_service_auth(auth_state, audience) -> Result<String, ExperienceError>` (drops `authenticate`/`refresh`/`sign_request` from the trait; they stay inherent methods).
- `ServiceAdapter`: `name()`, `as_social_browsing()`, `as_social_moderation()`, plus `discover(origin, path)` (anonymous GET), `exchange(origin, path, body, bearer)` (proof-bearer POST), `server_card(origin)` (GET `/api/server`, short timeout), `probe_page(origin)` (GET `/api/discovery` + product check).
- `SocialBrowsing`: `get(context, path)` / `send(context, method, path, body)` taking `&RequestContext` — the enrollment-session credential as Bearer, exactly as the old hub did.
- `SocialModeration`: the four named case methods (`list/read/preview/apply`) against the endpoints scripted in `tests/common/mod.rs:852-862`.
- All results use `ExperienceError` (the hub's error mapping at hub.rs:2173/2650+ already depends on `DpopChallenge`).
- Move `CONNECTOR_PRODUCT` into core (single source; today it's cli-only and the tangent crate doesn't compile).

## Step 2 — Fix the adapter crates
- `adapter-service-tangent`: keep `experience.rs` (the complete proven client) and implement the corrected traits by delegating to it in `lib.rs`; delete the wrong `/.well-known/mcp-companion` calls, the six `not implemented` stubs, and the per-request `get_service_auth` misuse. Crate/dir name stays — it names the remote service, like `bluesky`.
- `adapter-auth-atproto`: fix `get_service_auth` to use `EXCHANGE_LXM` (not `com.tangent.exchange`), keep aud/exp percent-encoded, and add the single DPoP-nonce-challenge retry (fake PDS at `tests/common/mod.rs:745` challenges first).
- `adapter-service-bluesky`: keep as a documented stub member.

## Step 3 — Hub: wire the registries, fix call sites
- Add builder registration (`with_service(Arc<dyn ServiceAdapter>)`, `with_auth(Arc<dyn AuthAdapter>)`) inserting into the existing registry maps (hub.rs:111-112).
- `build_hub` (crates/cli/src/lib.rs) installs the real `UreqExperience` + `AtprotoOauth` adapters.
- Rewrite the ~25 `services.get("tangent").unwrap()` call sites (hub.rs:351…1840) to the corrected traits: browsing reads/writes via `get`/`send` with paths re-derived from the fake server's scripted routes; enrollment step 3 via `exchange(canonical, "/mcp/token", …)`; discovery via `discover(canonical, "/.well-known/tangent-mcp")`; page probe via `probe_page`; server cards via `server_card`. Delete the dead `auth_path` string (hub.rs:786-791).

## Step 4 — Local identity rename to `companion-lobby`
- Env vars: `COMPANION_LOBBY_HOME` (lib.rs:21), `COMPANION_LOBBY_PORT` (manager.rs:90), `COMPANION_LOBBY_NO_BROWSER` (browser.rs:33), document `COMPANION_LOBBY_AUTHSERVER` (atproto_oauth.rs:57). Data dir `~/.companion-lobby`.
- `usage()` (main.rs:62-86): header `companion-lobby`, `operator`→`manager`, real env names (replacing the literal `crate_HOME`/`crate_PORT`/`crate_NO_BROWSER` placeholders), fix the duplicated `companions` line to document `enrollments`, fix the "Use enroll …" pointer (main.rs:366).
- MCP `serverInfo.name` → `companion-lobby` (mcp.rs:22); thread names, tray tooltip, lockfile/diagnostics messages, `manager.html` visible copy ("Tangent"→"Companion Lobby").
- Unchanged (wire/protocol, per parity scope): tool names, `/api/v1/experience/*` paths + JSON fields, `local.tangent.mcp.exchange`, `/.well-known/tangent-mcp`, `/mcp/token`, `ts_` prefix, `CONNECTOR_PRODUCT` value `tangent-space-connector`, `bsky.social` defaults, `tangentRef` vocabulary.

## Step 5 — Revive the tests
- Move `tests/` → `crates/cli/tests/` (root manifest is virtual, so they currently never compile).
- Rewrite imports to the new crates (`companion_lobby::*`, `companion_core::*`, `adapter_service_tangent::experience`, `adapter_auth_atproto::*`); fix `ConnectorHub::new` to the 3-arg form; install a thin fake `ServiceAdapter` over the existing fake port via the new seam; `CARGO_BIN_EXE_companion-lobby`; renamed env vars; temp-dir prefixes.
- Update assertions that pin identity strings (serverInfo name, help text) — keep wire-string assertions as-is.

## Step 6 — Cleanup and docs
- Drop cli's dead `ureq` dep and core's unused `uuid` dep; refresh stale module docs (cli lib.rs:1, adapters/mod.rs, application/mod.rs, core intake.rs:17 "operator"→"manager", manager.rs:132).
- Rewrite README for CompanionLobby: correct crate layout, binary/env/dir names, tool list matched to the actual catalog (14 tools + the 4 stewardship-gated moderation tools), and repoint the dangling `../../../docs/...` links at the sibling `../tangent-space/docs/...`.

## Verification
`cargo build`, `cargo test` (all suites green, including the real-binary stdio journey), `cargo clippy` on the `deny(unsafe_code)` workspace. Fixtures in `tests/common/mod.rs` are the behavioral oracle throughout.