# governance-auth

The OAuth2 credential helper a developer runs on their laptop. It points Claude Code, Codex
and VS Code Copilot at this org's AI gateway, and their telemetry at this org's collector.

```text
governance-auth login   ->  OIDC authorization code + PKCE  ->  session at 0600
governance-auth token   ->  a valid access token on stdout  ->  apiKeyHelper / auth.command
```

The rest of the tree is scoped, one scope per thing it acts on:

```text
governance-auth refresh                 force a new token now, even if the cached one is fresh
governance-auth status                  session, telemetry wiring and drain health
governance-auth configure               (re-)write the three tools' config and the drain schedule
governance-auth logout                  revoke at the issuer, then clear locally
governance-auth otel headers            OTLP headers as JSON, for `otelHeadersHelper`
governance-auth copilot push            drain Copilot's spool to the collector
governance-auth self update             replace this binary with the latest release
```

`token` is deliberately NOT scoped and never will be: it is the one command name embedded in
a file this binary cannot rewrite (the VS Code extension's own argv). See `src/cli`'s module
doc for the rule that decided the rest.

📖 **Full reference: [`docs/governance-auth/`](../../docs/governance-auth/README.md)** —
commands, the four-source configuration matrix, every file it writes and which keys it owns
inside each, token exchange, and troubleshooting. This README is the orientation; that is the
manual.

Why a credential helper at all, rather than static per-developer API keys:
[ADR-0010](../../docs/adr/0010-governance-auth-keycloak-oauth2-credential-helper.md).
Packaging, distribution and the config-precedence decision:
[ADR-0012](../../docs/adr/0012-governance-auth-packaging-and-distribution.md).

## This binary authorizes nobody

It is a pure OAuth2 *client*. The authorization server validates its own tokens and the
gateway validates the JWTs it accepts; nothing here makes an access decision. What it does
hold is real user credentials — authorization codes, PKCE verifiers, access and refresh
tokens — which is what drives the three properties below.

## The three properties to preserve

**Fails closed.** No session, a rejected refresh, or a failed token exchange each produce
nothing on stdout and a non-zero exit. There is no branch that falls back to a weaker
credential. This matters because Codex responds to a failed helper by proceeding
**unauthenticated** rather than stopping (see
[`ai-client-flows.md`](../../docs/integrations/ai-client-flows.md)) — so anything emitted on a
bad day is a credential that will actually be sent.

**HTTPS or loopback, checked in three independent places.** At CLI-parse time; in discovery,
which origin-pins every endpoint the discovery document returns; and in the redirect policy,
which re-checks every hop so a `3xx` can't walk an HTTPS request down to plaintext. The
loopback carve-out is structural and deliberately *not* a configurable "allow insecure" flag —
that would be a test double reachable from a production path.

**Secrets are typed, not remembered.** Credentials live in `Redacted<T>`, whose `Debug`/
`Display` print `<redacted>`, so a stray `{:?}` can't leak one. Files that can carry a token
are `0600`, written tmp-then-rename. A config file that inlines a secret while being group- or
world-readable is refused, not loaded.

## Layout

| Module | What it does |
|---|---|
| [`config.rs`](src/config.rs) | The clap surface and the five-layer precedence resolve. Read its module doc before adding an option. |
| [`config_file.rs`](src/config_file.rs) | The two file layers, and the secret-permission rules. |
| [`oauth/`](src/oauth/) | `discovery`, `authcode`, `pkce`, `device`, `token_endpoint`, `exchange`, and `mod.rs`'s command orchestration. |
| [`cache.rs`](src/cache.rs) | The session store, its locking, and the cache→state migration. |
| [`otel.rs`](src/otel.rs) | Every write into another tool's config file. |
| [`security.rs`](src/security.rs) | The one transport-security predicate, applied at three points. |
| [`update.rs`](src/update.rs) | Self-update, and the version constant that stops it looping. |
| [`redacted.rs`](src/redacted.rs) | The secret newtype. |

## Traps this crate has already paid for

- ⚠️ **Never give a config-backed option a clap `default_value`.** It fires before either
  config-file layer is consulted, so layers 3 and 4 silently stop existing for that option.
  Booleans have the same trap wearing `ArgAction::SetTrue`. Only tests that go through *real*
  clap parsing can catch it — see the maintainer section of
  [`configuration.md`](../../docs/governance-auth/configuration.md).
- ⚠️ **The path written into Codex's config must be absolute.** Codex spawns it without a
  shell, so a bare name fails with `os error 2` and the provider falls back to
  unauthenticated. Claude Code resolves the bare name, so this trap is invisible on one of the
  two clients.
- ⚠️ **`otel.exporter` in Codex's config is a tagged enum**, not a string. The wrong shape is
  valid TOML that Codex rejects at load — and Codex refuses to start rather than degrading, so
  getting it wrong bricks the tool.
- ⚠️ **A released binary must report its release tag**, injected via
  `GOVERNANCE_AUTH_RELEASE_VERSION`. When it reported the workspace version instead,
  `self update` updated, still saw the old version, and updated again — forever.
- ⚠️ **The `asset_name()` matrix and the release workflow's build matrix move together.** The
  musl/gnu split is load-bearing: a musl binary that fetched the gnu asset would update itself
  onto a build that can't start on its own distro.
