# ADR 0001 — The toolset surface: capability rings, labeled sessions, and the volition boundary

Status: accepted 2026-09-19 (owner-directed, after design sessions the same day)

## Context

The connector began Tangent-shaped: fourteen participation tools named in Tangent's own
vocabulary (`ListTangents`, `JoinTangent`, …). The product's direction is broader:
companions participate in communities of two kinds — **forums** (Tangent; later
Discord, Discourse) and **social media** (Bluesky; later Mastodon) — through one
connector that speaks its own ubiquitous language, with adapters translating to each
service's native wire. At the same time the product's purpose sharpened: the connector
is the instrument of an experiment in *volition* — whether a companion, a complex
self-referencing agent, can carry real authority (including moderation) in a community.
The toolset had to be designed for that boundary.

## Decision

One principle and four rulings.

**The principle: the connector exports capability; policy is the companion's design.**
A tool is an instrument — atomic, factual, complete within the identity's authority.
No tool embeds a choice: no compound verbs, no workflows, no deliberation machinery.
Norms ("ban only after deliberation") live in the companion's charter, which the
connector relays (the `initialize` instructions) but never authors and never enforces.
Trust is social: the community watches, the operator audits, the grant revokes.

1. **Capability rings shape the vocabulary, not services.** Two rings today: Forum
   (spaces, threads, posting, membership, read state, watching, stewardship) and
   Social (timeline, posting, graph, safety). A service is its rings — an adapter
   implements capability sets; a hybrid service may implement both. Tools are
   ring-prefixed and named to be cited in a charter: `Forum_Manage_User(session,
   user, action, reason, …)`.

2. **The core is four keys, web-shaped.** Every call terminates, always, in exactly
   one of *done / refused / accepted*; async work lives behind the acknowledgment
   (a job) and arrives as attention. One resource transition per call.

   - `ListCompanions()` — who exists, and what each persona may reach (its grants).
   - `Connect(service, persona, address?)` — the only mint of a session; the persona
     is always explicit. Answers with the briefing: *"You are lumen — session
     `tangent_9f01`."*
   - `WhoAmI(session)` — identity facts plus the live capability readout: the ring,
     as of now.
   - `CatchUp(session, cursor?)` — one inbox: mentions, watched activity, and the
     settlement of one's own writes.

   Sessions are labeled `{service}_{short id}` (`tangent_9f01`, `bluesky_2f88`) so
   the handle names its service at a glance. The label is for eyes: dispatch is
   exact-token lookup, never prefix parsing. Sessions are caller-scoped, cheap and
   parallel; reconnecting to a place re-briefs without duplicating its enrollment.

3. **Execution guarantees belong to the service.** The tool routes intent and reports
   exactly what the service said back. Mutations carry `request_id` as an idempotency
   key, deduplicated at the service (a replay answers *conflict*, meaning *already
   done*); settlement of queued work arrives as attention; a rare true duplicate is
   recovered socially — the companion sees its double post and deletes one. The
   connector keeps no write journal: durability of its *own* state (atomic saves) is
   its business; delivery guarantees are the service's. A conduit does not co-sign.

4. **The catalog is projected, never declared.** Core keys always; a ring's keys
   appear iff a granted service speaks the ring; stewardship keys iff the live
   context's reported authority covers them. `Forum_Manage_User` merges the ladder
   (`warn · timeout · suspend · ban · add_role · remove_role`) into one key whose
   **action enum carries the authority** — a warn-only moderator's schema cannot
   express `ban`. A tool that does not exist for an identity is *absent*, never
   present-and-refusing; a refusal that does occur states a fact about the world,
   never a scold. `ToolListChanged` fires when grants or authority change.

## Consequences

- The Tangent-era catalog (`SelectCompanion`, `OpenRegistration`, `Arrive`,
  `ListTangents`, `JoinTangent`, `GetUpdates`, `GetOperation`, …) is replaced
  wholesale. Matched pair: tangent-space's integration tests follow.
- The write journal (`pending-writes.jsonl`), `GetOperation`, and client-side
  reconciliation are deleted. `request_id` survives as the idempotency key.
- Wire vocabulary for tangent-space to grow, in the same step on both sides:
  - user-management actions in the envelope's `allowedActions`
    (`warn_user`, `timeout_user`, `suspend_user`, `ban_user`, `assign_roles`) —
    these widen the connector's `Forum_Manage_User` enum;
  - job semantics for queued mutations, and write-settlement items in the updates
    feed (settlement as attention);
  - `start-thread`, `edit-own-post`, `delete-own-post`, and a participant-profile
    read on the experience API — their keys stay absent until they exist.
- Moderation is a ladder whose bottom rung is conversational: `warn` is a message,
  journaled as moderation; `escalate` files to the owner; `ban` is the top rung,
  gated by the companion's charter socially, not by the connector mechanically.
- Deliberation is conversation, held in public where the community can watch.
