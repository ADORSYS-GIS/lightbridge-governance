# `governance-auth`

The OAuth2 credential helper that points a developer's Claude Code, Codex and VS Code
Copilot at this org's AI gateway, and their telemetry at this org's collector.

It is **not a server**. It is a pure OAuth2 *client*: `login` runs the interactive flow
once, `token` prints a currently-valid access token on every subsequent call, and the two
AI clients invoke `token` themselves through their own credential-helper hooks
(`apiKeyHelper`, `[model_providers.*.auth] command`). Nothing in this binary makes an
authorization decision — the authorization server validates its own tokens and the gateway
validates the JWTs it accepts.

| Page | What's in it |
|---|---|
| [`commands.md`](./commands.md) | Every subcommand, what it does, and its stdout/stderr contract. |
| [`configuration.md`](./configuration.md) | Every option × flag × env var × config-file key, and the five-layer precedence. |
| [`files.md`](./files.md) | Every path this binary reads or writes, and which keys it owns inside each foreign config file. |
| [`token-exchange.md`](./token-exchange.md) | The opt-in RFC 8693 flow, its audience requirements, and its fail-closed contract. |
| [`troubleshooting.md`](./troubleshooting.md) | Symptom → cause → fix, for the failures this has actually produced. |

Decisions and background live elsewhere and are not repeated here:

- [ADR-0010](../adr/0010-governance-auth-keycloak-oauth2-credential-helper.md) — why a
  credential helper at all, instead of static per-developer API keys.
- [ADR-0012](../adr/0012-governance-auth-packaging-and-distribution.md) — packaging,
  distribution, and the config-precedence decision this binary implements.
- [`docs/integrations/ai-client-flows.md`](../integrations/ai-client-flows.md) — how each
  client behaves when a credential helper fails.
- [`docs/runbooks/onboard-a-developer-ai-client.md`](../runbooks/onboard-a-developer-ai-client.md)
  — the "tell a tired person what to type" version of this.

## What it does, in order

```
governance-auth login                       # once, interactively
  ├─ OIDC discovery against --issuer
  ├─ authorization code + PKCE  (loopback :17452-17456)  or --device-code
  ├─ session → <state>/governance-auth/<hash>.json, mode 0600
  └─ configure: writes Claude Code / Codex / VS Code / shell rc,
     and installs the timer that drains Copilot's OTel spool

governance-auth token                       # every request, invoked by the client
  ├─ load session (under a file lock)
  ├─ refresh if within the expiry skew  ── fails closed if the refresh is rejected
  ├─ optionally exchange it (RFC 8693)  ── fails closed if the exchange is rejected
  └─ print the access token to stdout, and nothing else
```

## Install

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://adorsys-gis.github.io/lightbridge-governance/install.sh | sh
```

Six targets are published per release (macOS arm64/x64, Linux x64/arm64, each in musl and
glibc). The installer picks one from `uname`, **verifies the published `.sha256` before
anything reaches your `$PATH`**, and prints the `export PATH=...` line rather than editing
your rc files — `configure` writes its own managed block into those, and two writers to one
`.zshrc` is a mess somebody untangles by hand.

`--version <tag>` pins a release and `--bin-dir <dir>` moves the location; `--libc gnu`
selects the glibc build, which you want only if musl's lack of NSS breaks DNS resolution on
your machine. Options, the uninstaller, and the script as readable plain text:
<https://adorsys-gis.github.io/lightbridge-governance/>

To remove it — binary, drain schedule, session (revoked at the IdP, not just deleted) and
local state:

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://adorsys-gis.github.io/lightbridge-governance/uninstall.sh | sh
```

⚠️ Run `governance-auth configure` with no `--gateway-url`/`--otel-endpoint` **before**
uninstalling if you want the keys it wrote into Claude Code, Codex and VS Code retracted
automatically. Afterwards the binary that owns them is gone and the uninstaller can only
tell you which keys to delete by hand.

## Quickstart

```bash
governance-auth login \
  --issuer https://auth.example/realms/platform \
  --client-id governance-auth-cli \
  --gateway-url https://api.example \
  --otel-endpoint https://otel.example \
  --otel-token "$OTLP_INGEST_TOKEN"
```

That single command authenticates, caches the session, writes the inference and telemetry
wiring into whichever of the three clients are installed, and schedules the Copilot spool
drain. Everything after it is automatic: Claude Code and Codex call `token` and
`otel headers` themselves, and a systemd user timer (or launchd agent) calls `copilot push`
every five minutes.

⚠️ Restart VS Code afterwards — Copilot reads its telemetry settings at window start.

Put `--issuer`/`--client-id` in a config file or in `GOVERNANCE_AUTH_*` env vars so later
commands don't need them — see [`configuration.md`](./configuration.md).

## The three properties worth knowing before you change anything

**`token` fails closed.** No valid session, a rejected refresh, or a failed token exchange
all produce *nothing on stdout* and a non-zero exit. There is no branch that falls back to
a weaker credential. This matters more than it looks: per
[`ai-client-flows.md`](../integrations/ai-client-flows.md), Codex responds to a failed
helper by proceeding **unauthenticated** rather than stopping, so anything this binary
emits on a bad day is a credential someone will actually send.

**Only `login` is interactive.** `token` is invoked by a background process on a timer; it
must never launch a browser or block on a prompt. And since #141, `login` itself doesn't
open a browser either unless you ask it to — see `--open-browser`.

**Secrets are structural, not habitual.** Credentials are wrapped in `Redacted<T>`, whose
`Debug`/`Display` print `<redacted>`, so a stray `{:?}` can't leak one. Files that can
carry a token are written at mode `0600` tmp-then-rename, and a config file that inlines a
secret while being group- or world-readable is *refused*, not loaded.
