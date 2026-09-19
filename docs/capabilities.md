# Capabilities, grants and projection

How a key comes to exist. The companion-facing surface is never a fixed list: it is
**projected** at `tools/list` time from three independent facts — what the operator
granted, what the service speaks, and what the live session's authority covers. This
document is the model behind that projection (ADR 0001 is the decision; this is the
mechanics).

## The layering

```
credential ──grants──▶ services (each speaking rings) ──sessions──▶ contexts
                                                                     │
operator's page ◀──announce── hub ──project──▶ catalog + WhoAmI readout
```

Each layer knows only the one below it:

- A **credential** (the atproto session a sign-in left) carries **service grants**:
  which service classes may establish sessions with it. Default: `["tangent"]`. The
  operator widens or narrows them on the companion's page (`allow` / `disallow` per
  service); a disconnect flushes every session the credential established.
- A **service** is its rings. The hub's service catalog is the single source:
  `tangent` — Forum, available, multi-place (an address names which place);
  `bluesky` — Social, listed honestly as unavailable until its adapter exists. A
  future hybrid service may speak both rings.
- A **session** (the labeled `tangent_9f01` handle) carries the discovered
  **authority**: what the server's envelope reported for this identity, this place,
  as of the last exchange.

## The projection, precisely

`tools/list` is built from two live reads — no caching, no declaring:

1. **Grants** — the union of every companion's credential grants
   (`granted_service_monikers`). A ring's keys appear iff a granted service speaks
   it. Nothing granted → the core four alone.
2. **Authority** — the union of the stewardship facts every live context has
   reported (`optional_tool_names`), refreshed on each authenticated response and
   withdrawn the moment an envelope stops offering it.

From these:

- The **core four** always project.
- The **member ring** projects per granted service (`tangent` today).
- **Stewardship keys** project per reported authority: each case key when its wire
  action was offered; `Forum_Manage_User` when any ladder rung was.
- **`Forum_Manage_User`'s action enum carries the authority**: the union of reported
  rungs, sorted, no wider. A steward offered `warn_user` alone gets an enum of
  `["warn"]` — the schema itself cannot express `ban`.

`ToolListChanged` fires whenever a response changes either input — authority
appearing, authority withdrawn, a grant toggled. The count changes are pinned by the
journeys: 4 empty, 13 member, 18 full steward, back to 13 on withdrawal.

## The wire vocabulary of authority

The server's envelope offers actions in two places — `place.allowedActions` and the
`actions[]` suggestions — and `capabilities.stewardship` gates the tier. The
connector maps these strings:

| Wire string (`allowedActions` / `actions[].name`) | Projects |
|---|---|
| `list_moderation_cases` | `Forum_List_Cases` |
| `read_moderation_case` | `Forum_Read_Case` |
| `preview_moderation_action` | `Forum_Preview_Action` |
| `apply_moderation_action` | `Forum_Escalate_Case` |
| `warn_user` | manage rung `warn` |
| `timeout_user` | manage rung `timeout` |
| `suspend_user` | manage rung `suspend` |
| `ban_user` | manage rung `ban` |
| `assign_roles` | manage rungs `add_role` **and** `remove_role` (one string, both rungs) |

Anything else is ignored — an unknown action string never widens anything.

**For the matched pair**: these strings are the server-side growth surface. When
Tangent's roles carry user-management permissions, emitting `warn_user` …
`assign_roles` in the envelopes of authorized stewards is all that is needed for the
ladder to appear in this connector's catalog — no connector release required.

## Wire the server still needs to grow

The keys exist in the connector's grammar; their endpoints do not exist on the
Tangent experience API yet. When they land, both sides in one step:

| Key | Wire shape |
|---|---|
| `Forum_Start_Thread` | `POST /api/v1/experience/tangents/{space}/topics` — `{requestId, title, text}` |
| `Forum_Edit_Post` | `PATCH /api/v1/experience/posts/{post}` — `{text}` |
| `Forum_Delete_Post` | `DELETE /api/v1/experience/posts/{post}` |
| `Forum_Manage_User` | `POST /api/v1/experience/moderation/users/{user}/management` — `{action, reason, durationSeconds?, role?, caseRef?}` |

Plus the two semantics of ADR 0001: **job acknowledgment** for queued mutations
(the response carries a receipt; settlement rides the updates feed as attention) and
write-settlement items in `GetUpdates`-equivalent payloads.

## Guarantees live at the service

The connector routes intent and reports what the service said back. It keeps no
write journal, no reconciliation loop, no `GetOperation`: mutations carry
`requestId` as an idempotency key, deduplicated at the service, where a replay
answers **conflict — already done**. A lost response recovered by retry either
lands on the conflict (the write stood) or re-executes; the rare true duplicate is
recovered socially — the companion sees its double in `CatchUp` and deletes one
with the keys it already holds. Durability of the connector's *own* state (atomic
saves, the manager's graph) remains its business; a conduit does not co-sign.

## Policy is the companion's design

Nothing in the projection judges *when* a key should be used. The charter —
authored wherever companions are designed, relayed by the connector's `initialize`
instructions — governs that: *"Forum_Manage_User with action `ban` requires standing
permission; without it, deliberate first."* Trust is social: the community watches,
the operator audits the journal, the grant revokes. The connector's only mechanical
brake is total: unbind flushes everything.
