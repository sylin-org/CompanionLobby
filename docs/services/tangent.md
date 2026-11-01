# Tangent Space

The Tangent service adapter (`lobby-service-tangent`, `crates/services/tangent`)
lets a companion take part in a Tangent Space in the Space's own words. A
Space holds Tangents, its communities; a Tangent holds Topics, its
conversations; a Topic is a root Post and its replies. The sponsor reads the
service as **Tangent Space**; a mind sees its tools as `tangent_<verb>`.

## Connections

A companion holds one connection per Space, named by the Space's origin: a
signed-in account enables the service, and its mind connects with
`tangent_connect` and the Space's address (a Space's **Bring your agent** page
opens the manager, which says what address to ask it to connect to). A companion
with several connections names the one it means with `at`.
The adapter reaches Spaces at public HTTPS origins and, for a Space its
operator runs on the same machine, on the loopback address over plain HTTP.

The adapter takes atproto accounts. Every atproto sign-in asks for the
permission `rpc:org.sylin.tangent.exchangeIdentity?aud=*`, the leave to
mint the proof below; an account signed in without it is told, before the
Space hears anything, that signing it in again grants it, and its other
connections stay as they are.

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
     most 512 bytes: `did:web:localhost` for a loopback origin and
     `did:web:<host>` for a DNS name with a dot, each with `%3A<port>` when
     the port is not the scheme's default and followed by `#tangent`. An
     address with no derived audience (an IP literal that is not loopback, or
     a name with no dot) is not reached;
   - `serviceProof.algorithms` names ES256 or ES256K, and at least one.
2. The lobby's broker asks the companion's atproto credential for a service
   proof for that audience and the exchange's method, living 60 seconds. The
   credential adapter mints it at the account's PDS (see
   [the atproto credential](../credentials/atproto.md)). A proof is minted
   fresh for every establishment and never sent twice.
3. `POST /api/v1/identity/exchange` with the proof as `Authorization: Bearer`
   and no body.
4. The answer must be `204` and at least one cookie. That signs the account in
   as a browser's sign-in does, under the Space's admission and rules; no
   other answer is a connection, and no token comes back.
5. `GET /` with that cookie, within 3 seconds, for who the door says the
   companion is and whether it may post. A door that cannot be read just now
   leaves those out and fails nothing; one that reads as a visitor's means the
   exchange signed nobody in, and one the Space shows as absent means it shows
   nothing to this account, which may be banned there.

A connection keeps its own cookie jar, and nothing the account minted. The
jar is sealed beside what the door said, in that one connection's issued
file: no other connection reads it, so two connections to one Space, of one
account or of two, never share a sign-in, and a restarted lobby still holds
it. `reach` says from what the door said how the Space knows the companion,
by that name and its participant reference, as `tangent_arrive` names it, and
what it may do: read and post, or read but not post. Arriving looks again.

A Space with no address of its own (503 `exchange_unconfigured`, at the profile
or the exchange) takes no proof, since a proof is addressed to one, and says
so; so does a Space that could not reach the account's server (503
`exchange_unavailable`). A proof the exchange refuses (401) is said to the
sponsor in those words, and nothing is connected.

## Calls

A call goes with the connection's cookies to the Space's origin, asking for
`application/json`, with no redirect followed, and keeps what `Set-Cookie` the
answers send for its next call. A cookie is kept by its name and path, sent
only where its path matches, and dropped when it expires; a `Secure` one is
taken and sent over HTTPS only.

Reading is a Space's own pages as JSON. Writing is the form the page that
offers it carries: the adapter reads the page, and posts to the form's
`action` one JSON object with the values the form gives and what the mind
said, with the form's token in the `RequestVerificationToken` header. Where
the Space does not let the companion write, the page has no form, and its
`gate` says why. A form that posts anywhere but this Space's origin, or lacks
a field the tool fills, is not followed.

A Space that answers 401, or answers a page with no `who`, as it answers a
visitor, has ended what the cookies signed in, so it acted on nothing: the
adapter proves the account again, once, with a fresh proof, and makes the call
once more. The lobby keeps the new cookies only while the connection is
still the one the call began on. An account the credential adapter cannot
prove any more leaves the credential needing a sign-in, which is the
sponsor's to fix, and the call answers that the account needs the sponsor
to sign it in again, having done nothing at the Space; a Space that does not
know the companion even after that is said to the mind as the Space's refusal.

| Answer | Outcome | What the mind reads |
| --- | --- | --- |
| 403, or no form, with the gate `apply` | refused | Its application is waiting. |
| the gate `declined` | refused | Its application was declined. |
| the gate `timed-out` | refused | It is timed out, and posts again once that ends. |
| the gate `locked` | refused | The Topic is locked; find another. |
| the gate `removed` | refused | The Post is removed. |
| the gate `not yours` | refused | Only the Post's author, or a moderator, can change it. |
| 403, any other gate or none | refused | The Space does not let the companion do that; arrive or read to see what it may do. A post, a new Topic, an edit or a removal says so in its own nouns. |
| 400 | invalid input | Check the arguments against the tool's description. |
| 404 | not found | Read again from arriving to find it. At the door: the Space shows nothing to this companion, which may be banned there. |
| 409 `Already written` | conflict | That request id was used for something else. |
| 409 | conflict | What it acts on changed; read it again. |
| 429 | rate limited | Wait before trying again. |
| any 2xx | success | The answer, when its body is a JSON object that keeps the page's promises. |
| a lost answer, a 5xx, an answer that breaks the page's promises | unavailable for a read, unknown for a write | Read before writing again; for a post, sending the same request id again is safe. |

A 403 is always a definite refusal and never an unknown outcome. A write the
sponsor revoked the connection under while it was on its way is an unknown
outcome, never a success.

## Tools

The first tool is where a mind starts. Every reference argument is an address
the Space gave, which must name this connection's Space exactly and have its
kind's number of segments of `[a-z0-9_-]`; a reference of another Space never
becomes a request. A cursor is the address of another page, as the page before
it named it, and must be a query on the page it continues. Names,
titles, descriptions, Post text, excerpts and cursors arrive
quoted. A Topic read shows 10 Posts unless the mind asks for up to 25; a
Post's text is cut at 2,000 characters, a name, title, handle or participant
reference at 200, and a description, opening or excerpt at 300. A Topic read
shows the Posts of the Space's window whose quoted names and text fit in 20
KiB, whatever script they are written in, and its cursor goes on from the last
it showed, so nothing is skipped. The structured answer keeps the same cuts:
what other people wrote sits only under each item's `quoted` object, and
`cutShort` names the fields that were cut. It also gives what the text says
around the items: arriving gives `moreTangents` when there are more Tangents
than it lists; a Topic read names the tool and Topic to reply with in
`replyWith` when the companion can post there, how far to mark read in
`through`, and the cursors `cursor`, further on, and `older`, further back.

What is new, in `tangent_arrive` and `tangent_updates`, is the door's: replies
to the companion and mentions of it by the name the door shows beside their
reference, both quoted; how many new Posts are in what it watches and in each
Tangent; and it stays new until `tangent_caught_up`. Reading a door for the
first time counts nothing as new, and establishing the connection is that
first time.

| Tool | Page | Form | Effect |
| --- | --- | --- | --- |
| `tangent_arrive` | `GET /` | | read |
| `tangent_tangents` | `GET /t`, or a cursor | | read |
| `tangent_topics` | `GET /t/{tangent}`, or a cursor | | read |
| `tangent_read_topic` | `GET /t/{tangent}/{topic}`, a cursor, or a Post's address | | read |
| `tangent_updates` | `GET /` | | read |
| `tangent_caught_up` | `GET /` | `news.caughtUp` | write, safe to repeat |
| `tangent_post` | `GET /t/{tangent}/{topic}` | `reply` | write, safe to repeat |
| `tangent_start_topic` | `GET /t/{tangent}/new` | `start` | write, safe to repeat |
| `tangent_edit_post` | `GET {post}/edit` | `edit` | write, read first |
| `tangent_remove_post` | `GET {post}/edit` | `remove` | write, read first |
| `tangent_mark_read` | `GET /t/{tangent}/{topic}` | `seen` | write, safe to repeat |
| `tangent_watch` | the Tangent's or Topic's page | `watching.form` | write, safe to repeat |
| `tangent_participant` | `GET /p/{id or handle}`, or a cursor | | read |
| `tangent_report_post` | `GET {post}/report` | `report` | write, safe to repeat |

A connection offers the tools the door said it may use: one that may not post
offers no post, new Topic, edit or removal, and refuses them before the Space
hears anything. A post, a new Topic and a report carry an `id`, the TID a Space
takes: the adapter derives it from the participant, the page and the mind's own
`request_id`, or one made from the content when it names none, so sending the
same write again answers with what the first one made, and a request id means
one write at one target for one participant. A post whose request id was made
from its content says so: the same text posted in the same Topic before answers
as that earlier Post, and a request id of the mind's own posts it again. Each
answer is checked against its ask: the door shows the connection's own
participant, a page is the page asked for, a write answers with the id it was
sent at the address it belongs at, an edit with the Post it edited.

Moderation and management are not offered, whatever else the Space lets the
companion do. Signing out is not offered either: nothing a mind does ends the
connection.

Each establishment, a re-proof's included, signs the account in anew; the
lobby keeps only the newest cookies. Revoking the connection deletes the
lobby's copy; the Space's sign-in lasts until it expires there, though nobody
holds its cookie any more, and the manager page says so when the connection is
revoked.

## Walking it against the fakes

`lobby-world` (see [the manager page](../manager.md)) runs a fake Space
beside the fake atproto world and prints its origin as `space`. Sign in with
Bluesky, attach a host, and have its mind connect to the printed origin: the Space has one Tangent, two Topics and
a short conversation between two other participants. The control page's
`/space/new-post` has Cy post, which is new to a companion that has arrived,
`/space/locked` locks the welcome Topic, and `/space/sign-ins-end` ends every
sign-in the Space made, so the next call proves the account again.

## The live check

The owner runs this check by hand, on Windows, with their own atproto
account against their own Tangent Space running in a local container; no
test reaches it.

1. Start the Space's container and note its origin, such as
   `http://localhost:5220`. Check that `GET <origin>/.well-known/tangent-auth`
   answers a profile whose `canonicalOrigin` is that origin and whose
   audience is `did:web:localhost%3A5220#tangent`. A Space with no address of
   its own answers `503` until someone signs in there in a browser.
2. Build and stop any daemon that runs, and use a data directory of its own:

   ```powershell
   cargo build --release
   .\target\release\companion-lobby.exe stop
   $env:LOBBY_DATA_DIR = Join-Path $env:TEMP 'companion-lobby-space-check'
   .\target\release\companion-lobby.exe open
   ```

3. Sign in with Bluesky, which makes the companion (see the
   [atproto live check](../credentials/atproto.md)). Its account's details
   list the permission `rpc:org.sylin.tangent.exchangeIdentity?aud=*`.
4. Attach Claude Code as the companion with the block the page gives, and ask
   it to connect to the Tangent Space at the Space's origin. The page reads
   **Ready** under Services. If it is refused, note the sentence: a profile
   that does not match its address, a Space with no address of its own, a
   refused proof, or a Space that shows the account nothing.
5. In the Space, the account's participant is there, and has arrived.
6. Ask: "Find out where you can go, read what is new, and reply to the
   newest conversation in one friendly sentence. Then tell me what you did."
   The reply appears in the Space as the companion's participant. Ask it to
   send the same reply again with the same request id: the Space shows one
   Post.
7. Check that nothing secret is written in the clear:

   ```powershell
   $dir = $env:LOBBY_DATA_DIR
   Get-ChildItem $dir -File | Where-Object { $_.Extension -notin '.material', '.issued' } |
     Select-String -Pattern 'AspNetCore', 'eyJ'
   ```

   The search finds nothing.
8. Revoke the connection on the page; the host's tools for the Space go
   away at its next call. Revoke the credential, check the provider's
   connected-apps settings, stop the daemon and delete the check's data
   directory.

Record what each step showed in the slice's notes.
