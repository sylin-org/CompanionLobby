# The manager page

The manager page is where the sponsor keeps the keys. It is served by the
daemon at `http://127.0.0.1:<port>/` as three embedded files, holds no truth,
and follows the lobby through the live feed (see [daemon.md](daemon.md)). It
serves the sponsor's moments in [participants.md](participants.md): first
run, lending an identity, opening a connection, the glance, trouble, and
pulling the key.

## Routes

| Route | What the sponsor sees |
| --- | --- |
| `/` | With no companion, the first run: a form that names the first companion. Otherwise the companion cards, each with its portrait or initials, how many minds are attached, how many connections are ready or need a sign-in, and each account it holds by its label and kind, with *Revoke*, confirmed in place; an account that needs a sign-in also offers *Sign in again*, or *Continue sign-in* when that sign-in already waits. One primary action, *Add a companion*. |
| `/companion/<id>/attach` | What just happened and what comes next: the companion's `mcpServers` block, keyed by the companion's moniker and naming it by its immutable id, with *Copy block*. The page says whether a mind is attached yet, and once one is, offers lending the companion an account. |
| `/companion/<id>` | The companion's page: its name, portrait and attachments, then three sections in this order: Accounts, Connections, Journal. *Host configuration* leads back to the block; *Name and portrait* opens the rename form, the portrait and the removal, each confirmed in place. |
| `/companion/<id>/sign-in/<sign-in id>` | The handoff between the lobby and the provider's sign-in page. One the lobby does not remember says so, says it cannot tell how it ended, and leads back to the companion, whose accounts show what is signed in. |
| `/journal` | The whole journal as sentences, newest first, with paging. |

## The moments

**First run.** The page asks for a display name, suggests a moniker from
it, and takes an optional portrait: a PNG, JPEG, WebP or GIF image of at most
512 KiB, previewed before it is sent. Without one the companion shows its
initials. Creating the companion leads to its attach page. A portrait the
lobby did not keep leaves a note there and on the companion's page, saying
why, with *Choose a portrait*, which opens *Name and portrait* at the
portrait field, and *Keep the initials*; it stays until one of them is
chosen or a portrait is kept.

**Lending an identity.** Accounts lists one row per kind of account the
lobby can sign in, by the title the kind's adapter gives it. A row is one of:

- signed in: the account by its label (a handle, or an account name), the
  word *Signed in*, and *Revoke*. The provider's stable key for the account
  shows only inside the row's *Details*;
- needing a sign-in: the account by its label, the words *Sign in needed*,
  *Sign in again*, the page's primary action, which starts a sign-in for
  that same account without anything typed and leads to the handoff, and a
  quieter *Revoke* beside it;
- signing in: the account the sponsor meant, the word *Signing in*, and
  *Continue*, which leads to the handoff;
- otherwise a field for the account, with the hint the kind's adapter gives
  (whether it may be left empty for the provider's page to choose), and
  *Sign in with <kind>*. A revoked account says it was revoked above the
  form.

The handoff names the companion and the account the sponsor meant, says that
the companion receives the account's identity and nothing more, and offers
*Continue to sign in*, which opens the provider's page in a new tab, and
*Cancel sign-in*. The account meant is read from the lobby, so a reload or a
second tab shows it too; the provider's page itself is linked only from the
tab that started the sign-in, and any other tab offers *Start this sign-in
again here* instead, which ends the waiting one and starts the same sign-in
anew. The handoff follows the sign-in to its end and
says it plainly: signed in, with the account's label; cancelled; expired; or
not completed by the provider's answer. Nothing on it ever says connected.
The page the provider's tab lands on says how the sign-in ended and offers
one link, back to the companion's page.

**Opening a connection.** Connections lists the companion's connections,
each by the sponsor's name for it, its service and its address, with one
word and one action:

| Word | When | Action |
| --- | --- | --- |
| Ready | The connection and its account are ready. | *Revoke*, confirmed in place. |
| Sign in needed | The connection stands but its account needs signing in again. | *Sign in again*, which leads to the account's own *Sign in again*, and a quieter *Revoke*, confirmed in place. |
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
New entries arrive as they are written. On a companion's page the renewals
that kept an account signed in are routine: the journal keeps them, and
the page shows them only after *Show routine entries*, which *Hide routine
entries* undoes. The whole journal at `/journal` shows them all.

**Trouble.** When a provider refuses to renew an account, or the lobby
cannot tell whether a renewal landed, the account says *Sign in needed* on
the companion's card and in its row, with *Sign in again* as its one
action, and each of its connections says *Sign in needed*. Nothing else on
the page changes, except that the forms for new connections to a service
that takes that account say to sign it in again first. Signing the same
account in again brings the account and every connection back as they were.

**Pulling the key.** *Revoke* appears wherever a signed-in account or a
connection is shown, the companion's card included, and is confirmed in
place, never in a dialog. Revoking an account revokes every connection made
with it, and the page says so before it happens; afterwards it says whether
the provider confirmed the revocation, offers none, or could not be
reached, that it cannot tell for an account that waited for a sign-in,
whose last key a renewal may already have replaced, and that the provider
was not asked when the lobby held no key for the account.

## The rules the page keeps

- No modal, alert, confirm or prompt. A destructive action is confirmed in
  place: the confirm button takes the focus, and Escape or *Keep* puts the
  action back.
- After every action the focus lands where its result is: the status line
  of the handoff, the connection's row and its new action, the sign-in field
  of a revoked account. A refusal of what a form asked is said inside that
  form until the sponsor types again.
- While the sponsor types in a form or answers a question asked in place,
  what the lobby changes waits; a line there says the lobby changed, and the
  change shows as soon as the sponsor leaves.
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
`passbook-holder`, `board`, `second-board`, `atmosphere-handle`, `space`
and `control`, and runs until its input closes or Enter is pressed. The
`space` is a fake of the first service's Space, documented with that
service under `docs/services`, with `/space/locked` and `/space/tokens-end`
on the control page. Opening a
path on the printed `control` origin turns the passbook authority's knobs:
`/keys/brief` (keys issued from then on live two seconds, so the lobby
renews them over and over), `/keys/long` (an hour), `/renewals/refuse` and
`/renewals/allow`. In another terminal, run the
test-only command with the printed `LOBBY_AUTHSERVER`, a data directory of
its own and a free port, and open the page it serves:

```sh
LOBBY_AUTHSERVER=<printed origin> LOBBY_DATA_DIR=<an empty directory> LOBBY_PORT=<a free port> \
  cargo run -p lobby-daemon --features synthetic --bin companion-lobby-synthetic -- open
```

Then walk the moments: name a companion; sign it in with Atmosphere as the
printed handle and with Passbook by typing the printed `passbook` origin;
open a connection to the printed `board`, and one to the printed `space`;
revoke them; read the journal.
Every sign-in is approved at once as the printed account. For trouble, open
`control/keys/brief` before signing in with Passbook, open a connection,
then open `control/renewals/refuse`: within a second or two the card says
*Sign in again*. Open `control/renewals/allow` and `control/keys/long`, then
sign in again from the card. Revoke an account from its card. Stop the
daemon with `companion-lobby-synthetic stop` and the same environment.

The page's text, as the browser rendered it on such a walk, is kept in
`crates/testkit/fixtures/manager/`: the lobby and a companion's accounts,
with an account's details closed and opened, and both in trouble. Each
file starts with a line naming the SHA-256 of the `manager.js` it was
recorded against, and a test fails as soon as the script differs, so a
change to the page is always walked again; record the files again, from
the text of the page's main region up to its journal, with that line
first. A test holds the account key to its place in that text.
