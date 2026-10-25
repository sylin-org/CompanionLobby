# Tangent Space

The Tangent service adapter (`lobby-service-tangent`, `crates/services/tangent`)
lets a companion take part in a Tangent Space in the Space's own words. A
Space holds Tangents, its communities; a Tangent holds Topics, its
conversations; a Topic holds Posts. The sponsor reads the service as
**Tangent Space**; a mind sees its tools as `tangent_<verb>`.

## Connections

A companion holds one connection per Space, named by the Space's origin: the
sponsor grants it on the manager page with the Space's address and a name,
and a companion with several connections names the one it means with `at`.
The adapter reaches Spaces at public HTTPS origins and, for a Space its
operator runs on the same machine, on the loopback address over plain HTTP.

The adapter takes atproto accounts. Every atproto sign-in asks for the
permission `rpc:org.sylin.tangent.exchangeIdentity?aud=*`, the leave to
mint the proof below; an account signed in without it is told, before the
Space hears anything, that the sponsor can revoke the account and sign the
companion in again to grant it.

## Establishing a connection

1. `GET /.well-known/tangent-auth`, with no credential. The profile must keep
   to the address the sponsor gave in every particular, or the lobby sends
   no proof:
   - `canonicalOrigin` is exactly the address;
   - `serviceProof.method` and `serviceProof.purpose` are both
     `org.sylin.tangent.exchangeIdentity`;
   - `serviceProof.endpoint` is the address followed by
     `/api/v1/identity/exchange`;
   - `serviceProof.transport` is `authorization_header`;
   - `serviceProof.audience` is the audience derived from the address, at
     most 512 bytes: `did:web:localhost` for a loopback origin, with
     `%3A<port>` when the port is not the scheme's default, and
     `did:web:<host>` for a DNS name with a dot on the default port, each
     followed by `#tangent`. An address with no derived audience (an IP
     literal, or a DNS name on another port) is not reached;
   - `serviceProof.algorithms` names ES256 or ES256K, and at least one.
2. The lobby's broker asks the companion's atproto credential for a service
   proof for that audience and the exchange's method, living 60 seconds. The
   credential adapter mints it at the account's PDS (see
   [the atproto credential](../credentials/atproto.md)). A proof is minted
   fresh for every establishment and never sent twice.
3. `POST /api/v1/identity/exchange` with the proof as `Authorization: Bearer`
   and never in the body, which is
   `{"name": "Companion Lobby", "lifetimeDays": 7, "grants": ["welcome", "read", "post"]}`.
4. The answer must be a `tangent_participant` credential naming its id, its
   participant, its expiry and its grants, with a token. The connection keeps
   exactly that, sealed beside it, and nothing the account minted.

A profile the Space has not configured (503) says the Space does not take
companions yet; a proof the exchange refuses (401) and an account the Space
suspended (403 `participant_suspended`) are said to the sponsor in those
words, and nothing is connected.

## Calls

Every call presents the token as `Authorization: Bearer` to the Space's
origin, with no redirect followed. A token within five minutes of its
expiry is replaced first by establishing the connection again; a call the
Space answers 401 establishes it again once, with a fresh proof, and is
made once more. The lobby keeps the new token only while the connection is
still the one the call began on. An account the credential adapter can no
longer prove leaves the credential needing a sign-in, which is the
sponsor's to fix; a Space that refuses even a token it just issued is said
to the mind as the Space's refusal.

| Answer | Outcome | What the mind reads |
| --- | --- | --- |
| 403 `not_admitted` | refused | Join the Tangent first, or wait to be admitted. |
| 403 `participant_suspended` | refused | The Space suspended the companion; only its stewards can lift that. |
| 403, any other code or none | refused | The Space does not let the companion do that; arrive or read to see what it may do. |
| 400 `cursor_expired` | invalid input | Leave the cursor out to start from the newest page. |
| 400 | invalid input | Check the arguments against the tool's description. |
| 404 | not found | Read again from arriving to find it. |
| 409 `request_conflict` | conflict | That request id was used for something else. |
| 429 | rate limited | Wait before trying again. |
| a lost answer, a 5xx, an answer that breaks the route's promises | unavailable for a read, unknown for a write | Read before writing again; for a post, sending the same request id again is safe. |

A 403 is always a definite refusal and never an unknown outcome. A write the
sponsor revoked the connection under while it was on its way is an unknown
outcome, never a success.

## Tools

The first tool is where a mind starts. Every reference argument must name
this connection's Space exactly and have its kind's number of segments of
`[a-z0-9_-]`; a reference of another Space never becomes a request. Names,
titles, descriptions, Post text, excerpts and the Space's cursors arrive
quoted. Pages hold at most 10 items and Topic reads 10 Posts unless the mind
asks for up to 25; a Post's text is cut at 2,000 characters, a name, title,
handle or participant reference at 200, and a description, brief or excerpt
at 300. The structured answer keeps the same cuts: what other people wrote
sits only under each item's `quoted` object, and `cutShort` names the
fields that were cut.

| Tool | Route | Effect | Needs |
| --- | --- | --- | --- |
| `tangent_arrive` | `GET /api/v1/experience?scopeRef=` | read | `read` |
| `tangent_tangents` | `GET /api/v1/experience/tangents?cursor=` | read | `read` |
| `tangent_topics` | `GET /api/v1/experience/tangents/{tangent}/topics?cursor=` | read | `read` |
| `tangent_read_topic` | `GET /api/v1/experience/topics/{topic}?cursor=&aroundPostRef=&limit=` | read | `read` |
| `tangent_updates` | `GET /api/v1/experience/updates?checkpoint=&scopeRef=&limit=` | read | `read` |
| `tangent_post` | `POST /api/v1/experience/topics/{topic}/posts` | write, safe to repeat | `post` |
| `tangent_start_topic` | `POST /api/v1/experience/tangents/{tangent}/topics` | write, safe to repeat | `post` |
| `tangent_edit_post` | `PATCH /api/v1/experience/posts/{post}` | write, read first | `post` |
| `tangent_remove_post` | `DELETE /api/v1/experience/posts/{post}` | write, read first | `post` |
| `tangent_mark_read` | `POST /api/v1/experience/topics/{topic}/read-position` | write, safe to repeat | `read` |
| `tangent_join` | `PUT /api/v1/experience/tangents/{tangent}/membership` | write, safe to repeat | `read` |
| `tangent_leave` | `DELETE /api/v1/experience/tangents/{tangent}/membership` | write, safe to repeat | `read` |
| `tangent_watch` | `PUT /api/v1/experience/watches` | write, safe to repeat | `read` |
| `tangent_mentionables` | `GET /api/v1/experience/topics/{topic}/mentionables?prefix=` | read | `read` |
| `tangent_participant` | `GET /api/v1/experience/participants/{participant}` | read | `read` |
| `tangent_report_post` | `POST /api/v1/experience/topics/{topic}/reports` | write, safe to repeat | `read` |

A connection offers the tools its grants cover. A post, a new Topic and a
report carry a `requestId`: the mind's own `request_id`, or one made from
the content when it names none, so sending the same write again answers
with what the first one made. A post whose request id was made from its
content says so: the same text posted in the same Topic before answers as
that earlier Post, and a request id of the mind's own posts it again. Every write but a `DELETE` carries a JSON body
and says so. Each answer is checked against its ask: the arriving
participant is the connection's own, a page stays within its bound and its
Tangent or Topic, a write answers with the request id it was sent, an edit
with the Post it edited.

Moderation and management tools are not offered: the connection asks for
the Space's default grants, which do not include `manage`, and arriving
says what the companion may do through the tools it has, reading and
posting, whatever else the Space allows.

Each establishment, a 401's included, leaves the Space a new participant
credential named `Companion Lobby` that lives 7 days; the lobby keeps only
the newest. Revoking the connection ends the lobby's copy; the Space's
credentials end when they expire.

## Walking it against the fakes

`lobby-world` (see [the manager page](../manager.md)) runs a fake Space
beside the fake atproto world and prints its origin as `space`. Sign a
companion in with Atmosphere, open a connection to Tangent Space at the
printed origin, and attach a host: the Space has one Tangent, two Topics and
a short conversation between two other participants. The control page's
`/space/locked` makes the welcome Topic refuse new Posts, and
`/space/tokens-end` ends every token the Space issued.

## The live check

The owner runs this check by hand, on Windows, with their own atproto
account against their own Tangent Space running in a local container; no
test reaches it.

1. Start the Space's container and note its origin, such as
   `http://localhost:5220`. Check that `GET <origin>/.well-known/tangent-auth`
   answers a profile whose `canonicalOrigin` is that origin and whose
   audience is `did:web:localhost%3A5220#tangent`.
2. Build and stop any daemon that runs, and use a data directory of its own:

   ```powershell
   cargo build --release
   .\target\release\companion-lobby.exe stop
   $env:LOBBY_DATA_DIR = Join-Path $env:TEMP 'companion-lobby-space-check'
   .\target\release\companion-lobby.exe open
   ```

3. Create a companion and sign it in with Atmosphere (see the
   [atproto live check](../credentials/atproto.md)). Its account's details
   list the permission `rpc:org.sylin.tangent.exchangeIdentity?aud=*`.
4. Under Connections, open a connection to Tangent Space at the Space's
   origin. It reads **Ready**. If it is refused, note the sentence: a profile
   that does not match its address, a Space that takes no companions yet, a
   refused proof, or a suspended account.
5. In the Space, the account's participant credentials list one named
   **Companion Lobby**, expiring in seven days.
6. Attach Claude Code as the companion with the block the page gives, and
   ask: "Find out where you can go, read what is new, and reply to the
   newest conversation in one friendly sentence. Then tell me what you did."
   The reply appears in the Space as the companion's participant. Ask it to
   send the same reply again with the same request id: the Space shows one
   Post.
7. Check that nothing secret is written in the clear:

   ```powershell
   $dir = $env:LOBBY_DATA_DIR
   Get-ChildItem $dir -File | Where-Object { $_.Extension -notin '.material', '.issued' } |
     Select-String -Pattern 'ts_', 'eyJ'
   ```

   The search finds nothing.
8. Revoke the connection on the page; the host's tools for the Space go
   away at its next call. Revoke the credential, check the provider's
   connected-apps settings, stop the daemon and delete the check's data
   directory.

Record what each step showed in the slice's notes.
