# The daemon

One process, `companion-lobby run`, holds the lobby's state and serves
127.0.0.1. Every other subcommand of the same binary talks to it over that
loopback port: `attach` relays an MCP host's frames, `open` shows the manager
page, `stop` drains it.

## Files in the data directory

The data directory is resolved as `README.md` describes; `LOBBY_DATA_DIR`
overrides it and a path that leads into a `.companion-lobby` directory is
refused. On Windows the base folder comes from the known-folder API alone: a
process whose `LOCALAPPDATA` or `APPDATA` says otherwise still resolves the
same folder, and one whose `USERPROFILE` Windows does not recognize gets no
folder and refuses until `LOBBY_DATA_DIR` names one, rather than using
another. Whoever creates the directory restricts it to its owner. On every
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
says which in `trace.log`. Hosts' shims read the secret each time they
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

The record lives beside the lock rather than in it because, on Windows, an
exclusive lock also stops other processes from reading the locked file.
Files the daemon replaces (`lobby.json`, `attach.secret`, `daemon.json`) are
written to a temporary file named `<file>.<pid>-<n>.tmp` first; such a file
left by a crash counts as the lobby's own. The daemon reads no other file in
the directory.

## The state file

`lobby.json` is schema 1, camelCase JSON:

```json
{
  "schema": 1,
  "storeId": "<32 hex digits>",
  "revision": 3,
  "companions": [
    { "id": "<32 hex digits>", "moniker": "ada", "displayName": "Ada", "revision": 1 }
  ]
}
```

The store id is minted once per install and names the journal's entries.
`revision` advances on every commit; each companion's own `revision`
advances whenever it changes, and a rename or removal names the revision it
read. Live attachments are runtime facts and are not written.

- **Refused, never rewritten.** A file that is empty, does not parse, names
  no schema or another one, carries a field schema 1 does not have, or holds
  a value its rules refuse (a duplicate id or moniker, an identifier of the
  wrong shape) stops the daemon with the reason, and the file stays exactly
  as it was. There is no decoder for any file this product did not write.
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
name, a fixed lobby word or an outcome code:

```json
{"id":"<storeId>:7","seq":7,"at":1767225600000,"initiator":"mind","attachment":"<32 hex digits>","companion":"<32 hex digits>","kind":"command","command":"attach","outcome":"done"}
```

`initiator` is `sponsor` (the manager page), `mind` (with the attachment it
acted through) or `lobby` (the lobby reacting to an event). `kind` is
`command` (with the command's fixed name) here, and `invoked` and `settled`
for the external calls adapters make. Fields that do not apply are absent.

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

The event pipeline runs every event through small handlers, in order, before
the next command starts. The feed handler turns it into an item of the live
feed. The cascade handler ends the attachments of a removed companion, each
journaled as a `detach` initiated by the lobby, pending and then settled. The
companion is already gone, so an attachment ends even when its pending entry
cannot be written; the journal's health says so.

The commands are `create_companion`, `rename_companion` and
`remove_companion`, which only the sponsor issues through the operator API,
and `attach` and `detach`, which a mind issues through its shim. A rename or
removal that names a revision the companion has moved past is refused with
409. A removal while minds are attached as the companion is refused with 409
unless it confirms the cascade. A mind attached by moniker stays attached
through a rename, because an attachment is bound to the companion's id; a
host configuration that names the old moniker reaches nothing afterwards.

## The manager page and the live feed

The manager page is a shell of three embedded files, `index.html`,
`manager.css` and `manager.js`, served at `/`, `/companion/<id>` and
`/journal` with a content security policy that allows only the page's own
script, stylesheet and origin. It holds no truth. It loads
`GET /api/lobby`, follows `GET /api/events`, and writes text only through
`textContent`, never markup. A removal is confirmed in place; the page opens
no dialog. Each companion shows the `mcpServers` block for an MCP host,
naming the companion by its immutable id, and a rename of the moniker warns
that configurations naming the moniker stop working.

`GET /api/events` is the live feed as server-sent events. Each item is one
JSON object (`companion`, `companion_removed`, `journal` or
`journal_health`) with the id `<epoch>:<seq>`, where the epoch is fresh at
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
| `GET /`, `/companion/<id>`, `/journal` | Host | The manager page. |
| `GET /manager.css`, `/manager.js` | Host | Its stylesheet and script. |
| `GET /api/lobby` | Host | `{"storeId", "feed": {"epoch", "position"}, "companions": [...], "journal": {"healthy", "highWater"}}`, one consistent picture. |
| `GET /api/companions` | Host | `{"companions": [{"id", "moniker", "displayName", "revision", "attachments", "hostConfig"}]}`. |
| `GET /api/journal?before=<cursor>&limit=<n>` | Host | `{"entries", "upper", "next"}`, newest first; 409 for a cursor of another journal. |
| `GET /api/events` | Host | The live feed, as server-sent events. |
| `POST /api/companions` | operator write | Creates a companion from `{"moniker", "displayName"}`: 201 with `{"companion"}`, or 400 for an invalid name, 409 for a moniker in use. |
| `PATCH /api/companions/<id>` | operator write | Renames from `{"expectedRevision", "moniker", "displayName"}` (either name may be left out): 200 with `{"companion"}`, or 400, 404, or 409 for a moved revision or a moniker in use. |
| `DELETE /api/companions/<id>` | operator write | Removes from `{"expectedRevision", "cascade"}`: 200 with `{"removed", "endedAttachments"}`, or 404, or 409 for a moved revision or live attachments without `"cascade": true`. |
| `POST /api/stop` | operator write | 202, then the daemon stops accepting connections, ends every live feed, lets accepted requests finish (at most 10 seconds), closes its listener, releases the lock and exits. |
| `POST /mcp/attach` | attachment, install secret | Attaches as `{"companion": "<id or moniker>"}`: 201 with `{"attachment", "token", "companion"}`, or 404 for an unknown companion. |
| `POST /mcp` | attachment, its token | One JSON-RPC message for the attachment named in the `Lobby-Attachment` header: 200 with the answer, 202 for a notification, 404 when the attachment is not live. |
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
7. attaches as the named companion with the install secret, relays each host
   frame to `POST /mcp` with the attachment's token and writes each answer
   back as one line, and detaches with the token when the host closes stdin.

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
tools of its companion's ready connections as `<service>_<tool>`. A tool that
the companion cannot use is absent, not present and refusing. `reach` reports
the companion and the live tool set, so a mind can rely on it even when its
host ignores tool list changes. The tool list does not change while an
attachment lives, so `initialize` declares `listChanged: false`.

## Stopping and rebuilding

`companion-lobby stop` verifies the daemon as the shim does (comparing
fingerprints sends the secret nowhere, so it does not need the secret's file
to be private), sends a same-origin `POST /api/stop`, and returns once the
daemon lock is released. A stopping daemon keeps its listener bound,
accepting nothing, until the drain is over, so no other process can take the
port while the daemon still holds the lock and its record names the port;
then it closes the listener, removes its record and lets go of the lock. On
Windows a running executable cannot be replaced, so `stop` before
`cargo build`; a shim of the new build would also replace a daemon of the old
one by itself, as long as the old one runs from a different file.
