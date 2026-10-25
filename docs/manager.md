# The manager page

The manager page is where the sponsor keeps the keys. It is served by the
daemon at `http://127.0.0.1:<port>/` as three embedded files, holds no truth,
and follows the lobby through the live feed (see [daemon.md](daemon.md)). It
serves the sponsor's moments in [participants.md](participants.md): first
run, lending an identity, opening a connection, the glance, and pulling the
key.

## Routes

| Route | What the sponsor sees |
| --- | --- |
| `/` | With no companion, the first run: a form that names the first companion. Otherwise the companion cards, each with its portrait or initials, how many minds are attached, the accounts it is signed in as and how many connections are ready, and one primary action, *Add a companion*. |
| `/companion/<id>/attach` | What just happened and what comes next: the companion's `mcpServers` block, keyed by the companion's moniker and naming it by its immutable id, with *Copy block*. The page says whether a mind is attached yet, and once one is, offers lending the companion an account. |
| `/companion/<id>` | The companion's page: its name, portrait and attachments, then three sections in this order: Accounts, Connections, Journal. *Host configuration* leads back to the block; *Name and portrait* opens the rename form, the portrait and the removal, each confirmed in place. |
| `/companion/<id>/sign-in/<sign-in id>` | The handoff between the lobby and the provider's sign-in page. |
| `/journal` | The whole journal as sentences, newest first, with paging. |

## The moments

**First run.** The page asks for a display name, suggests a moniker from
it, and takes an optional portrait: a PNG, JPEG, WebP or GIF image of at most
512 KiB, previewed before it is sent. Without one the companion shows its
initials. Creating the companion leads to its attach page.

**Lending an identity.** Accounts lists one row per kind of account the
lobby can sign in, by the title the kind's adapter gives it. A row is one of:

- signed in: the account by its label (a handle, or an account name), the
  word *Signed in*, and *Revoke*. The provider's stable key for the account
  shows only inside the row's *Details*, with what the provider granted;
- signing in: the account the sponsor meant, the word *Signing in*, and
  *Continue*, which leads to the handoff;
- otherwise a field for the account, optional, and *Sign in with <kind>*.
  A revoked account says it was revoked above the form.

The handoff names the companion and the account the sponsor meant, says that
the companion receives the account's identity and nothing more, and offers
*Continue to sign in*, which opens the provider's page in a new tab, and
*Cancel sign-in*. The account meant is read from the lobby, so a reload or a
second tab shows it too; the provider's page itself is linked only from the
tab that started the sign-in. The handoff follows the sign-in to its end and
says it plainly: signed in, with the account's label; cancelled; expired; or
not completed by the provider's answer. Nothing on it ever says connected.

**Opening a connection.** Connections lists the companion's connections,
each by the sponsor's name for it, its service and its address, with one
word and one action:

| Word | When | Action |
| --- | --- | --- |
| Ready | The connection and its account are ready. | *Revoke*, confirmed in place. |
| Sign in needed | The connection stands but its account cannot be used. | *Sign in again*, which leads to the account's row. |
| Revoked | The sponsor revoked it, or its account. | *Open again* with a ready account the service takes, or *Sign in* when there is none. |

A revoked connection is shown only while nothing ready or newer holds its
name. Below the list, each installed service has a form: the address (a
bare host is read as `https://`), a name suggested from the address, and,
when the companion holds more than one account the service takes, which
account. A service whose kind of account the companion does not hold says
which kind it takes. A lobby with no service installed says so, and offers
nothing.

**The glance.** The journal reads as sentences: who, as which companion, did
what, through which connection, and how it ended, in the names the sponsor
gave. It shows no ids, codes, secrets or content, leaves out the entries
written before an outcome was known, and pages back with *Older entries*.
New entries arrive as they are written.

**Pulling the key.** *Revoke* appears wherever an account or a connection
is shown and is confirmed in place, never in a dialog. Revoking an account
revokes every connection made with it, and the page says so before it
happens.

## The rules the page keeps

- No modal, alert, confirm or prompt. A destructive action is confirmed in
  place: the confirm button takes the focus, and Escape or *Keep* puts the
  action back.
- One primary action per screen, in the warm accent; everything else is
  quiet. A dark canvas, compact type and thin separators.
- What the sponsor types is kept as a draft in the tab's own storage, so
  a reload loses nothing; everything else comes from the lobby.
- Every control is reachable by keyboard, with a visible focus ring, and the
  page works at 375 pixels wide without scrolling sideways.
- Text reaches the page only as text, never as markup; images come only from
  the lobby's own origin, or from a file the sponsor just chose.

## Walking the journey against local fakes

The test kit's `lobby-world` command starts a passbook authority, two boards
and a fake world of Atmosphere accounts on loopback, and prints their origins:

```sh
cargo run -p lobby-testkit --bin lobby-world
```

It prints one `name=value` line each for `LOBBY_AUTHSERVER`, `passbook`,
`passbook-holder`, `board`, `second-board` and `atmosphere-handle`, and runs
until its input closes or Enter is pressed. In another terminal, run the
test-only command with the printed `LOBBY_AUTHSERVER`, a data directory of
its own and a free port, and open the page it serves:

```sh
LOBBY_AUTHSERVER=<printed origin> LOBBY_DATA_DIR=<an empty directory> LOBBY_PORT=<a free port> \
  cargo run -p lobby-daemon --features synthetic --bin companion-lobby-synthetic -- open
```

Then walk the moments: name a companion; sign it in with Atmosphere as the
printed handle and with Passbook by typing the printed `passbook` origin;
open a connection to the printed `board`; revoke it; read the journal.
Every sign-in is approved at once as the printed account. Stop the daemon
with `companion-lobby-synthetic stop` and the same environment.
