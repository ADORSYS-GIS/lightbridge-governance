# Onboard a developer's Claude Code / Codex to the gateway

**When:** a developer wants Claude Code and/or Codex pointed at `api.ai.camer.digital`
with real per-developer OAuth2 (ADR-0010), instead of a manually-issued static key.

⚠️ **Not yet operational.** This runbook documents the target flow once the
`ai-helm`-side Keycloak client exists ([#680](https://github.com/ADORSYS-GIS/ai-helm/issues/680),
[#679](https://github.com/ADORSYS-GIS/ai-helm/issues/679)) -- see ADR-0010's Appendix for
exactly what that registration needs. Until then, `governance-auth login` has nothing to
authenticate against.

## 1. Install `governance-auth`

Download the release binary for your platform (macOS arm64/x64, Linux x64/arm64) and put
it on `$PATH`. There is no package manager entry yet -- copy it into
`~/.local/bin` or equivalent.

## 2. Log in once

```bash
governance-auth login \
  --issuer https://auth.ai.camer.digital/realms/platform \
  --client-id governance-auth-cli
```

`--issuer` is resolved through plain OIDC discovery, so this works against any
RFC 8414-compliant issuer -- Keycloak today, but nothing about `governance-auth` assumes
it.

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
`status` and `logout` all read the same env vars.

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
