# Token exchange (RFC 8693)

**Off by default. Opt-in only.** Nothing changes unless you turn it on.

> ⚠️ **Not available against `auth.ai.camer.digital`.** As of 2026-08-31 the
> `token-exchange` grant is removed from the `governance-auth-cli` client
> ([`ai-helm-values`#329](https://github.com/ADORSYS-GIS/ai-helm-values/pull/329)), and
> `governance-auth-exchange-cli` was never registered. Turning this on against our gateway
> returns `400 unauthorized_client` — *"Client is not authorized to use token_exchange grant
> type"* (verified against the deployment).
>
> The reason is that the premise expired. Exchange exists to trade an identity-provider token
> for one of ours; since ADR-0025 gave `authz-idp` subject ownership, `governance-auth` logs
> in against our IdP directly and is handed our token to begin with. There is no second
> credential to trade for. Use `governance-auth login --device-code`.
>
> **This page still documents the feature accurately** — it is a capability of the binary,
> not of one deployment, and an install that registers an exchange client can still use it.
> Read it as reference, not as instructions for this gateway.

Some deployments want `token`/`otel-headers` to present a *different*, downstream-minted
credential rather than the raw token issued by `--issuer` — typically exchanging the
identity-provider access token for a project-scoped token minted by `lightbridge-authz`'s
native `/oauth2/token` endpoint. That is what this is for.

"Authenticate at A, present credentials minted by B" is the whole point, which is why
`--exchange-issuer` is a **separate field** from `--issuer` rather than a mode of it. The two
must be able to differ.

## Turning it on

```bash
governance-auth token \
  --token-exchange \
  --exchange-issuer https://auth.example \
  --exchange-client-id governance-auth-exchange-cli
```

Every one of these is also a `GOVERNANCE_AUTH_EXCHANGE_*` env var and a config-file key, with
the same precedence as every other option — see [`configuration.md`](./configuration.md). In
practice this belongs in a config file, not on the command line: `token` is invoked by a
background process, and the flags have to be present on *that* invocation too.

`--exchange-token-endpoint <url>` skips the discovery round trip if you already know the
endpoint, and wins over `--exchange-issuer` when both are set. `--exchange-scopes` requests
specific scopes; omitting it takes the exchange server's allow-list.

Enabling exchange without an `--exchange-client-id`, or without either an issuer or an
explicit token endpoint, is a loud configuration error naming the missing flag — not a
silently-disabled exchange.

## What goes on the wire

```
POST <token endpoint>
grant_type          = urn:ietf:params:oauth:grant-type:token-exchange
client_id           = <--exchange-client-id>
subject_token       = <the cached upstream access token>
subject_token_type  = urn:ietf:params:oauth:token-type:access_token
scope               = <--exchange-scopes>            (only if set)
```

### What it deliberately does *not* send

- **No `project_id`.** This deployment required it until upstream PR #309 merged; it is now
  optional and resolves to the subject's own auto-provisioned default project. Exposing a
  `--exchange-project-id` knob would reintroduce a required field the server itself dropped.
- **No `audience` / `resource`.** RFC 8693 defines the parameter, but this deployment's
  exchange handler never reads it — verified live and stated in the integration guide. The
  minted token's `aud`/`azp` are always exactly the requesting `client_id`, regardless of what
  is sent. A config knob that appears to scope the token's audience but silently does nothing
  would be worse than omitting it: a configuration that lies about its own effect.

## Fail closed

If exchange is enabled and the exchange fails for **any** reason — network error, malformed
response, `invalid_grant`, `invalid_client` — `token`/`otel-headers` exit non-zero and print
nothing to stdout. There is never a silent fallback to the un-exchanged upstream token.

This is structural, not a check that could be forgotten: `run` returns a `Result`, the caller
propagates it with `?` *before* its single `println!`, and there is no branch in between that
reaches for `session.access_token`. An operator who turned exchange on deliberately chose not
to emit the upstream token; emitting it on a bad day would hand the gateway a credential they
had opted out of.

## The audience requirement, which is where this actually fails

`lightbridge-authz` checks the subject token's audience **twice, against two different
values**, and rejects with `401 invalid_token` or `400 invalid_grant` respectively. A subject
token whose `aud` does not include *both* the bearer-validation audience *and* your
`--exchange-client-id` fails the exchange every time.

So `--exchange-client-id` must be registered in the exchange server's own client list, **and**
the upstream token you present must already carry that `client_id` in its `aud`. That is a
change to the *upstream* client registration, not to anything here. See lightbridge-authz's
`docs/token-exchange-integration.md`.

## An exchange server with no `authorization_endpoint`

`--exchange-issuer` tolerates a server that serves **no** `authorization_endpoint`, which
OIDC Discovery §3 permits for a provider that supports no authorization endpoint. Requiring
the field used to make this exact command fail with `missing field 'authorization_endpoint'`
([#145](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/145)). Fixed, and pinned
by a test.

⚠️ **`lightbridge-authz` is no longer an example of such a server.** This section used to say
it "has no `/authorize` route and omits the field". Since ADR-0025 moved subject ownership to
authz, `authz-idp` **does** serve `/authorize` and **does** advertise
`authorization_endpoint`; `lightbridge-console` runs the browser authorization-code flow
against it in production. The leniency above is still correct and still wanted -- it just no
longer describes this deployment.

That also retires the conclusion this section used to draw. "The exchange server cannot serve
the interactive login, so log in at the IdP and exchange at authz" is obsolete: authz **is**
the IdP now. For `governance-auth-cli` specifically the token-exchange grant has been removed
altogether -- see the banner at the top of this page.

## Verification status

Verified live against the deployed gateway, with an exchanged token as the only credential:

| Path | Result |
|---|---|
| Exchange (`/oauth2/token`) | 200, token returned |
| `POST /v1/chat/completions` | 200 |
| `POST /anthropic/v1/messages` | 200 |
| `POST /otel/v1/traces` | 200 |

Both the inference and telemetry planes accept the exchanged token; no identity-provider
token appears in the request path.

### Known gaps, observed rather than inferred

These are properties of the **gateway's** authorization rules, not of this binary, and are
recorded here because they change what an exchanged token actually buys you:

- **Per-model enforcement is inert.** A model absent from the token's `allowed_models` claim
  returned `200`. The live predicate checks only that `model_policy` is `allow_all` or
  `allowlist`; it never compares the requested model against the list. Whether
  `allowed_models` is meant to be authoritative or informational is a decision for whoever
  owns those rules.
- **Caller-kind claims.** Exchanged tokens assert `lightbridge_caller_kind: "api_key"` with an
  `api_key_id` that does not resolve on introspection —
  [lightbridge-authz#407](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/407).
