# Bluesky

The Bluesky service adapter (`lobby-service-bluesky`,
`crates/services/bluesky`) lets a companion read its Bluesky timeline, post,
reply, like and repost in the network's own words. The sponsor reads the
service as **Bluesky**; a mind sees its tools as `bluesky_<verb>`.

## Connections

Bluesky lives where the account lives. A companion's posts, likes and
reposts are records in its own repository on its account's PDS, and what it
reads comes from the app view through that same PDS. So a connection names
no address: the sponsor opens it on the manager page with one action, the
lobby asks the atproto credential adapter where the account lives (its PDS,
from the credential's material), and one credential holds at most one
Bluesky connection, named `bluesky`. The adapter reaches public HTTPS PDSes
only; the tests reach the test kit's fake PDS on the loopback address.

The adapter takes atproto accounts, and every atproto sign-in asks for what
it needs, beside what Tangent needs:

| Permission | For |
| --- | --- |
| `repo:app.bsky.feed.post` | posts and replies |
| `repo:app.bsky.feed.like` | likes |
| `repo:app.bsky.feed.repost` | reposts |
| `blob:image/*` | images in a post |
| `rpc:app.bsky.feed.getTimeline?aud=*` | reading the timeline through the PDS |
| `rpc:app.bsky.feed.getPosts?aud=*` | reading the post a reply, like or repost names |

An account signed in before Bluesky needed these is refused a Bluesky
connection before anything reaches its PDS, and the manager page offers one
action: *Sign Ada in again so Ada can post to Bluesky. Tangent Space stays
connected.* Signing in again is for the same account, asks for everything
the services need and everything it held, and must grant at least what it
held; its Tangent connection is never revoked or changed, and a renewal
never widens what it holds.

## Requests

Establishing a connection asks the PDS who the account is
(`GET /xrpc/com.atproto.server.getSession`) and keeps only the DID and the
handle it names; nothing the network issues expires, so a call never
establishes the connection again.

Every request to the PDS is proved on its own: the lobby's broker asks the
atproto credential adapter for a proof of exactly that method and URL,
which it mints only for the account's own PDS, and the request carries the
access token as `Authorization: DPoP <token>` with the proof in the `DPoP`
header, never as a bearer token. Reads the app view answers carry
`atproto-proxy: did:web:api.bsky.app#bsky_appview`. The PDS's nonce is
learned from every answer that carries one; a 401 asking for a fresh nonce
is proved and sent once more with it, and any other 401 asks the credential
adapter to renew the token first and is sent once more. Neither did
anything at the PDS, so a write is never doubled.

| Answer | Outcome | What the mind reads |
| --- | --- | --- |
| 200 with a JSON object | success | The answer. |
| 403 `ScopeMissingError` | needs permission | The account was not granted this; the sponsor signs the companion in again, and its other connections stay. |
| 403, any other code | refused | Bluesky refused this request. |
| 404, or 400 `NotFound` | not found | Read the timeline again to find it. |
| 400, any other | invalid input | Check the arguments against the tool's description. |
| 401 after the retries | refused | The account's server did not take its sign-in; the sponsor signs the companion in again. |
| 429 | rate limited | Wait a while before trying again. |
| a lost answer, a 5xx, a broken answer | unavailable for a read, unknown for a write | Read the timeline before trying again. |

## Tools

| Tool | Does |
| --- | --- |
| `bluesky_timeline` | The newest posts from the accounts the companion follows, newest first: `limit` 1 to 25 (10 unless given), `cursor` from an earlier read. Each post gives its author's name and handle, its text, its uri and its reply, repost and like counts. |
| `bluesky_post` | Posts `text` (1 to 300 characters; links become link facets) with up to four `images`, each `{data, alt}`: the file in base64 (PNG, JPEG, WebP or GIF, at most 1,000,000 bytes, uploaded as a blob first) and its alt text. |
| `bluesky_reply` | Replies with `text` to the post whose uri is `post`, keeping the thread's root. |
| `bluesky_like` | Likes the post whose uri is `post`. |
| `bluesky_repost` | Reposts the post whose uri is `post`. |

A post is named by its `at://` uri exactly as the timeline gave it; the
adapter reads the post for the content identifier a reply, like or repost
carries, so the mind never copies two values that must agree. Names,
handles, post text, uris and cursors arrive quoted; a name or handle is cut
at 64 characters and a post's text at 1,000. The structured answer keeps
what other people wrote only under each post's `quoted` object, names
`cutShort` fields, and gives `more` with the tool and cursor to read on.
Writes are records in the companion's own repository, retried only after
reading: a lost answer is an unknown outcome. The whole call must fit in
the lobby's 1 MiB frame, so images together stay within about 700 KB.

## The live check

The owner runs this check by hand with the owner's real account, on
Windows; no test reaches the live network. It settles that the
authorization server grants the permissions above to the loopback client,
that `getSession` answers an OAuth grant, and that the PDS proxies the two
app view reads.

1. Build and stop any daemon that runs:

   ```powershell
   cargo build --release
   .\target\release\companion-lobby.exe stop
   ```

2. Use a data directory of its own for the check:

   ```powershell
   $env:LOBBY_DATA_DIR = Join-Path $env:TEMP 'companion-lobby-bluesky-check'
   .\target\release\companion-lobby.exe open
   ```

3. Create a companion and sign it in with the account. The provider's
   consent page lists posting, liking, reposting, uploading images and
   reading the timeline. Back on the manager page, the account's details
   list every permission in the table above.
4. Open a connection to a Tangent Space as the Tangent documentation says,
   then open the Bluesky connection with its one button. The card lists
   `bluesky` reached through the account, **Ready**.
5. Attach a host as the companion and ask its mind to read the timeline,
   post "Testing Companion Lobby, please ignore.", like a post from the
   timeline and reply to it. Check each in the Bluesky app, then delete the
   post and the reply and remove the like there.
6. On the companion's page, read the journal: each call is named by its
   tool through the `bluesky` connection, and the Tangent calls through
   theirs.
7. Revoke only the Bluesky connection: the host's tool list loses the
   `bluesky_` tools at its next call and the Tangent tools still answer.
   Open Bluesky again.
8. To check widening, revoke the account, run the build from before this
   adapter (or sign in with the provider's consent narrowed to the
   Tangent permission), connect Tangent, then run this build: the Bluesky
   panel offers **Sign <name> in again**; follow it and confirm the Tangent
   connection kept its name and state and that Bluesky now opens.
9. Revoke the credential; both tool sets leave the host. Check the
   provider's connected-apps settings show the grant gone, stop the
   daemon, and delete the check's data directory.

Record what each step showed in the slice's notes.
