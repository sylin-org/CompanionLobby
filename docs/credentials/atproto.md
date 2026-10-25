# atproto credentials

The atproto credential adapter (`crates/credentials/atproto`) signs a
companion's account in with atproto OAuth, as the client the protocol allows
a program on the sponsor's own machine to be, keeps the grant alive, and
revokes it at the provider. It is the only reader of the material it hands
the daemon. This page says what it sends, what it keeps, what it refuses,
and how the owner checks it against the real network.

## Signing in

The sponsor starts a sign-in on a companion's page in the manager, in the
Accounts section, with the account field left empty or holding a handle or a
DID. The manager page then shows the sign-in's own page, which names the
companion and the account meant, and links to the provider's page in a new
tab.

1. **Where it goes.** With nothing typed, the sign-in goes to the default
   authorization server, `https://bsky.social`, whose own pages let the
   sponsor choose the account; nothing names the account in advance, so no
   `login_hint` is sent and nothing is resolved before the request is pushed.
   With a handle typed (`ada.example.test`, a leading `@` and any case
   accepted), the handle resolver (`https://public.api.bsky.app`,
   `com.atproto.identity.resolveHandle`) names its DID; failing that, the
   handle's own host answers `/.well-known/atproto-did`. A did:plc document
   comes from the DID directory (`https://plc.directory`), a did:web document
   from its host's `/.well-known/did.json` (hostname-level did:web only). The
   document must name the DID itself and claim the handle back, and its
   `#atproto_pds` service is the account's PDS. The PDS's
   `/.well-known/oauth-protected-resource` names exactly one authorization
   server.
2. **The server's promises.** The authorization server's
   `/.well-known/oauth-authorization-server` must name itself as `issuer`,
   require pushed authorization requests, support the `code` response type,
   the authorization code grant, PKCE `S256`, public clients (`none`), DPoP
   with `ES256`, the `atproto` scope, the `iss` response parameter and client
   id metadata documents, and keep its authorize, PAR and token endpoints on
   its own origin. A server missing any of it is refused before anything is
   pushed.
3. **The pushed request.** The client id is
   `http://localhost?redirect_uri=<the return route>&scope=atproto`, which
   declares the lobby's one return route,
   `http://127.0.0.1:<port>/signin/atproto/return`, and the one scope a
   sign-in asks for. The PAR carries that client id, that redirect URI,
   `response_type=code`, `scope=atproto`, the lobby's random `state`, and an
   S256 challenge over a fresh 32-byte verifier; no `nonce` parameter, since
   the pushed request URI takes its place. A DPoP key made for this sign-in
   alone signs a proof on each request: `typ` `dpop+jwt`, the public key as a
   JWK, `htm` and `htu` (without query), `iat`, `exp` two minutes later, a
   fresh `jti`, and the server's nonce once it has issued one; the server's
   first answer (`use_dpop_nonce`) issues it and the request is made once
   more.
4. **The wait.** The verifier and the key wait in the daemon's memory under
   the `state`, ten minutes at most, used once. The sponsor may cancel the
   sign-in from its page; a companion waits on one sign-in at a time.
5. **The return.** The provider sends the browser to the return route, which
   completes the sign-in and sends the browser straight on to
   `/signin/done?outcome=<outcome>`, so the code and the `state` leave
   the address bar at once. The return must carry the `state` of a sign-in
   still waiting, an `iss` naming the very authorization server it went to,
   and a code; a return carrying `error` signs nothing in. The code is
   exchanged at the token endpoint with the verifier, the same redirect URI
   and client id, and a proof from the same key. The answer must be a
   DPoP-bound token pair (`token_type` `DPoP`, an access token and a refresh
   token) whose `sub` is a DID and whose `scope` includes `atproto` and asks
   for nothing more. When the sponsor typed an account, `sub` must be that
   account: another account the provider signs in never becomes the
   credential. Only then is `sub` resolved: its document's PDS must be the
   one the typed account named, if one was typed, and that PDS must name the
   same authorization server. The document's handles are then resolved in
   turn, and the first that names the DID back is the account's label.

The access token is never parsed: an opaque token signs in as well as a JWT.
When it expires is read from the token answer's `expires_in` (five minutes
when the answer does not say, thirty days at most).

When the companion holds an atproto credential that needs signing in again,
**Sign in again** starts a sign-in for that account from the credential's
own material: the account is its DID, resolved as in step 1, and only that
account can complete the sign-in, which restores the credential and its
connections.

## What the lobby keeps

The credential record holds the companion, the kind `atproto`, the account
(the DID the token's `sub` named), the label the sponsor reads for it (the
first handle the DID's document claims that resolves back to that DID, or
the DID itself when none does; the manager page shows the DID only inside
the account's details), the permissions (the granted scope's
words, `atproto`), and its state. The material, sealed beside it as
`<credential id>.material` (DPAPI in the user's scope on Windows, mode 0600
elsewhere), is one JSON object tagged `atproto-oauth-1`: `subject`, `access`,
`accessExpiresAt` (seconds since the Unix epoch), `refresh`, `pds`,
`authserver`, `clientId` (a refresh must present the same one), `dpopKey`
(the P-256 scalar) and `permissions`. Material without every one of these,
or with one out of its shape, is not the adapter's: a credential whose
material it cannot read needs signing in again. Nothing of the material
reaches the state file, a projection, the journal, the trace log or a page.

An account belongs to one companion. Revoking the credential keeps it as
history and keeps the account reserved to that companion; signing the
companion in again makes a new credential. Removing the companion deletes
its credentials and their material here; the grant at the provider stays
until the sponsor revokes the app's access in the provider's settings.

## Keeping the grant alive

The daemon asks the adapter to renew the credential, and the adapter
refreshes the grant a minute before the access token expires; until then it
answers that the material is fresh and reaches nothing.

1. **The server first.** The authorization server's metadata is read and
   checked as for a sign-in. A server that cannot be read, or answers 429,
   spent nothing: the lobby asks again later.
2. **The mark.** The slot marks the material rotating, which the daemon
   writes to the disk, synced, before the adapter goes on. Nothing is sent
   until it did.
3. **The refresh.** The token endpoint receives `grant_type=refresh_token`,
   the refresh token and the client id stored at sign-in, with a proof made
   with the grant's own key. The first refresh carries the nonce the server
   issued to that key last (at the sign-in, or at the refresh before), kept
   per server origin and per key for five minutes, 64 at most; a
   `use_dpop_nonce` answer issues a fresh one and the request is made once
   more, never a third time.
4. **The answer.** A 200 that keeps the profile (a `DPoP` token type, an
   access and a rotated refresh token, the same `sub`, and a scope inside the
   one the grant held) is committed through the slot on top of exactly the
   material it was made from, which clears the mark. 429 spent nothing; the
   mark is cleared and the lobby asks again later. Any other 4xx is the
   server ending the grant: nothing was spent, the mark is cleared, and the
   credential needs signing in again. Anything else (no answer read whole, a
   5xx, an answer that breaks the profile, another `sub`, a wider scope, or
   new material the daemon could not keep) may have spent the refresh token
   with nothing new kept: the mark stays, and the credential needs signing in
   again without that token ever being presented again.

One grant refreshes once at a time: the adapter holds a lock of its own per
credential across the whole refresh, and a second renewal of the same
credential waits for it and reads the new material.

## Revoking at the provider

When the sponsor revokes the credential, the adapter presents the grant's
refresh token (`token_type_hint=refresh_token`, with the client id and a
proof) to the revocation endpoint the authorization server's metadata names
on its own origin. A 200 is the provider confirming it, except for a
credential that waited for a sign-in: its refresh token may already have
been rotated by a renewal whose answer never arrived, and a revocation
endpoint answers 200 for a token it does not know, so the lobby still
sends it but tells the sponsor it cannot tell whether the grant ended. A
server that names no revocation endpoint cannot be asked: the grant ends
there when it expires, and the sponsor reads so.

## Permissions

A sign-in asks for `atproto`, the account's identity, and for what the
lobby's services need an account of this kind granted, each as one word of
the scope (a service that proves the account to itself declares
`rpc:<method>?aud=*`, the granular permission the PDS checks before it
mints a proof). The loopback client id declares exactly that scope. The
credential declares what was granted, so a service that needs more asks
the sponsor to sign the companion in again with the wider scope; nothing
but a new sign-in widens a credential's permissions, and a refresh of the
OAuth grant keeps them as they are.

## Minting a service's proof

A service adapter that needs the account proven to it asks for a proof for
the audience it declared and an XRPC method. The adapter answers only:

- an audience that is an exact service reference, `<did>#<service>`, never
  a bare DID, whose DID is a did:plc or a did:web (a port only on
  `localhost`, written `%3A<port>`), at most 512 bytes; and a method that
  is an NSID. Anything else is refused as unsupported before the material
  is read;
- a method the credential was granted, as `rpc:<method>?aud=*` or for that
  very audience. Without it the answer is that the account was signed in
  before the service needed it, that revoking it and signing it in again
  grants that, and that revoking it revokes its other connections too,
  before the PDS hears anything.

Holding the credential's lock, it renews the grant first when it is due,
then asks the account's PDS:
`GET /xrpc/com.atproto.server.getServiceAuth?aud=<audience>&lxm=<method>&exp=<now + at most 120>`,
with the access token under `DPoP` and a proof made with the grant's key
whose `htu` is the URL without its query and whose `ath` binds the access
token, carrying the PDS's own nonce for that key once it issued one. A 401
with a fresh `DPoP-Nonce` that says `use_dpop_nonce`, in the body's
`error` or in the `WWW-Authenticate` header, is answered once with the
nonce; the nonce is kept per PDS origin and key for later proofs. The
token of a 200 is the proof, which the service is presented under
`Bearer`. A 403 is the PDS refusing the method (revoke the account and
sign in again to grant it). Any other 401 is an access token the PDS
stopped taking, which can happen before it expires: the grant is renewed
once, due or not, and the proof asked for once more. Only a renewal the
authorization server refuses, or a second such 401, leaves the credential
needing signing in again.

## Reaching the network

The adapter reaches only public HTTPS origins, through the outbound agent:
each name is resolved once and every address it resolves to must be public,
TLS is verified against the roots the agent carries, no redirect is followed,
no proxy is taken from the environment, and every request has a deadline.
`LOBBY_AUTHSERVER` may name another public HTTPS origin as the default
authorization server; any other value stops the daemon from starting.

The tests point the adapter at the test kit's fake authorization server and
PDS on the loopback address, through `companion-lobby-synthetic`; the
product's `companion-lobby` refuses a loopback `LOBBY_AUTHSERVER`, and no
test reaches the network.

## The live check

The owner runs this check by hand with a real account, on Windows; no test
does. It also settles that the authorization server accepts a client id
whose redirect URI carries the return route's path.

1. Build and stop any daemon that runs:

   ```powershell
   cargo build --release
   .\target\release\companion-lobby.exe stop
   ```

2. Use a data directory of its own for the check, so nothing else is in it:

   ```powershell
   $env:LOBBY_DATA_DIR = Join-Path $env:TEMP 'companion-lobby-live-check'
   .\target\release\companion-lobby.exe open
   ```

3. On the manager page, create a companion, open its page, leave the account
   field empty and choose **Sign in with atproto**. The sign-in's page names
   the companion; choose **Continue to bsky.social**.
4. In the provider's tab, sign in and approve. The tab ends on a page saying
   the account is signed in, and its address carries no code and no state.
   If the provider shows an error instead (an unknown client or redirect
   URI), note its words: the client id was refused.
5. Back on the manager page, the companion's Accounts section lists the
   account's DID, the permission `atproto` and **signed in**.
6. Check that nothing secret is written in the clear, and that the material
   file is sealed:

   ```powershell
   $dir = $env:LOBBY_DATA_DIR
   Get-ChildItem $dir -File | Where-Object Extension -ne '.material' |
     Select-String -Pattern 'eyJ', 'refresh', 'dpopKey', 'access_token'
   Get-ChildItem $dir -Filter *.material | Select-String -Pattern 'eyJ', 'atproto-oauth-1'
   ```

   Both searches find nothing.
7. Start another sign-in for a second companion with the same account: the
   tab ends on a page saying nothing was signed in, because the account
   belongs to the first companion.
8. Leave the daemon running past the access token's expiry (about two
   hours covers it) and look at the companion's journal: it says the lobby
   kept the account signed in, and the account still reads **signed in**.
9. Revoke the credential on the manager page; the note says whether the
   provider confirmed it, and the provider's connected-apps settings
   show the grant gone. Sign the first companion in again, this time with
   the account's handle typed in the account field, to check resolution
   through the handle resolver and the DID directory: a new credential is
   listed beside the revoked one.
10. Revoke that credential too, check the provider's settings once more,
    stop the daemon, and delete the check's data directory.

Record what each step showed in the slice's notes.
