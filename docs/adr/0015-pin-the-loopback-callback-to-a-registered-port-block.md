# ADR-0015: Pin the loopback callback to a registered port block until RFC 8252 §7.3 lands upstream

- Status: Accepted
- Date: 2026-08-31
- Decision owners: @stephane-segning

## Context

`governance-auth`'s browser login binds a loopback listener and redirects the authorization
server back to it. It took an **ephemeral** port -- `TcpListener::bind(("127.0.0.1", 0))` --
which is the pattern RFC 8252 §7.3 describes for native apps, and which §7.3 makes the server
responsible for accommodating:

> The authorization server **MUST** allow any port to be specified at the time of the request
> for loopback IP redirect URIs, to accommodate clients that obtain an available ephemeral port
> from the operating system at the time of the request.

Our authorization server does not. `authkestra-op`'s `ClientRegistration::allows_redirect_uri`
is a plain `==` against the registered list, with no loopback exemption, so a port chosen at
runtime can never match a registration. The browser flow failed **100% of the time** with
`400 invalid redirect_uri` -- verified against the deployment before this change, and not a
configuration mistake: registering a redirect URI only moves the failure from "grant refused"
to "invalid redirect_uri".

Filed upstream as [marcjazz/authkestra#291](https://github.com/marcjazz/authkestra/issues/291),
confirmed unfixed on their default branch (past 0.6.3). Source of truth:
[#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84).

## Decision

Pin the callback to a **block of five fixed ports, `17452-17456`**, tried in order, and
register every one of them as a `redirect_uri` on the `governance-auth-cli` client. Refuse
loudly when all five are held rather than falling back to an ephemeral port.

Treat this as a **workaround with a deletion condition**, not an architecture: when
authkestra#291 lands, revert to `bind(("127.0.0.1", 0))`, delete
`app/governance-auth/src/oauth/callback_port.rs`, and drop the four extra registrations.

The block is chosen by constraint, not preference:

| constraint | reason |
|---|---|
| **below 32768** | the OS draws ephemeral ports from 32768-60999 (Linux) and 49152-65535 (macOS/IANA Dynamic). A "fixed" port inside either window can be handed to an unrelated process at any moment -- login would fail intermittently and unreproducibly, the worst failure mode for a credential helper and one that looks like a server bug |
| **above 1024** | lower ports need root; this runs as a developer |
| unassigned, quiet | not in `/etc/services`, and clear of 3000/5000/8000/8080/9000 |

Past those constraints the specific numbers are **arbitrary by design**. Nothing is encoded in
them and nobody should preserve them for their own sake; the *window* is what is load-bearing,
and that is what the test asserts.

## Consequences

**Positive**
- The browser login flow works at all, which it did not before.
- Five ports rather than one means a single occupied port does not lock a developer out --
  which is the failure §7.3 exists to prevent, and which a single fixed port would reintroduce.
- Failing loudly keeps the error at its cause. A silent ephemeral fallback would bind fine and
  then fail at `/authorize`, making a local port collision look like a server or registration
  problem.

**Negative**
- **The port list is a cross-repo contract.** `CALLBACK_PORTS` in
  `app/governance-auth/src/oauth/callback_port.rs` and `redirect_uris` in `ai-helm-values`
  `environments/prod/values/lightbridge-app.yaml` must match byte-for-byte. Changing one
  without the other yields `400 invalid redirect_uri`. Registration lands **first**: a
  registration the CLI does not use is inert, an unregistered CLI port is a hard failure.
- A developer running five conflicting services in that block cannot use the browser flow.
  `--device-code` needs no local listener and is the documented escape.
- We carry a workaround for someone else's bug, with the usual risk that "temporary" becomes
  permanent. The deletion condition is written into the module doc, the values file, the
  runbook and this ADR specifically to resist that.

**Neutral / follow-ups**
- `require_pkce: true` is set server-side on the client. The CLI's PKCE S256 was already
  unconditional with a regression test forbidding a way to disable it; this is the other half.
- When #291 lands, this ADR should be superseded rather than edited.

## Alternatives considered

- **Wait for the upstream fix** -- rejected: it is someone else's repository on someone else's
  schedule, and the browser flow is broken meanwhile. The issue is filed and this is reversible
  in one commit, so waiting bought nothing.
- **Device-code only, drop the browser flow** -- rejected: it works and was shipped first, but
  it is a worse desktop UX (retype a code) for the common case of a developer who *has* a local
  browser. Keeping only the fallback because the primary needs one config block is the wrong
  trade.
- **A single fixed port** -- rejected: reintroduces exactly the failure §7.3 exists to prevent.
  One unrelated process holding it locks the developer out with no recourse, and the resulting
  bug report ("login just hangs on my machine") is expensive to diagnose.
- **Prefix or wildcard redirect matching upstream** -- rejected as the thing to *ask for*:
  relaxing exact matching generally is a well-known open-redirect footgun. The upstream request
  is narrowly a loopback-only, port-only exemption, on IP literals (`127.0.0.1`/`::1`) and never
  the name `localhost`, which §8.3 advises against because it depends on name resolution the app
  does not control.
- **Fork `authkestra-op`** -- rejected: a fork of an auth library is a permanent maintenance and
  supply-chain cost for a defect with a filed issue and a small patch.

## Related

- Upstream: [marcjazz/authkestra#291](https://github.com/marcjazz/authkestra/issues/291) --
  the condition under which this ADR is superseded.
- Issue: [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
- Runbook: [`docs/runbooks/onboard-a-developer-ai-client.md`](../runbooks/onboard-a-developer-ai-client.md)
- Reference: [`docs/governance-auth/commands.md`](../governance-auth/commands.md)
- ADR-0010 -- `governance-auth` as a credential helper, which this constrains.
- `ai-helm-values` `environments/prod/values/lightbridge-app.yaml` -- the other half of the
  port contract.
