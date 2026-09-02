# Onboard a developer's Claude Code / Codex to the gateway

**When:** a developer wants Claude Code and/or Codex pointed at `api.ai.camer.digital`
with real per-developer OAuth2 (ADR-0010), instead of a manually-issued static key.

✅ **Operational, both login flows, as of 2026-08-31.** Each was completed end to end against
the live deployment -- browser sign-in included, not just the endpoints:

- **The gateway half.** All three paths return 200: `POST /v1/chat/completions`,
  `POST /anthropic/v1/messages`, and `POST /otel/v1/traces`.
- **`login --device-code`** -- works. No local listener, so it is also the answer on a
  headless box or when the callback ports are taken.
- **Plain `login` (browser)** -- works, via a pinned port block. Read the note on it below
  before changing anything about the ports; they are a contract with `ai-helm-values`.

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

Which login paths actually work:

- **`--device-code`** -- the `device_code` grant on the `governance-auth-cli` registration
  ([`ai-helm-values`#327](https://github.com/ADORSYS-GIS/ai-helm-values/pull/327)). Needs no
  Keycloak realm changes: you are verified through authz-idp's own relying-party leg, and the
  CLI never presents a subject token. No local listener either, so this is the flow for a
  headless box -- or for when all five callback ports below are occupied.
- **Plain `login` (browser) -- via pinned ports.** It used to be impossible:
  `governance-auth` took an *ephemeral* loopback port, while authz matches `redirect_uri` by
  exact string equality with no RFC 8252 §7.3 loopback exemption, so no registration could
  ever match a port the kernel picks at runtime.

  The CLI now binds the first free port of a fixed, pre-registered block --
  `17452-17456` -- and every one of them is registered as a `redirect_uri`
  ([`ai-helm-values`#331](https://github.com/ADORSYS-GIS/ai-helm-values/pull/331)).

  ⚠️ **This is a workaround for a spec violation on the server, not a design choice**
  ([ADR-0015](../adr/0015-pin-the-loopback-callback-to-a-registered-port-block.md)).
  §7.3 says an authorization server **MUST** allow any port for a loopback redirect,
  precisely so a native app can take an ephemeral one. Filed upstream as
  [marcjazz/authkestra#291](https://github.com/marcjazz/authkestra/issues/291); when it
  lands, the CLI goes back to an ephemeral port and the extra registrations are deleted.
  Until then the two lists are a **contract**: the ports in
  `app/governance-auth/src/oauth/callback_port.rs` and the `redirect_uris` in
  `ai-helm-values` must match byte-for-byte, or `/authorize` answers
  `400 invalid redirect_uri`.

  If all five ports are held by other processes, `login` refuses with a message naming
  them rather than silently falling back -- a fallback would bind fine and then fail at
  `/authorize`, which points at the wrong thing entirely. Use `--device-code`, which needs
  no local listener.

([#680](https://github.com/ADORSYS-GIS/ai-helm/issues/680) and
[#679](https://github.com/ADORSYS-GIS/ai-helm/issues/679) remain open, but they are about
Codex/Claude Code *gateway* integration -- this runbook once cited them as the blocker for
CLI client registration, which they never were.)

📖 This runbook is the short path. The exhaustive reference -- every flag, every env var,
every file this binary touches -- is [`docs/governance-auth/`](../governance-auth/README.md).

## 1. Install `governance-auth`

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://adorsys-gis.github.io/lightbridge-governance/install.sh | sh
```

Installs into `~/.local/bin` and prints the `export PATH=...` line if that isn't on your
`$PATH` already -- it deliberately does not edit your rc files, because `configure` (step 3)
writes its own managed block into them. Keep it current with `governance-auth self update`.

Six platforms are published, not four: macOS arm64/x64 and Linux x64/arm64 in **both** musl
and glibc flavours. The installer picks musl on Linux, which is the one to install -- the
glibc assets link against 2.39 and do not start on Ubuntu 22.04, Debian 12, RHEL 9 or Amazon
Linux 2023, failing in the dynamic loader before `main` in a way that reads as a corrupt
download rather than an OS mismatch.

`--version <tag>` pins a release, `--bin-dir <dir>` moves the install location, and the
checksum is verified before anything is put on your `$PATH` -- a missing or mismatched
`.sha256` is a refusal, not a warning. Full options and the uninstaller:
<https://adorsys-gis.github.io/lightbridge-governance/>

⚠️ Use an **absolute** path everywhere a config file names this binary. Codex spawns the
auth command directly rather than through a shell, so a bare `governance-auth` doesn't
resolve and the provider falls back to unauthenticated, silently.

## 2. Log in once

```bash
governance-auth login \
  --device-code \
  --issuer https://auth.ai.camer.digital \
  --client-id governance-auth-cli
```

It prints a verification URL and an 8-character code to stderr, then polls until you
complete it in a browser -- on any machine, which is why it needs no local listener and
cannot hit a port collision. `--open-browser` has no effect on this path; there is no local
redirect to open.

**Both flows work; `--device-code` is the one to reach for by default.** Dropping the flag
runs the browser authorization-code flow instead, which binds a local callback port:

```bash
governance-auth login \
  --issuer https://auth.ai.camer.digital \
  --client-id governance-auth-cli
```

That is a smoother desktop experience -- no code to retype -- and it is the right choice when
you are at a machine with a browser. Prefer `--device-code` when you are on a headless box,
when something else on your machine is holding `17452-17456`, or when you want the flow with
the fewest moving parts. The banner above explains why those ports are pinned and why that is
temporary.

`--issuer` is resolved through plain OIDC discovery, so this works against any
RFC 8414-compliant issuer -- `authz-idp` here, but nothing about `governance-auth` assumes
it. Note the issuer has **no realm path**: `authz-idp` is the provider itself, and a
`/realms/...` suffix 404s at discovery.

Set `GOVERNANCE_AUTH_ISSUER` / `GOVERNANCE_AUTH_CLIENT_ID` in your shell profile so you
don't have to pass `--issuer`/`--client-id` on every subsequent command -- `token`,
`status` and `logout` all read the same env vars. A config file works too and survives a
subprocess that doesn't inherit your environment; see
[`configuration.md`](../governance-auth/configuration.md).

### Or skip steps 3 and 4 entirely

Add `--gateway-url` and `--otel-endpoint` to the `login` above and it writes the wiring for
Claude Code, Codex and VS Code Copilot itself -- inference and telemetry both, only the keys
it owns, merged into your existing files rather than replacing them -- and it installs the
timer that drains Copilot's OTel spool to the collector:

```bash
governance-auth login \
  --device-code \
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

## 6. Token exchange (RFC 8693) -- not used here any more

**Do not reach for this. There is nothing to exchange against.** The
`urn:ietf:params:oauth:grant-type:token-exchange` grant was removed from the
`governance-auth-cli` client on 2026-08-31
([`ai-helm-values`#329](https://github.com/ADORSYS-GIS/ai-helm-values/pull/329)), and
`governance-auth-exchange-cli` -- the client id this section used to tell you to pass --
was never registered at all.

The reason is the whole point of Section 2: **we own the identity provider now.** Exchange
existed to trade a Keycloak access token for one of ours. Since ADR-0025 moved subject
ownership to `authz-idp`, the CLI logs in against our IdP directly and is handed our token
to begin with. There is no second credential to trade for.

Verified against the deployment on 2026-08-31 -- the grant is refused for this client:

```
POST /oauth2/token   grant_type=…token-exchange   client_id=governance-auth-cli
  → 400 {"error":"unauthorized_client",
         "error_description":"Client is not authorized to use token_exchange grant type"}
```

The `--token-exchange` / `--exchange-*` flags still exist in the binary, because the
capability is a property of `governance-auth` rather than of this deployment: another
install that registers an exchange client can still use them. They are OFF by default, so
nothing here changes for you. If you turn them on against *this* gateway you will get the
400 above.

⚠️ **Two corrections to what this section used to say**, kept rather than deleted because
both were repeated elsewhere and are worth un-learning explicitly:

- It claimed `lightbridge-authz` "has no `/authorize` route and omits the field, which OIDC
  Discovery §3 permits". **That is no longer true.** `authz-idp` serves `/authorize` and
  advertises `authorization_endpoint`; `lightbridge-console` runs the browser
  authorization-code flow against it in production today. The `#145` fix -- not requiring
  `authorization_endpoint` during discovery -- is still correct and still wanted, but the
  *reason* given for it has expired.
- It described the two Keycloak audience mappers as a prerequisite you should go and satisfy.
  They were never added, which is part of why no exchange ever completed. Nothing needs
  satisfying now; the requirement is gone with the grant.

If you are auditing an older deployment that still uses exchange, the mechanics are
unchanged and documented in `lightbridge-authz`'s `docs/token-exchange-integration.md`. The
one behaviour worth knowing either way: exchange **fails closed** -- if it is enabled and
the exchange fails, `token`/`otel headers` exit non-zero and print nothing, never falling
back to the un-exchanged upstream token.

## 7. Troubleshooting

- **`no cached session ... run \`governance-auth login\` first`** -- exactly what it
  says; the cache was empty (first run, or `logout` was called) and `token` correctly
  refused to launch an interactive browser on its own.
- **`token` fails after previously working** -- the refresh token was rejected
  (`invalid_grant`): the session was revoked, expired past its offline-session
  lifetime, or the client registration changed. Run `login` again; `governance-auth`
  never retries with a stale credential.
- **Claude Code error mentions the WAF / a 403 with an HTML body** -- not this binary.
  Claude Code's prompts contain XML-ish tags and source code that can trip body-inspection
  WAF rules on `/v1/messages`; that path needs an exemption on the gateway side, not a
  change here.
