# atproto credentials

The atproto credential adapter (`crates/credentials/atproto`) signs a
companion's account in with atproto OAuth, as the client the protocol allows
a program on the sponsor's own machine to be. It is the only reader of the
material it hands the daemon. This page says what it sends, what it keeps,
what it refuses, and how the owner checks it against the real network.

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
   `/signin/atproto/done?outcome=<outcome>`, so the code and the `state` leave
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
   same authorization server.

The access token is never parsed: an opaque token signs in as well as a JWT.

## What the lobby keeps

The credential record holds the companion, the kind `atproto`, the account
(the DID the token's `sub` named), the permissions (the granted scope's
words, `atproto`), and its state. The material, sealed beside it as
`<credential id>.material` (DPAPI in the user's scope on Windows, mode 0600
elsewhere), is one JSON object tagged `atproto-oauth-1`: `subject`, `access`,
`refresh`, `pds`, `authserver`, `clientId` (a refresh must present the same
one), `dpopKey` (the P-256 scalar) and `permissions`. Nothing of the material
reaches the state file, a projection, the journal, the trace log or a page.

An account belongs to one companion. Revoking the credential keeps it as
history and keeps the account reserved to that companion; signing the
companion in again makes a new credential.

## Permissions

A sign-in asks for `atproto`, the account's identity, and nothing else. The
credential declares what was granted, so a service that needs more asks the
sponsor to sign the companion in again with the wider scope; nothing but a
new sign-in widens a credential's permissions, and a refresh of the OAuth
grant keeps them as they are.

The adapter answers no authorization ask: a service that asks for a bearer
token or a proof for an audience is refused as unsupported before the
material is read. Refreshing the grant, revoking it at the provider and
minting proofs for services come with the service adapters that need them.

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
8. Revoke the credential on the manager page; it stays listed as revoked.
   Sign the first companion in again, this time with the account's handle
   typed in the account field, to check resolution through the handle
   resolver and the DID directory: a new credential is listed beside the
   revoked one.
9. Revoke that credential too, then revoke the app's access in the
   provider's settings, stop the daemon, and delete the check's data
   directory.

Record what each step showed in the slice's notes.
