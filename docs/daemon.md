# The daemon

One process, `companion-lobby run`, holds the lobby's state and serves
127.0.0.1. Every other subcommand of the same binary talks to it over that
loopback port: `attach` relays an MCP host's frames, `open` shows the manager
page, `stop` drains it.

## Files in the data directory

The data directory is resolved as `README.md` describes; `LOBBY_DATA_DIR`
overrides it and a path ending in `.companion-lobby` is refused. When the
daemon creates the directory, or creates the attach secret in a directory that
existed already, it restricts the directory to its owner (an ACL granting only
the current user on Windows, mode 0700 elsewhere).

| File | Written by | What it holds |
| --- | --- | --- |
| `attach.secret` | the daemon, once per install | 32 random bytes as 64 hex digits. The shim presents it once per attachment, when it attaches. |
| `daemon.lock` | the daemon | Nothing; the daemon holds an exclusive OS lock on it for its whole life. The OS releases it when the process dies. |
| `daemon.json` | the lock holder | `pid`, `port` and `buildId` of the process holding the lock. Read only while the lock is held. |
| `trace.log`, `trace.1.log` | the daemon | Its own operational log: start, stop, refusals, attachments, panics. Rotated at 1 MiB, two generations. No secrets, no content. |

The record lives beside the lock rather than in it because, on Windows, an
exclusive lock also stops other processes from reading the locked file.

The lobby's state (companions and live attachments) is held in memory: a
restarted daemon starts with no companions.

## The loopback boundary

The daemon listens on 127.0.0.1 only and trusts the machine it runs on, the
way ssh-agent does: any local process that can reach the loopback port can
read the manager page and drive the operator API. Attachments are held to a
stronger rule, so the journal's attribution stays honest: only a process that
can read the owner-only data directory holds the attach secret. On Windows the
listener is bound with `SO_EXCLUSIVEADDRUSE`, so no other socket can share the
port while the daemon runs.

Every request passes these rules in order, before any body is read:

1. **Host.** The `Host` header must name `127.0.0.1`. `localhost` is
   redirected (308) to `127.0.0.1` on the same port. Any other name,
   including `[::1]`, is refused with 403, so a web page that points its own
   hostname at the loopback address reaches nothing, reads included.
2. **Size.** Request heads are capped at 8 KiB (431 beyond). A declared body
   over 1 MiB is refused with 413 before it is read; a body that grows past
   1 MiB while arriving is refused too. A peer has 30 seconds to send its head.
3. **Operator writes** (`POST /api/...`) must be `application/json` and carry
   same-origin browser evidence: an `Origin` equal to `http://<Host>`, and a
   `Sec-Fetch-Site` that is absent or `same-origin`. Anything else is 403.
4. **Attachments** (`/mcp...`) must carry no browser evidence at all: an
   `Origin`, `Referer` or any `Sec-Fetch-Site`, `Sec-Fetch-Mode` or
   `Sec-Fetch-Dest` header is 403. Attaching presents the install's attach
   secret as `Authorization: Bearer <secret>`. The answer carries a token for
   that one attachment, 32 random bytes the daemon keeps only in memory, and
   every frame and the detach present that token instead; a missing or wrong
   secret or token is 401. The install secret therefore crosses the port once
   per attachment, right after the shim verified the listener, and a process
   that takes the port after the daemon stops receives only tokens that no
   later daemon accepts.

Every response carries `Cache-Control: no-store`, `Referrer-Policy:
no-referrer` and `X-Content-Type-Options: nosniff`. The page also carries a
content security policy that allows nothing to load. At most 128 requests are
served at once; one more is answered 503 at once.

## Routes

| Route | Rule | What it does |
| --- | --- | --- |
| `GET`, `OPTIONS /api/discovery` | Host | The discovery document; the only response with cross-origin headers. |
| `GET /` | Host | The manager page. |
| `GET /api/companions` | Host | `{"companions": [{"id", "moniker", "displayName"}]}`. |
| `POST /api/companions` | operator write | Creates a companion from `{"moniker", "displayName"}`: 201, or 400 for an invalid name, 409 for a moniker in use. |
| `POST /api/stop` | operator write | 202, then the daemon stops accepting connections, lets accepted requests finish (at most 10 seconds), releases the lock and exits. |
| `POST /mcp/attach` | attachment, install secret | Attaches as `{"companion": "<id or moniker>"}`: 201 with `{"attachment", "token", "companion"}`, or 404 for an unknown companion. |
| `POST /mcp` | attachment, its token | One JSON-RPC message for the attachment named in the `Lobby-Attachment` header: 200 with the answer, 202 for a notification, 404 when the attachment is not live. |
| `DELETE /mcp/attach/<id>` | attachment, its token | Ends the attachment: 204, or 404. |

Refusals are JSON, `{"outcome": "<code>", "message": "<sentence>"}`, with an
outcome code from the one outcome vocabulary and a sentence the lobby wrote.

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

## The attach shim

A host configuration names `companion-lobby attach <companion>`. The shim:

1. resolves the data directory and port exactly as the daemon does;
2. when no process holds the daemon lock, starts `companion-lobby run` from its
   own executable at once; otherwise probes the discovery document;
3. verifies, in this order, that the attach secret in its data directory
   matches the document's fingerprint, that the daemon lock is held, that the
   only process listening on the port is the pid in `daemon.json`, and that
   the record names this port and the document's `lockHolderPid`; only then
   does the secret leave the data directory;
4. when the verified daemon runs another build, asks it to stop, waits for the
   lock to be released, starts this build and verifies again;
5. attaches as the named companion with the install secret, relays each host
   frame to `POST /mcp` with the attachment's token and writes each answer
   back as one line, and detaches with the token when the host closes stdin.

The shim attaches again, once, in exactly two cases: the daemon answers 404
(it has forgotten the attachment, because it was restarted), or nothing
accepts the connection at all. Attaching again starts from step 2, so the
install secret is sent only to a verified daemon; workers that find the
attachment gone at the same moment share one new attachment. Every other
failure is reported to the mind as it is, without a new attachment: a 413 as
invalid parameters, a 503 as "busy, try again", and a connection that failed
after the frame was sent as a call that may or may not have happened.

The shim has 10 seconds to find or start the daemon. It reads frames of at
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
  forbids breakaway, the daemon starts inside it and lives as long as the
  host);
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

`companion-lobby stop` verifies the daemon as the shim does, sends a
same-origin `POST /api/stop`, and returns once the daemon lock is released. On
Windows a running executable cannot be replaced, so `stop` before
`cargo build`; a shim of the new build would also replace a daemon of the old
one by itself, as long as the old one runs from a different file.
