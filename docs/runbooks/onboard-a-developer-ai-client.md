# Onboard a developer's Claude Code / Codex to the gateway

**When:** a developer wants Claude Code and/or Codex pointed at `api.ai.camer.digital`
with real per-developer OAuth2 (ADR-0010), instead of a manually-issued static key.

⚠️ **Partially operational.** Split by half, as of 2026-08-31:

- **The gateway half is live and verified.** With a token obtained by exchange, all three
  paths return 200: `POST /v1/chat/completions`, `POST /anthropic/v1/messages`, and
  `POST /otel/v1/traces`. See [Section 6](#6-optional-token-exchange-rfc-8693).
- **The interactive `login` half is one config line away.** The identity provider is no
  longer the missing piece -- see below.

**`authz-idp` is now a full OpenID Provider, and `auth.ai.camer.digital` is it.**
`GET https://auth.ai.camer.digital/.well-known/openid-configuration` returns 200 and
advertises `authorization_code`, `refresh_token`, RFC 8628 `device_code` and RFC 8693
token-exchange, with `/authorize`, `/oauth2/device_authorization`, `/oauth2/introspect`,
`/oauth2/userinfo` and `/oauth2/end_session`. `lightbridge-console` already runs the
browser flow against it in production.

⚠️ This **supersedes** an earlier note here claiming the exchange server "serves no
`authorization_endpoint`, so it cannot host an interactive login at all." That was true
when authz was a thin exchange in front of Keycloak and is now false. Since ADR-0025 moved
subject ownership to authz, the shape is *log in at authz, which brokers to Keycloak
internally*. Keycloak is still there; it is no longer something you point a flag at.

What is actually still missing is narrower: `governance-auth-cli` is registered as a
client, but only for `token-exchange` and `refresh_token`, so **both** `login` paths are
refused today.

- **`--device-code`** -- unblocked by adding one grant type to the client registration
  ([`ai-helm-values`#327](https://github.com/ADORSYS-GIS/ai-helm-values/pull/327)). Once
  that merges and syncs, this is the path that works. It needs no Keycloak realm changes:
  you are verified through authz-idp's own relying-party leg, and the CLI never presents a
  subject token.
- **The browser flow** -- still blocked, and not by configuration. `governance-auth` binds
  an *ephemeral* loopback port, while authz matches `redirect_uri` by exact string equality
  with no RFC 8252 §7.3 loopback exemption, so no registered value can ever match. It needs
  either a fixed port here or §7.3 upstream in `authkestra-op`. Tracked on
  [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84).

Until #327 lands, Steps 2-5 describe the target flow rather than something you can run
today. ([#680](https://github.com/ADORSYS-GIS/ai-helm/issues/680) and
[#679](https://github.com/ADORSYS-GIS/ai-helm/issues/679) remain open, but they are about
Codex/Claude Code *gateway* integration -- this runbook previously cited them as the
blocker for CLI client registration, which they never were.)

📖 This runbook is the short path. The exhaustive reference -- every flag, every env var,
every file this binary touches -- is [`docs/governance-auth/`](../governance-auth/README.md).

## 1. Install `governance-auth`

Download the release binary for your platform (macOS arm64/x64, Linux x64/arm64) and put
it on `$PATH`. There is no package manager entry yet -- copy it into
`~/.local/bin` or equivalent. Keep it current with `governance-auth self-update`.

⚠️ Use an **absolute** path everywhere a config file names this binary. Codex spawns the
auth command directly rather than through a shell, so a bare `governance-auth` doesn't
resolve and the provider falls back to unauthenticated, silently.

## 2. Log in once

```bash
governance-auth login \
  --issuer https://auth.ai.camer.digital \
  --client-id governance-auth-cli
```

`--issuer` is resolved through plain OIDC discovery, so this works against any
RFC 8414-compliant issuer -- `authz-idp` here, but nothing about `governance-auth` assumes
it. Note the issuer has **no realm path**: `authz-idp` is the provider itself, and a
`/realms/...` suffix 404s at discovery.

Prints the URL to visit and waits; it does NOT open your browser automatically (a
headless SSH session, a container, or a CI runner would otherwise get a `DISPLAY`/
`xdg-open` that fails or hijacks an unrelated desktop). Pass `--open-browser` (or set
`GOVERNANCE_AUTH_OPEN_BROWSER=true`, or `open_browser = true` in a config file --
see `governance-auth --help`) to restore the old auto-open behaviour on a machine
where it's actually useful.

On a headless box (SSH, a Coder cloud workspace with no local browser) use
`--device-code` instead: it prints a verification URL and code to stderr and polls
until you complete it elsewhere. Independent of `--open-browser` -- there is no
browser to open for a device-code flow either way.

Set `GOVERNANCE_AUTH_ISSUER` / `GOVERNANCE_AUTH_CLIENT_ID` in your shell profile so you
don't have to pass `--issuer`/`--client-id` on every subsequent command -- `token`,
`status` and `logout` all read the same env vars. A config file works too and survives a
subprocess that doesn't inherit your environment; see
[`configuration.md`](../governance-auth/configuration.md).

### Or skip steps 3 and 4 entirely

Add `--gateway-url` and `--otel-endpoint` to the `login` above and it writes the wiring for
Claude Code, Codex and VS Code Copilot itself -- inference and telemetry both, only the keys
it owns, merged into your existing files rather than replacing them:

```bash
governance-auth login \
  --issuer https://auth.ai.camer.digital \
  --client-id governance-auth-cli \
  --gateway-url https://api.ai.camer.digital \
  --otel-endpoint https://otel.ai.camer.digital \
  --otel-token "$OTLP_INGEST_TOKEN"
```

`governance-auth configure` re-runs just that part for an existing session -- after
installing one of the tools for the first time, or when the endpoint or ingest token
changed. Exactly which keys in which files:
[`files.md`](../governance-auth/files.md).

The two sections below are the by-hand equivalent, for when you want to see it or when
you're editing managed settings your org pushes.

## 3. Wire it into Claude Code

In `~/.claude/settings.json` (or the managed-settings equivalent your org pushes):

```json
{
  "apiKeyHelper": "governance-auth token",
  "env": {
    "ANTHROPIC_BASE_URL": "https://api.ai.camer.digital/anthropic"
  }
}
```

Claude Code runs `governance-auth token`, caches the printed value for 5 minutes, and
re-runs it on an HTTP 401. ⚠️ `apiKeyHelper` cannot coexist with
`forceLoginMethod`/`forceLoginOrgUUID` managed settings -- if your org sets those, this
developer's config needs an explicit carve-out.

## 4. Wire it into Codex

In `~/.codex/config.toml`:

```toml
[model_providers.camer]
name = "camer"
base_url = "https://api.ai.camer.digital/v1"
wire_api = "responses"

[model_providers.camer.auth]
command = ["governance-auth", "token"]
refresh_interval_ms = 240000
```

`refresh_interval_ms` at 240s keeps Codex's proactive refresh comfortably inside the
realm's 300s (`accessTokenLifespan`) access-token window. `auth` cannot be combined with
`env_key` or `experimental_bearer_token` in the same provider block -- pick one.

## 5. Verify

```bash
governance-auth status
```

Reports whether a session is cached and its freshness. A real request from either tool
is the actual proof -- but confirming the helper alone rules out half the failure modes
before touching the tools.

## 6. Optional: token exchange (RFC 8693)

Some deployments want `token`/`otel-headers` to present a DIFFERENT, downstream-minted
credential rather than the raw token issued by `--issuer` -- e.g. exchanging a Keycloak
access token for a project-scoped token minted by `lightbridge-authz`'s native
`/oauth2/token` endpoint. This is OFF by default; nothing changes unless you opt in.

```bash
governance-auth token \
  --token-exchange \
  --exchange-issuer https://auth.ai.camer.digital \
  --exchange-client-id governance-auth-exchange-cli
```

The `client_id` must be registered in the exchange server's own client list, and the
upstream token you present must carry that `client_id` in its `aud` -- `lightbridge-authz`
checks the subject token's audience twice, against two different values, and rejects with
`401 invalid_token` or `400 invalid_grant` respectively. See its
`docs/token-exchange-integration.md`.

⚠️ `--exchange-issuer` works against a server that serves **no** `authorization_endpoint`.
`lightbridge-authz` is exactly that: it has no `/authorize` route and omits the field, which
OIDC Discovery §3 permits for a provider that supports no authorization endpoint. Requiring
it here used to make this exact command fail with `missing field 'authorization_endpoint'`
(#145) -- fixed, and pinned by a test whose mock now reproduces authz's real document.

(`--exchange-token-endpoint <url>` skips the discovery round trip if you already know
the endpoint; `--exchange-scopes "..."` requests specific scopes.) Every one of these
is also settable as `GOVERNANCE_AUTH_EXCHANGE_*` / `GOVERNANCE_AUTH_TOKEN_EXCHANGE` env
vars or config-file keys, with the same flag > env > per-user file > machine-wide file
precedence as every other option.

⚠️ **Fails closed.** If exchange is enabled and the exchange request fails for any
reason, `token`/`otel-headers` exit non-zero and print nothing to stdout -- never a
silent fallback to the un-exchanged upstream token. See the Keycloak-client audience
requirements in lightbridge-authz's `docs/token-exchange-integration.md` before turning
this on: a subject token whose `aud` doesn't include both the bearer-validation audience
and your `--exchange-client-id` will fail the exchange every time.

## 7. Troubleshooting

- **`no cached session ... run \`governance-auth login\` first`** -- exactly what it
  says; the cache was empty (first run, or `logout` was called) and `token` correctly
  refused to launch an interactive browser on its own.
- **`token` fails after previously working** -- the refresh token was rejected
  (`invalid_grant`): the Keycloak session was revoked, expired past its offline-session
  lifetime, or the client registration changed. Run `login` again; `governance-auth`
  never retries with a stale credential.
- **Claude Code error mentions the WAF / a 403 with an HTML body** -- not this binary.
  Claude Code's prompts contain XML-ish tags and source code that can trip body-inspection
  WAF rules on `/v1/messages`; that path needs an exemption on the gateway side, not a
  change here.
