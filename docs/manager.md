# The manager page

The manager page is where the sponsor keeps the keys. It is served by the
daemon at `http://127.0.0.1:<port>/` as three embedded files, holds no truth,
and follows the lobby through the live feed (see [daemon.md](daemon.md)). It
serves the sponsor's moments in [participants.md](participants.md): signing
in, attaching a mind, seeing what it reaches, the glance, trouble, and pulling
the key.

## Routes

| Route | What the sponsor sees |
| --- | --- |
| `/` | The companion cards, or the words that there are none yet. Each card has its portrait or initials, how many minds are attached, how many connections are ready or need a sign-in, and each account it holds by its label and kind, with *Remove*, confirmed in place; an account that needs a sign-in also offers *Sign in again*. One primary action, *Sign in*. |
| `/sign-in` | One way to sign in per kind of account: a button that goes straight to the provider's page, and *Name the account first* for a field holding the kind's hint. |
| `/companion/<id>/attach` | The companion's `mcpServers` block, keyed by the companion's moniker and naming it by its immutable id, with *Copy block*. The page says whether a mind is attached yet, and once one is, leads back to the companion. |
| `/companion/<id>` | The companion's page: its display name, moniker and portrait, each edited where it stands, and its attachments, then three sections in this order: Accounts, Services, Journal, and last the companion's removal, confirmed in place. *Attach a mind*, the primary action until one is, leads to the block; afterwards it reads *Host configuration*. |
| `/journal` | The whole journal as sentences, newest first, with paging. |

## The moments

**Signing in.** *Sign in* is how a companion comes to be. The sponsor picks a
kind of account, and the browser goes straight to the provider's page in the
same tab. With nothing typed the provider's page chooses the account; a kind
that cannot says so, and the field opens. When the browser comes back it lands
on the page of the companion the account belongs to:

- an account no companion holds makes a new one, in the same commit as the
  credential: its display name is the name the account goes by on its public
  profile, or the first word of its label; its moniker comes from that first
  word, with a number when it is taken; its portrait is the profile's picture,
  or the initials. All three change in place later;
- an account a companion holds opens that companion, signed in again. A
  revoked account stays reserved to its companion, so it comes back there too;
  removing the companion frees it.

Nothing else is asked. When a sign-in ends any other way, the page the browser
lands on says so in one sentence and offers one link back. An account that
belongs to another companion is named by that companion, and nothing changes.

**Accounts.** Accounts lists one row per account the companion holds, by its
label and the title its kind's adapter gives it, and a row is one of:

- signed in: the word *Signed in* and *Remove*. The provider's stable key for
  the account shows only inside the row's *Details*;
- needing a sign-in: the words *Sign in needed*, *Sign in again*, the page's
  primary action, which starts a sign-in for that same account without
  anything typed, and a quieter *Remove* beside it.

*+ Add* appears when a kind of account is not yet in use, and opens that kind's
way to sign in, for this companion. A companion with no account left says which
one was removed and that signing in with it again brings it back. Starting a
sign-in again ends one that was left behind at the provider's page.

**Services.** Services lists each installed service and what enables it: the
account the companion holds that the service takes, or the kind of account that
would. The sponsor connects nothing here: an account enables a service, and a
mind attached as the companion connects to it with the service's own connect
tool, by an address when the service has many, with none when it lives where
the account lives. Each connection the mind opened shows under its service, by
its name and its address, with one word and one action:

| Word | When | Action |
| --- | --- | --- |
| Ready | The connection and its account are ready. | *Revoke*, confirmed in place. |
| Sign in needed | The connection stands but its account needs signing in again. | *Sign in again*, which leads to the account's own *Sign in again*, and a quieter *Revoke*, confirmed in place. |
| Revoked | The sponsor revoked it, or removed its account. | *Open again* with a ready account the service takes, or *Sign in* when there is none. |

A revoked connection is shown only while nothing ready or newer holds its name,
and what the sponsor closed stays closed to the mind while that account stands:
only *Open again* opens it. An account signed in before a service needed more
of it is offered one thing there, signing it in again. A service's own page may
open the manager at `/?service=<name>&address=<origin>`: the manager takes those
once, clears them from the address, and says where to ask the mind to connect,
until a connection stands there. A lobby with no service installed says so.

**The glance.** The journal reads as sentences: who, as which companion, did
what, through which connection, and how it ended, in the names the sponsor
gave. It shows no ids, codes, secrets or content, leaves out the entries
written before an outcome was known, and pages back with *Older entries*.
New entries arrive as they are written. On a companion's page the renewals
that kept an account signed in are routine: the journal keeps them, and
the page shows them only while *Show routine entries* is pressed; the
control keeps its name and says whether it is pressed. The whole journal at `/journal` shows them all.

**Trouble.** When a provider refuses to renew an account, or the lobby
cannot tell whether a renewal landed, the account says *Sign in needed* on
the companion's card and in its row, with *Sign in again* as its one
action, and each of its connections says *Sign in needed*. Nothing else on
the page changes, except that the forms for new connections to a service
that takes that account say to sign it in again first. Signing the same
account in again brings the account and every connection back as they were.

**Pulling the key.** *Remove* appears wherever a signed-in account is shown,
the companion's card included, and *Revoke* wherever a connection is, each
confirmed in place, never in a dialog. Removing an account revokes it at its
provider and every connection made with it, and the page says so before it
happens; afterwards it says whether
the provider confirmed the revocation, offers none, or could not be
reached, that it cannot tell for an account that waited for a sign-in,
whose last key a renewal may already have replaced, and where to check
and remove it at the provider then, and that the provider was not asked
when the lobby held no key for the account. Revoking a connection says
that the lobby deleted what the service issued for it, and that the
service may still honor that until it expires there.

## The rules the page keeps

- No modal, alert, confirm or prompt. A destructive action is confirmed in
  place: the confirm button takes the focus, and Escape or *Keep* puts the
  action back.
- After every action the focus lands where its result is: the connection's
  row and its new action, the companion's card, the control that stands where
  an account was removed. A refusal of what a form asked is said inside that
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
and a fake world of Bluesky accounts on loopback, and prints their origins:

```sh
cargo run -p lobby-testkit --bin lobby-world
```

It prints one `name=value` line each for `LOBBY_AUTHSERVER`, `passbook`,
`passbook-holder`, `board`, `second-board`, `account-handle`, `pds`,
`space` and `control`, and runs until its input closes or Enter is pressed.
The `pds` is the fake account server Bluesky accounts live on, holding a
timeline with two posts by other accounts, which the second service reaches
with no address. The `space` is a fake of the first service's Space,
documented with that service under `docs/services`, with `/space/new-post`,
`/space/locked` and `/space/sign-ins-end` on the control page. `/accounts/older` makes
Bluesky sign-ins from then on grant only identity and the Space's leave,
as an account signed in before the second service needed more, and
`/accounts/asked` makes them grant what they ask for again: sign in after
the first, have the mind connect to the Space, and the page offers signing the
companion in again for the second. Opening a
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

Then walk the moments: sign in with Bluesky, which makes a companion of the
printed handle; add Passbook to it by typing the printed `passbook` origin;
attach a mind and have it connect to the printed `board` and the printed
`space`; revoke them; read the journal.
Every sign-in is approved at once as the printed account. For trouble, open
`control/keys/brief` before signing in with Passbook, have the mind connect,
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
