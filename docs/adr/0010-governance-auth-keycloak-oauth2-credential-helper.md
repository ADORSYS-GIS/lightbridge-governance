# ADR-0010: `governance-auth`, a Keycloak OAuth2 credential-helper binary for Claude Code and Codex

- Status: Proposed
- Date: 2026-08-04
- Decision owners: @stephane-segning

## Context

Developers want Claude Code and OpenAI Codex pointed at this org's own gateway
(`api.ai.camer.digital`) instead of, or alongside, each vendor's hosted backend. Both
tools support this through a credential-helper hook -- an external executable the tool
re-invokes to obtain a fresh bearer token -- rather than a static key pasted into config:
Claude Code's `apiKeyHelper` (output cached 5 minutes, re-run on HTTP 401) and Codex's
`[model_providers.<id>.auth] command` (proactively re-invoked on `refresh_interval_ms`).

This is tracked upstream as two open, unowned tickets in `ai-helm`:
[#680](https://github.com/ADORSYS-GIS/ai-helm/issues/680) (Claude Code) and
[#679](https://github.com/ADORSYS-GIS/ai-helm/issues/679) (Codex). Both ask for real
OAuth2 against the org's existing Keycloak issuer -- "the same way our other agent
clients do" -- not another manually-issued static key. `ai-helm`'s `keycloak-baseline`
realm already sets `accessTokenLifespan: 300` (5 minutes), which lines up with Claude
Code's helper cache window.

`ai-helm` ADR-0007 designed almost exactly this (`kc-token`, a Go CLI, device-code
default) but was superseded by ADR-0009 -- narrowly, because CI should use Keycloak
token exchange instead of a static secret, and *humans* were assumed to be served by the
existing static-key self-service portal (`self-service.ai.camer.digital`). That reasoning
doesn't cover this case: the `apiKeyHelper`/`auth.command` hook contract requires an
executable that prints a fresh token on every invocation, which a portal-copied static
key can't satisfy without manual rotation. This is not reopening ADR-0007/0009 -- it
fills a gap neither anticipated.

The gateway already authenticates via Keycloak-issued JWTs (Authorino validates them);
no new server-side authorization component is required.

## Decision

Build **`governance-auth`**, a new Rust binary in this workspace (`app/governance-auth`),
that is a pure OAuth2 *client* -- it mints, caches and refreshes a Keycloak access token
and prints it, nothing more:

- `governance-auth login` -- interactive first-time setup. Default flow: Authorization
  Code + PKCE (S256) via a localhost loopback redirect (RFC 8252), opening the system
  browser. `--device-code` (RFC 8628) is the fallback for headless sessions (SSH, cloud
  dev boxes) with no local browser -- the flow the org's own (superseded-for-different-
  reasons) `kc-token` CLI defaulted to.
- `governance-auth token` -- the credential-helper entrypoint wired into `apiKeyHelper` /
  `auth.command`. Prints a currently-valid access token to stdout and nothing else,
  transparently refreshing via the cached refresh token when near expiry. **Fails
  closed**: a missing session or a rejected refresh is a non-zero exit and empty stdout,
  never a stale or fabricated token, and it never launches an interactive browser from an
  unattended re-invoke.
- `governance-auth status` / `logout` -- inspect / clear the cached session.

The session cache lives at `~/.cache/governance-auth/<sha256(issuer+client_id)>.json`
(`~/Library/Caches` on macOS), mode `0600`, written tmp-then-rename, guarded by a
coarse file lock so Claude Code and Codex invoking the helper concurrently on a cold
cache can't race a rotating refresh token or open two browser tabs.

This does **not** touch `governance-core::credential`/`Integration` -- that is a
separate, unrelated credential system for OTLP-ingest auth (epic #30's push connector),
a different endpoint and a different concern. `governance-auth` is distributed
independently of the server image (cross-compiled binaries via GitHub Releases), not
bundled into the `lightbridge-governance`/`governance-ctl` Docker image.

## Consequences

**Positive**
- Closes ai-helm #680/#679 with the mechanism each vendor actually documents, not a
  workaround -- no static API key checked into a dotfile, real server-side revocation via
  Keycloak, refresh-token rotation instead of re-login every 5 minutes.
- No new server-side component: Keycloak is already the authorization server, the
  gateway already validates its JWTs. This is client-only work.
- One binary serves both products; the credential-helper contract (print token to
  stdout, everything else to stderr) is identical for `apiKeyHelper` and
  `auth.command`.

**Negative**
- Requires a new Keycloak client registration in `ai-helm`
  (`charts/keycloak-baseline`) -- a public client with PKCE enforced, loopback redirect
  URIs, and device-flow capability. This ADR does not grant that; it is a coordinated,
  separate change in that repo.
- Distribution to developer laptops (installer, dotfiles/Coder workspace template
  wiring) is not solved by this ADR -- tracked as follow-up work, mirroring epic #30's
  own "Coder workspace template first, dotfiles second" rollout plan.

**Neutral / follow-ups**
- Whether the gateway's Authorino AuthConfig validates an `aud` claim is unconfirmed;
  `governance-auth` exposes `--audience`/`GOVERNANCE_AUTH_AUDIENCE` to request one if so,
  matching `kc-token`'s own `--audience` flag.
- ai-helm #680 and #679 each still have their own blocking spike (AIEG's
  `/anthropic/v1/messages` routing to a `schema: OpenAI` backend; Codex's `/v1/responses`
  SSE compatibility) -- those gate whether the *inference* path works once authenticated,
  not whether this binary correctly mints a token. Independent concerns, sequenced in
  parallel.

## Alternatives considered

- **A shell/Python script instead of a compiled binary** -- rejected: this workspace is
  Rust/cratestack-only by convention (AGENTS.md), and a script can't reuse the PKCE/
  crypto primitives (`sha2`, `base64`, `getrandom`) already vetted and pinned here.
- **Point developers at the existing static-key self-service portal
  (`self-service.ai.camer.digital`)** -- the path ADR-0007/0009 implicitly leaves open for
  humans. Rejected for this use case specifically: the credential-helper hook needs an
  executable to invoke on every check, not a value to copy once: a portal key still
  requires manual rotation and carries no per-developer revocation story as clean as a
  Keycloak refresh token.
- **A subcommand on the existing `governance-ctl` binary** -- rejected: `governance-ctl`
  is built into the server image and runs as the in-cluster `copilot-sync` CronJob; an
  interactive, browser-opening developer login flow is a different lifecycle and
  distribution story entirely, and mixing them muddies both.
- **Device-code flow as the default** (matching the superseded `kc-token`) -- rejected as
  the *default*, kept as the fallback: developers running Claude Code/Codex on a laptop
  with a browser get better UX from a loopback redirect (no code to copy/type), matching
  how `claude mcp login`/`codex mcp login` themselves already behave.

## Related

- ai-helm [#680](https://github.com/ADORSYS-GIS/ai-helm/issues/680),
  [#679](https://github.com/ADORSYS-GIS/ai-helm/issues/679)
- ai-helm ADR-0007 (`kc-token`, superseded), ADR-0009 (token exchange for CI)
- Runbook: `docs/runbooks/onboard-a-developer-ai-client.md`
- Epic #30 / RFC-0003 (client-side OTLP ingestion) -- adjacent, not a dependency
