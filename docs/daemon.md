# The daemon

One process, `companion-lobby run`, holds the lobby's state and serves
127.0.0.1. Every other subcommand of the same binary talks to it over that
loopback port: `attach` relays an MCP host's frames, `open` shows the manager
page, `stop` drains it.

## Files in the data directory

The data directory is resolved as `README.md` describes; `LOBBY_DATA_DIR`
overrides it and a path that leads into a `.companion-lobby` directory is
refused. On Windows the base folder comes from the known-folder API, asked
for the process token, and must lie under the profile root Windows records
for the user's account, which no environment variable moves: a process whose
`LOCALAPPDATA`, `APPDATA` or `USERPROFILE` says otherwise still resolves the
same folder, and a folder that resolves outside that root is refused until
`LOBBY_DATA_DIR` names one, rather than used. Whoever creates the directory
creates it owner-only in the same call (`CreateDirectoryW` with an owner-only
security descriptor on Windows, mode 0700 elsewhere), so it never exists with
any other access, and then refuses it if anything but a private lobby file is
already inside. On every
start, before it writes anything there, the daemon checks that the directory
is still owner-only. If it is not, the daemon narrows it again only when
everything inside is a file the daemon itself writes, owned by the current
user (by the directory's owner elsewhere) and, on Windows, with no access
entry of its own beyond what it inherits from the directory, and no
protected DACL, which inherits nothing whatever its entries claim; then it
checks that the narrowing took, that nothing else appeared while it ran and,
on Windows, that every file now grants access to the current user alone. A
directory holding anything else is refused and left untouched: pointing
`LOBBY_DATA_DIR` at a shared folder never rewrites
that folder's access, and a lobby file that another account owns or that was
opened up for someone while the directory was open may have been planted or
read, which narrowing the directory would hide. On Windows owner-only means
the current user owns the directory and it has a protected DACL with one
entry, full control for the current user, inherited by everything inside,
set and read through the Win32 security API; elsewhere it is mode 0700. A
daemon run elevated on Windows creates files owned by the Administrators
group, usually the default owner of an elevated token; a later run that is
not elevated finds the directory not owner-only and refuses it, and the
sponsor removes the directory or runs elevated again.

`attach.secret` is sealed with DPAPI in the current user's scope on Windows,
so a copy of the file is useless to another account or another machine;
elsewhere it is text with mode 0600. When the daemon starts it replaces the
secret with a fresh one if there is none, if the file does not unseal as one
this user sealed, or if the file grants access to anyone but its owner, and
says which in `trace.log`. Replacing a secret that was there is a command the
lobby initiates, `renew_attach_secret`, journaled pending and then settled;
when the pending entry cannot be written the daemon does not start. Hosts' shims read the secret each time they
attach, so a replaced secret needs no change anywhere. A sealed secret does
not survive a profile reset or a move to another machine; the daemon then
writes a new one.

| File | Written by | What it holds |
| --- | --- | --- |
| `lobby.json` | the daemon | The state file: the truth. See below. |
| `journal.jsonl`, `journal.1.jsonl` to `journal.5.jsonl` | the daemon | The journal: attribution. See below. `journal.rotating.jsonl` and `journal.retiring.jsonl` exist only while a rotation runs, or after a crash stopped one. |
| `attach.secret` | the daemon | 32 random bytes as 64 hex digits, sealed. The shim presents it once per attachment, when it attaches. |
| `daemon.instance` | the daemon | Nothing; its exclusive OS lock decides which daemon runs. A start that finds it held refuses at once. Nothing else touches it. |
| `daemon.lock` | the daemon | Nothing; the running daemon holds an exclusive OS lock on it, and the shim, `open` and `stop` learn whether a daemon runs by taking it for an instant. The daemon waits out such a probe when it starts. The OS releases both locks when the process dies, so a daemon killed outright never blocks the next start. |
| `daemon.json` | the lock holder | `pid`, `port` and `buildId` of the process holding the lock. Read only while the lock is held. |
| `trace.log`, `trace.1.log` | the daemon | Its own operational log: start, stop, refusals, attachments, journal failures, panics. Rotated at 1 MiB, two generations. No secrets, no content. |
| `<credential id>.material` | the daemon | One credential's material, as its credential adapter encoded it, sealed. Replaced when a renewal commits or the same account signs in again, kept while the credential needs signing in again, deleted when it is revoked or its companion removed, and swept when the daemon starts if no credential that is not revoked names it. |
| `<connection id>.issued` | the daemon | What a service issued for one connection, as its service adapter encoded it, sealed. Kept while the connection waits for a sign-in, deleted when it is revoked or its companion removed, and swept when the daemon starts if no connection that is not revoked names it. |
| `<credential id>.rotating` | the daemon | A rotating mark, holding nothing secret: written and synced before a credential adapter spends the provider's single-use secret, deleted once the new material is kept. A mark the daemon finds when it starts is a renewal that never finished. |

The record lives beside the lock rather than in it because, on Windows, an
exclusive lock also stops other processes from reading the locked file.
Files the daemon replaces (`lobby.json`, `attach.secret`, `daemon.json`, the
material and issued files and the rotating marks) are written to a temporary
file named
`<file>.<pid>-<n>.tmp` first; such a file left by a crash counts as the
lobby's own. The daemon reads no other file in the directory.

Material and issued files are sealed like the attach secret: with DPAPI in
the current user's scope on Windows, each kind for its own purpose so a file
of one kind never opens as the other, and mode 0600 elsewhere. The daemon
never decodes either: only the adapter that encoded a blob reads it.

## The state file

`lobby.json` is schema 1, camelCase JSON:

```json
{
  "schema": 1,
  "storeId": "<32 hex digits>",
  "revision": 3,
  "companions": [
    { "id": "<32 hex digits>", "moniker": "ada", "displayName": "Ada", "revision": 1,
      "portrait": "png" }
  ],
  "credentials": [
    { "id": "<32 hex digits>", "companion": "<companion id>", "kind": "<provider kind>",
      "account": "<account key>", "label": "<what the sponsor knows it by>",
      "permissions": ["<what the provider granted>"],
      "state": "needsSignIn" }
  ],
  "connections": [
    { "id": "<32 hex digits>", "companion": "<companion id>", "credential": "<credential id>",
      "service": "<service name>", "address": "https://service.test", "label": "front",
      "state": "needsSignIn", "revision": 2 }
  ]
}
```

The store id is minted once per install and names the journal's entries.
`revision` advances on every commit; each companion's own `revision`
advances whenever it changes, and a rename, a new portrait or a removal
names the revision it read. A companion's `portrait` is `png`, `jpeg`,
`webp` or `gif`, or `null` when it shows its initials; the image is kept
beside the file as `<companion id>.portrait`, not sealed, since it is no
secret, and is deleted with the companion. A credential's `account` is the
provider's stable key for the account and its `label` what the sponsor
knows the account by, as the provider's adapter reported it at sign-in: a
handle or an account name. A credential is `ready`, `needsSignIn` (the
provider refused to renew it, or the lobby cannot tell whether a renewal
landed) or `revoked`, and its `permissions` are what the provider granted at
sign-in, in the provider's own words; a renewal may narrow them and only a
new sign-in widens them. A connection is `ready`, `needsSignIn` (its
credential needs signing in again) or `revoked`, and its `revision` advances
on every change, so a call pinned to one revision can tell that the
connection changed while it ran. Material and
what services issued are kept beside the file, never in it. Live
attachments and the sign-ins the lobby waits on are runtime facts and are
not written.

- **Refused, never rewritten.** A file that is empty, does not parse, names
  no schema or another one, carries a field schema 1 does not have or lacks
  one it has (a credential without `permissions`, say), or holds
  a value its rules refuse (a duplicate id or moniker, an identifier of the
  wrong shape, a credential of a companion the file does not hold, one
  account signed in for two companions, two credentials of one kind for one
  companion that are not revoked, a ready connection through a credential
  that is not ready, a waiting connection through one that does not need a
  sign-in, or one label naming two connections of a service that are not
  revoked) stops the daemon with the
  reason, and the file stays exactly as it was. There is no decoder for any
  file this product did not write.
- **Forward policy.** Each release reads its own schema and the predecessors
  it documents here, writes only its own, and refuses a newer one. Schema 1
  has no predecessor.
- **Atomic writes.** The new bytes go to a temporary file with a name unique
  to the call and are synced to the disk; one rename then replaces the file,
  so a reader sees the old file or the new one, whole. Off Windows the
  directory is synced after the rename. On Windows a rename that meets
  another handle holding the file without letting it be replaced (a sharing
  violation, a lock violation or access denied) is tried again for about a
  second, then fails closed: the old file stays whole and the temporary file
  is removed.
- **One commit.** A change is applied to a copy of the state and committed
  in one write; the state in memory becomes that copy only once the write
  landed. A failed commit leaves the file and the state in memory as they
  were, and the command answers 503.

## The journal

The journal is append-only JSON lines, one entry per line, written
synchronously: an append returns once its line is on the disk. An entry
holds no content and no secrets; every field is an identifier, a validated
name, a fixed lobby word or an outcome code. Beside the identifiers an entry
keeps the names the sponsor read when it was written (`companionName`,
`kindTitle`, `accountLabel`, `serviceTitle`, `connectionLabel`), so its
sentence reads the same after a rename and a removed companion keeps its
history by name:

```json
{"id":"<storeId>:7","seq":7,"at":1767225600000,"initiator":"mind","attachment":"<32 hex digits>","companion":"<32 hex digits>","kind":"command","command":"attach","outcome":"done","companionName":"Ada"}
```

`initiator` is `sponsor` (the manager page), `mind` (with the attachment it
acted through) or `lobby` (the lobby reacting to an event, or refusing an
attach that named no companion, since no attachment came to exist). `kind` is
`command` (with the command's fixed name), or `invoked` and `settled` for a
tool call: an `invoked` entry with outcome `pending` is written before the
service is reached (no entry, no call), and a `settled` entry with the call's
outcome after it. Both name the attachment, the companion, the `connection`
the call went through, the `service` and the `tool`; neither holds the
arguments or the answer. Commands on a connection name it and its service
too. Fields that do not apply are absent:

```json
{"id":"<storeId>:9","seq":9,"at":1767225600000,"initiator":"mind","attachment":"<32 hex digits>","companion":"<32 hex digits>","connection":"<32 hex digits>","service":"board","tool":"post","kind":"settled","outcome":"done","companionName":"Ada","serviceTitle":"Board","connectionLabel":"front"}
```

- **Numbers.** `seq` only grows: a daemon that opens the journal continues
  after the highest number any generation holds, so numbers stay monotone
  across rotations, restarts and a daemon killed outright.
- **Generations.** When an entry would take `journal.jsonl` past 1 MiB, it
  becomes `journal.1.jsonl`, the older files move up by one, and the oldest
  beyond five rotated files is dropped, so the journal keeps about 6 MiB. Nothing is deleted for its age. A rotation is
  all or nothing: the live file is set aside first, and when any move fails,
  as it does while another program holds a journal file open without
  allowing it to move, every move already made is undone and `journal.jsonl`
  grows past 1 MiB until a later rotation succeeds. A rotation a crash
  stopped is read as it lies and finished by the next one. A line torn
  by a crash is ended before the next one is written, so it spoils only
  itself, and readers skip it.
- **Pages.** `GET /api/journal` reads newest first, at most 100 entries a
  page (50 by default). A page reads the files newest first and stops once
  it has its entries, without holding up commands, and is read again when a
  rotation moved the files meanwhile. Its `next` cursor names the store and the highest
  entry the first page saw, so entries appended meanwhile never shift a later
  page; a cursor from another journal is refused with 409.

## Commands and events

Everything that changes the state file or the live attachments goes through
one command pipeline:

1. **validate**: a command that cannot apply is refused with an outcome and
   a sentence the lobby wrote, and the refusal is journaled (except a detach
   whose token did not match: nothing proved who asked);
2. **journal, pending**: an entry with outcome `pending` is written before
   anything changes; when it cannot be written the command is refused with
   503 and nothing changed;
3. **apply and commit**: one commit to the state file, or one change to the
   live attachments;
4. **journal, settled**: an entry with the command's outcome. When it cannot
   be written the command still answers with what really happened, never a
   false error; `GET /api/lobby` reports the journal as failing, the manager
   page says so, and `trace.log` says why. The next entry that is written
   reports it healthy again;
5. **emit**: the command's events go to the event pipeline.

A command that needs an adapter is split around it: validation and the
pending entry under the lobby, the adapter on a blocking thread with nothing
held, then a re-check of what the command pinned, the commit and the settled
entry under the lobby again. What changed while the adapter ran wins.

The event pipeline runs every event through small handlers, in order, before
the next command starts. The feed handler turns it into an item of the live
feed. The catalog handler notes each companion whose connections changed and,
once the drain is over, tells every attachment of it, once, that its tools
changed (see the notice stream below), however many connections changed
together. The keep-alive handler wakes the loop that keeps credentials alive
whenever a credential changes.
The cascade handler ends the attachments of a removed companion, each
journaled as a `detach` initiated by the lobby, pending and then settled. The
companion is already gone, so an attachment ends even when its pending entry
cannot be written; the journal's health says so. No handler calls an
adapter.

The commands are `create_companion`, `rename_companion`,
`remove_companion`, `begin_sign_in`, `cancel_sign_in`, `complete_sign_in`,
`revoke_credential`, `revoke_at_provider`, `grant_connection` and
`revoke_connection`, which only the sponsor issues through the operator API,
and `attach` and `detach`, which a mind issues through its shim; the lobby
itself issues `detach` in the cascade, `renew_credential` whenever it keeps
a credential alive, and `renew_attach_secret`, `end_unfinished_renewal` and
`sweep_keyring` when it starts, the last only when a material or
issued-data file no live record names, or a rotating mark no ready
credential holds, is left from a daemon that stopped between writing the
file and committing its record. Removing a companion
removes its credentials and connections and deletes what they kept. A rename or
removal that names a revision the companion has moved past is refused with
409. A removal while minds are attached as the companion is refused with 409
unless it confirms the cascade. A mind attached by moniker stays attached
through a rename, because an attachment is bound to the companion's id; a
host configuration that names the old moniker reaches nothing afterwards.

## Credentials, connections and calls

The daemon runs with the adapters its registry lists, and nothing else: a
credential or connection whose adapter is not installed stays in the state
file, unused, and reaches no catalog. `companion-lobby` runs with the
product's adapters: one credential adapter and one service adapter that
takes its accounts, each documented on a page of its own under
`docs/credentials` and `docs/services`. A sign-in of a provider kind
asks the provider for what every installed service that takes that kind
declares it needs. The test build also makes `companion-lobby-synthetic`,
the same command whose registry holds the test kit's passbook credential
adapter and board service adapter, through the `synthetic` feature that only
the daemon's own dev-dependency turns on, so the host is proven through
adapters whose words belong to no real provider or service. The daemon's
library never names them, and a test checks that `companion-lobby`, built
beside it with every test feature on, carries no passbook material tag.

- **Signing in.** `POST /api/sign-ins` names a companion, a provider kind and
  a hint (what the sponsor typed). The lobby refuses a companion that already
  holds a ready credential of that kind, and one that already waits on a
  sign-in of that kind, before the adapter starts anything; then it asks the
  credential adapter where the browser goes, and remembers the sign-in for
  10 minutes under a random `state` (at most 32 wait at once, the oldest
  expiring first). The sponsor follows it by an id of its own, never the
  `state`: it is `pending` while it waits and ends `signed_in`, `refused`,
  `cancelled` (`POST /api/sign-ins/<id>/cancel`) or `expired`, and the live
  feed says each. The provider sends the browser back to
  `/signin/<kind>/return` with that `state`; a return the lobby is not
  waiting for is refused unjournaled, since nothing proves who sent it, and
  says whether its sign-in was cancelled, expired or already finished. Each
  sign-in completes once, whatever its adapter answers. The adapter completes
  it and names the account the provider authenticated, whatever the hint
  said, and the permissions it granted. An account belongs to one companion,
  and stays reserved to it after its credential is revoked; signing it in for
  another companion is refused with 409, and signing the same companion in
  again creates a new credential while the revoked one stays as history. The
  material is sealed into the keyring before the credential is committed.
  When the companion holds a credential of the kind that needs signing in
  again, the sign-in is for that account: the adapter starts it from that
  credential's material, whatever the hint says, and completing it restores
  the credential under its own id, with new material, and every connection
  that waited for it, without a new grant. Another account is refused with
  409 while one waits. A sign-in that ends without coming back is let go
  of by its adapter at once.
- **Connecting.** `POST /api/connections` names a companion, one of its
  credentials, a service, an address and a label. The lobby checks that the
  credential is ready and of a kind the service accepts, that the address is
  one origin the service may reach, and that no ready connection of that
  companion and service has the label; then the service adapter establishes
  the connection with no lock held. When it needs to prove who it acts for it
  asks the lobby's broker, which hands the ask to the credential's adapter
  with a slot that re-checks, at the read, that the credential is still ready
  and reads only its material; what the adapter mints goes back to the
  service adapter exactly as minted. A credential adapter refuses an ask it
  cannot answer before it reads anything or reaches the provider, and the
  connection is refused with that outcome. The connection is committed only
  when the credential is still ready and the label still free; it keeps only
  what the service issued.
- **Keeping alive.** A loop beside the listener asks each ready credential's
  adapter to renew it, one at a time on a blocking thread with nothing held,
  whenever the adapter said it would want to be asked (at once after a
  sign-in, at least every five minutes, and 30 seconds after a renewal that
  could not finish). Material that is not due changes nothing and is not
  journaled. A renewal is journaled `renew_credential` from the moment the
  adapter marks the material rotating, which the lobby does only while the
  material is still what the adapter read: the pending entry is written and
  the rotating mark is synced to the disk before the adapter spends the
  provider's single-use secret. Committing the new material lands only on
  top of exactly what the adapter read, never twice, and never with more
  permissions than the credential holds; it replaces the material file and
  then deletes the mark. A provider that asks for fewer requests, or that
  could not be reached, changes nothing and is asked again later. A renewal
  the provider refused (`needs_sign_in`), or one that ended with its mark in
  place, since the provider may have rotated the secret with nothing new
  kept (`outcome_unknown`), leaves the credential and every ready connection
  made from it waiting for a sign-in in one commit; their tools leave the
  catalog, every attachment of the companion hears so once, and the old
  material is never renewed again. A daemon that starts and finds a mark on
  a ready credential does the same, journaled `end_unfinished_renewal`
  before anything else runs, and does not start when it cannot. A stop waits
  for a renewal under way to finish. An authorization a service asks for
  while it establishes a connection may renew the material first, through
  the same slot, and a refusal there leaves the credential waiting the same
  way.
- **Revoking.** Revoking a connection deletes what the service issued for it.
  Revoking a credential revokes every connection made from it in the same
  commit, under one `revoke_credential` entry, and deletes the material and
  everything those connections kept. Then, with nothing held, its adapter
  revokes it at the provider from the material the lobby held in memory, a
  step journaled `revoke_at_provider`: `done` when the provider confirmed
  it, `unsupported` when the provider offers no revocation, and the outcome
  otherwise; the answer says which. A credential that waited for a sign-in
  is still revoked at the provider with the last secret the lobby held, but
  a renewal may already have spent that secret, so the provider's answer
  confirms nothing: the step settles `outcome_unknown` and the answer says
  the lobby cannot tell.
- **Calls.** A service tool call resolves the connection the mind named with
  `at` among its companion's ready connections to that service, journals the
  call as invoked, and pins the connection's revision. The call runs with no
  lock held. When it settles the connection is read again: if it was revoked
  or changed meanwhile, the late result never reaches the mind; a read
  answers `revoked`, and a write, which the service may have carried out,
  answers `outcome_unknown` so the mind reads before it retries. A tool the
  companion cannot use is not in its catalog, and calling it is "Unknown
  tool" (-32602) without reaching any service, except a tool whose
  connections wait for a sign-in: calling it is an error result with the
  outcome `needs_sign_in` and a sentence saying the tool comes back once the
  sponsor signs the account in again. `reach` names the connections that
  wait, and `at` naming one of them is told that one waits.
- **Establishing again during a call.** A service adapter may find that the
  service stopped taking what it issued for the connection, or is about to. It reads what was issued through the call's slot, and through the
  slot's authority, a broker for the connection's credential, it
  establishes the connection again with a fresh proof; the slot keeps what
  the service issued anew only while the connection is still ready at the
  revision the call pinned. A minting that renewed the credential, or found
  it needs signing in again, settles that before the call does, so an
  account the provider stopped proving leaves the credential needing a
  sign-in and its connections waiting.
- **Refusals in the service's words.** An adapter answers a definite refusal
  as an explained outcome: the outcome, and a sentence its author wrote,
  compiled into the adapter, stating the service's fact and the next step.
  The mind reads that sentence, and the sponsor reads it when a grant is
  refused; an adapter with nothing to add leaves the lobby's own sentence.
  A write whose connection was revoked while it was on its way says that it
  may have landed and to read the target first.
- **What a connection offers.** A service adapter may say which of its
  tools what the service issued for one connection allows; a companion's
  catalog holds the tools any of its ready connections allows, and each
  service is named by the title the registry gives it.

## The manager page and the live feed

The manager page is a shell of three embedded files, `index.html`,
`manager.css` and `manager.js`, served at every route of the page with a content security policy that allows only the page's own
script, stylesheet and origin. It holds no truth. It loads
`GET /api/lobby`, follows `GET /api/events`, and writes text only through
`textContent`, never markup. A removal is confirmed in place; the page opens
no dialog. Each companion shows the `mcpServers` block for an MCP host,
naming the companion by its immutable id, and a rename of the moniker warns
that configurations naming the moniker stop working. What the page shows,
route by route, is in [manager.md](manager.md).

`GET /api/events` is the live feed as server-sent events. Each item is one
JSON object (`companion`, `companion_removed`, `credential`, `sign_in`, `connection`,
`journal` or `journal_health`) with the id `<epoch>:<seq>`, where the epoch is fresh at
every start of the daemon. A page that connects without a position, or with
one this feed cannot continue (another epoch, a position ahead of the feed,
or one older than the 512 items or 512 KiB the feed keeps), first receives
`{"kind":"reset"}` and loads `GET /api/lobby`, whose `feed.position` is the
last item the picture includes; it applies only the items after it. A page
that reconnects with `Last-Event-ID` receives what it missed. A page that
falls more than 256 items behind receives a reset and the stream ends. At
most 16 feeds are served at once, and a page that goes away frees its place
at once. A page whose feed is refused or ends still loads the lobby, and
opens the feed again a few seconds later. Stopping the daemon ends every
feed.

## The loopback boundary

The daemon listens on 127.0.0.1 only and trusts the machine it runs on, the
way ssh-agent does: any local process that can reach the loopback port can
read the manager page and drive the operator API. Attachments are held to a
stronger rule, so the journal's attribution stays honest: only a process that
can read the owner-only data directory holds the attach secret. On Windows the
listener is bound with `SO_EXCLUSIVEADDRUSE`, so no other socket can share the
port while the daemon runs.

Every request passes these rules in order, before any body is read:

1. **Host.** The `Host` header must name `127.0.0.1`, with no port or the
   daemon's own. `localhost` is redirected (308) to `127.0.0.1` on the same
   port. Any other name or port, including `[::1]`, is refused with 403, so a
   web page that points its own hostname at the loopback address reaches
   nothing, reads included.
2. **Size.** Request heads are capped at 8 KiB (431 beyond). A declared body
   over 1 MiB is refused with 413 before it is read; a body that grows past
   1 MiB while arriving is refused too. A peer has 30 seconds to send its head
   and 10 seconds after it to send its body (408 beyond), so a trickled body
   cannot hold one of the 128 request slots or a stop's drain.
3. **Operator writes** (every method but `GET` under `/api/`) must be
   `application/json` and carry same-origin browser evidence: an `Origin`
   equal to `http://127.0.0.1:<port>`, built from the port the daemon is
   bound to and never from the request, and a `Sec-Fetch-Site` that is absent
   or `same-origin`. Anything else is 403.
4. **Reads** (`GET` under `/api/`, the discovery document aside) carry no
   evidence of another origin: an `Origin`, when present, is the page's own,
   and a `Sec-Fetch-Site`, when present, is `same-origin` or `none`. Anything
   else is 403, so a page elsewhere cannot hold the live feed's places, and a
   browser that lets it read across origins reads nothing.
5. **Attachments** (`/mcp...`) must carry no browser evidence at all: an
   `Origin`, `Referer` or any `Sec-Fetch-Site`, `Sec-Fetch-Mode` or
   `Sec-Fetch-Dest` header is 403. Attaching presents the install's attach
   secret as `Authorization: Bearer <secret>`. The answer carries a token for
   that one attachment, 32 random bytes the daemon keeps only in memory, and
   every frame and the detach present that token instead; a missing or wrong
   secret or token is 401. The install secret therefore crosses the port once
   per attachment, right after the shim verified the listener and the very
   connection it travels on, and a process that takes the port after the
   daemon stops receives only tokens that no later daemon accepts.

Every response carries `Cache-Control: no-store`, `Referrer-Policy:
no-referrer` and `X-Content-Type-Options: nosniff`. The page's files also
carry a content security policy that allows only the page's own script,
stylesheet and origin. At most 128 requests are served at once; one more is
answered 503 at once. Commands run on blocking threads, since each writes the
state file and the journal before it answers.

## Routes

| Route | Rule | What it does |
| --- | --- | --- |
| `GET`, `OPTIONS /api/discovery` | Host | The discovery document; the only response with cross-origin headers. |
| `GET /`, `/journal`, `/companion/<id>`, `/companion/<id>/attach`, `/companion/<id>/sign-in/<sign-in id>` | Host | The manager page; see [manager.md](manager.md). |
| `GET /manager.css`, `/manager.js` | Host | Its stylesheet and script. |
| `GET /api/lobby` | Host | `{"storeId", "feed": {"epoch", "position"}, "companions": [...], "credentials": [...], "connections": [...], "signIns": [...], "kinds": [...], "services": [...], "journal": {"healthy", "highWater"}}`, one consistent picture. `signIns` are the sign-ins the lobby waits on, then the last 64 it remembers ending, each `{"id", "companion", "kind", "account", "status", "expiresAt"}`, `account` being what the sponsor typed to name the account meant (empty when the provider's page chooses) and `status` one of `pending`, `signed_in`, `cancelled`, `expired` or `refused`; a sign-in past its time is ended as expired before the picture is taken. A credential is `{"id", "companion", "kind", "account", "label", "permissions", "state"}` and a connection `{"id", "companion", "credential", "service", "address", "label", "state", "revision"}`; neither ever carries material or what a service issued. `kinds` are the provider kinds the sponsor may sign in with, each `{"kind", "title", "hint"}`, the title being what the sponsor reads and the hint what the sponsor reads beside the field naming the account; `services` are the installed services, each `{"name", "title", "accepts"}`, `accepts` naming the kinds whose accounts can open a connection to it. |
| `GET /api/companions` | Host | `{"companions": [{"id", "moniker", "displayName", "revision", "portrait", "attachments", "hostConfig"}]}`; `portrait` is the path of the companion's portrait, or `null`. |
| `GET /api/companions/<id>/portrait` | Host | The companion's portrait, typed by its own leading bytes, or 404. |
| `GET /api/journal?before=<cursor>&limit=<n>&companion=<id>&finished=1&routine=0` | Host | `{"entries", "upper", "next"}`, newest first; 409 for a cursor of another journal. With `companion`, only the entries about that companion; with `finished=1`, only the entries that say how something ended, leaving out the pending ones; with `routine=0`, none of the renewals that kept an account signed in, which the journal keeps all the same. Each entry carries its `sentence`: who, as which companion, did what, through which connection, and how it ended, named the way the sponsor named them. |
| `GET /api/events` | Host | The live feed, as server-sent events. |
| `POST /api/companions` | operator write | Creates a companion from `{"moniker", "displayName"}`: 201 with `{"companion"}`, or 400 for an invalid name, 409 for a moniker in use. |
| `PATCH /api/companions/<id>` | operator write | Renames from `{"expectedRevision", "moniker", "displayName"}` (either name may be left out): 200 with `{"companion"}`, or 400, 404, or 409 for a moved revision or a moniker in use. |
| `PUT /api/companions/<id>/portrait` | operator write | Gives the companion a portrait from `{"expectedRevision", "image"}`, `image` being a `data:` URL of a PNG, JPEG, WebP or GIF image of at most 512 KiB, recognized by its own bytes, or `null` to go back to initials: 200 with `{"companion"}`, or 400 for anything else, 404, or 409 for a moved revision. |
| `DELETE /api/companions/<id>` | operator write | Removes from `{"expectedRevision", "cascade"}`: 200 with `{"removed", "endedAttachments"}`, or 404, or 409 for a moved revision or live attachments without `"cascade": true`. |
| `POST /api/sign-ins` | operator write | Starts a sign-in from `{"companion", "kind", "hint"}`: 201 with `{"signIn": {"id", "companion", "kind", "account", "status", "expiresAt"}, "url"}`, `url` being where the sponsor's browser goes; or 404 for an unknown companion or kind, 409 when the companion already holds a ready credential of that kind or already waits on a sign-in of it. When the companion holds a credential of the kind that needs signing in again, the sign-in is for that account and `account` is its label. |
| `POST /api/sign-ins/<id>/cancel` | operator write | Cancels a sign-in while it waits: 200 with `{"signIn"}`, or 404 when none with that id waits. |
| `GET /signin/<kind>/return` | Host | Where a provider sends the browser back. Completes a sign-in the lobby waits for, once, and answers 303 to `/signin/done?outcome=<outcome>&companion=<id>` with no body, the companion named only when the lobby knew the sign-in, so the code and the `state` leave the address bar at once and nothing the provider sent rides on. |
| `GET /signin/done` | Host | The page that says how a sign-in ended, from the outcome alone: 200 when the account is signed in, 404 when the lobby does not remember a sign-in waiting for this answer (cancelled, expired, finished or never started), 409 for an account another companion holds, a kind the companion already holds or another account than the one that waits to be signed in again, 403 for a return that did not prove the sign-in, 503 when the provider could not be reached. It loads only the manager page's stylesheet, echoes nothing, and offers one link: back to the companion's page, by its display name, when the lobby knows the companion, and back to the lobby otherwise. |
| `POST /api/credentials/<id>/revoke` | operator write | Revokes the credential and every connection made from it, then at the provider: 200 with `{"credential", "provider"}`, `provider` being `confirmed`, `not_offered`, `not_confirmed`, `sent_last_known` (the credential waited for a sign-in, so the provider was sent the last secret the lobby held and its answer confirms nothing) or `not_asked` (no material to name the account with); or 404, or 409 when it is already revoked. |
| `POST /api/connections` | operator write | Establishes a connection from `{"companion", "credential", "service", "address", "label"}`: 201 with `{"connection"}`; or 400 for an address or label the rules refuse, 404, 409 for a label in use, 403 for a credential that was revoked (`revoked`) or needs signing in again (`needs_sign_in`), or the outcome the adapters answered (`unsupported` is 403). |
| `POST /api/connections/<id>/revoke` | operator write | Revokes the connection: 200 with `{"connection"}`, or 404, or 409 when it is already revoked. |
| `POST /api/stop` | operator write | 202, then the daemon stops accepting connections, ends every live feed, lets accepted requests finish (at most 10 seconds), closes its listener, releases the lock and exits. |
| `POST /mcp/attach` | attachment, install secret | Attaches as `{"companion": "<id or moniker>"}`: 201 with `{"attachment", "token", "companion"}`, or 404 for an unknown companion. |
| `POST /mcp` | attachment, its token | One JSON-RPC message for the attachment named in the `Lobby-Attachment` header: 200 with the answer, 202 for a notification, 404 when the attachment is not live. |
| `GET /mcp` | attachment, its token | The attachment's notice stream, as server-sent events: a comment when it opens, then a `notifications/tools/list_changed` frame each time the companion's tools change, with a keepalive comment every 15 seconds. It ends when the attachment ends or the daemon stops; a stop drops it rather than waiting for it. |
| `DELETE /mcp/attach/<id>` | attachment, its token | Ends the attachment: 204, or 404. |

Refusals are JSON, `{"outcome": "<code>", "message": "<sentence>"}`, with an
outcome code from the one outcome vocabulary and a sentence the lobby wrote.
A command that could not be journaled or committed is 503
(`temporarily_unavailable`) and changed nothing.

### The discovery document

```json
{
  "product": "companion-lobby",
  "discoveryVersion": 1,
  "managerOrigin": "http://127.0.0.1:5219",
  "buildId": "<32 hex digits>",
  "protocolVersions": ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"],
  "storeFingerprint": "<32 hex digits>",
  "lockHolderPid": 4242
}
```

Fields are only ever added. `buildId` is the first 16 bytes of the SHA-256 of
the daemon's executable, so two processes agree exactly when they run the same
bytes. `storeFingerprint` is derived from the attach secret with a one-way,
domain-separated hash; it identifies the install without revealing the secret.

A request that carries browser evidence (an `Origin`, `Referer` or any
`Sec-Fetch-*` header) gets only `product`, `discoveryVersion`,
`managerOrigin` and `protocolVersions`. The build id, the fingerprint and the
pid are stable per install, so a page that could read them would recognize
this machine across sites; the shim sends no browser evidence and reads the
whole document.
## The attach shim

A host configuration names `companion-lobby attach <companion>`. The shim:

1. resolves the data directory and port exactly as the daemon does;
2. when no process holds the daemon lock, starts `companion-lobby run` from its
   own executable at once; otherwise probes the discovery document;
3. verifies, in this order, that the attach secret in its data directory
   matches the document's fingerprint, that the daemon lock is held, that the
   only process listening on the port is the pid in `daemon.json`, and that
   the record names this port and the document's `lockHolderPid`;
4. when the verified daemon runs another build, asks it to stop, waits for the
   lock to be released, starts this build and verifies again;
5. reads the attach secret, refusing a file that grants access to anyone but
   its owner or that does not unseal as one this user sealed (the daemon
   replaces such a file when it next starts);
6. connects, and writes the attach request only after the TCP table shows
   that the far end of that very connection belongs to the lock holder's pid:
   between the verification and the connection the daemon could have closed
   its listener and another process taken the port, but a connection, once
   made, cannot move to another process. Otherwise it writes nothing and says
   the secret was not sent;
7. attaches as the named companion with the install secret, opens the
   attachment's notice stream before it reads the host's first frame, relays
   each host frame to `POST /mcp` with the attachment's token and writes each
   answer back as one line, writes each notification the stream carries as
   one line too, and detaches with the token when the host closes stdin. A
   notice stream that ends while the shim is attached is opened again, for
   the attachment of the moment.

The shim attaches again, once, in exactly two cases: the daemon answers 404
(it has forgotten the attachment, because it was restarted), or nothing
accepts the connection at all. Attaching again starts from step 2, so the
install secret is sent only to a verified daemon on a verified connection; workers that find the
attachment gone at the same moment share one new attachment. Every other
failure is reported to the mind as it is, without a new attachment: a 413 as
invalid parameters, a 503 as "busy, try again", and a connection that failed
after the frame was sent as a call that may or may not have happened.

The shim has 10 seconds to find, start or restart the daemon, one budget for
all of it, because the host is waiting for its first answer. It reads frames of at
most 8 MiB, answers a line that is not JSON with a local parse error (-32700)
and goes on, and stops after an oversized frame. A frame over the daemon's
1 MiB body limit is answered locally with invalid parameters (-32602) and is
not sent. Notifications are relayed in the order they arrived, before any
request read after them; at most 16 requests are relayed at once. Its stdout
carries JSON-RPC frames and nothing else; everything meant for a person goes
to stderr.

### Starting the daemon

The daemon the shim starts must outlive the shim and must not hold anything of
the host's:

- standard input, output and error are null, and its working directory is the
  data directory;
- on Windows it starts with `CREATE_NO_WINDOW`, `DETACHED_PROCESS` and
  `CREATE_NEW_PROCESS_GROUP`, so no console window appears and a host's Ctrl+C
  never reaches it, and with `CREATE_BREAKAWAY_FROM_JOB`, so a host that kills
  its job object on exit does not take the daemon with it (when the job
  forbids breakaway, the daemon starts inside it, lives only as long as the
  host, and the shim says so on stderr; both cases are journey tests with a
  stand-in host that is in its job before the shim starts);
- on Windows the shim first clears the inherit flag on its own standard
  handles: the host's pipes are inheritable in the shim, and a daemon that
  inherited them would keep the host from ever seeing the shim's stdout close;
- elsewhere it starts in its own process group.

### Recording a host

`companion-lobby attach <companion> --log <file>` appends a JSON line to
`<file>` for every frame the shim relays: `{"ms", "dir", "frame"}` where `dir`
is `host` for what the host sent, `lobby` for the answer and `shim` for the
shim's own start, attach and end. See `docs/hosts.md`.

## The MCP endpoint

The daemon serves the initialize-based protocol revisions 2024-11-05 through
2025-11-25. `initialize` echoes a supported revision and offers 2025-11-25
otherwise. The stateless revision 2026-07-28, which starts with
`server/discover`, is not served: `server/discover` gets "method not found"
(-32601), which a dual-era host reads as a server of the earlier era and falls
back to `initialize`. Only the standard JSON-RPC error codes are used.

The identity behind every request is the attachment the shim proved with the
secret. Whatever a host says about itself in `clientInfo` is attribution,
never authority.

The catalog is projected, never declared: a mind sees `reach`, plus the native
tools of every service its companion holds a ready connection to, as
`<service>_<tool>`. Each such tool takes one more argument, `at`, whose
values are the sponsor's labels for the companion's connections to that
service: it may be left out while there is one and is required once there
are more. Names, descriptions and schemas are the adapter's words and the
sponsor's labels, never text a service sent. A tool that the companion
cannot use is absent, not present and refusing. `reach` reports the
companion, each service it reaches with the labels and addresses of its
connections, and the live tool set, so a mind can rely on it even when its
host ignores tool list changes. Its text says who the companion is, then for
each service where it is, by the sponsor's names, the tool to start with,
the service's first, and what else it reads and writes there. `initialize` declares `listChanged: true`,
and the shim relays a `notifications/tools/list_changed` each time the
companion's connections change.

A tool call answers the tool's result, whose text marks third-party passages
as quoted strings, or an error result (`isError: true`) whose text is the
adapter's or the lobby's own sentence for the outcome and whose
`structuredContent` is `{"outcome": "<code>"}`.

## Stopping and rebuilding

`companion-lobby stop` verifies the daemon as the shim does (comparing
fingerprints sends the secret nowhere, so it does not need the secret's file
to be private), sends a same-origin `POST /api/stop`, and returns once the
daemon lock is released. A stopping daemon keeps its listener bound,
accepting nothing, until the drain is over, so no other process can take the
port while the daemon still holds the lock and its record names the port;
then it closes the listener, removes its record and lets go of the lock. A
live feed is not part of the drain: its connection is dropped when the stop
arrives, so a page that stopped reading its feed never holds a stop up. On
Windows a running executable cannot be replaced, so `stop` before
`cargo build`; a shim of the new build would also replace a daemon of the old
one by itself, as long as the old one runs from a different file.
