# Commands

Every subcommand, what it does, and — for the two that a machine parses — exactly what
lands on stdout.

**The stdout/stderr split is a contract, not a style choice.** `token` and `otel-headers`
are invoked by Claude Code and Codex, which read stdout and parse it. So *all* UX, prompts,
warnings and errors go to **stderr**, and stdout carries the credential and nothing else,
ever. Anything extra on stdout breaks the caller's parse.

Options are global: they may be written before or after the subcommand
(`governance-auth --issuer … token` and `governance-auth token --issuer …` are both
accepted). That is deliberate — the helper hooks embed a single command string, and both
vendors' docs show the subcommand written first.

---

## `login`

Interactive first-time authentication. Runs OIDC discovery against `--issuer`, performs the
authorization-code flow with PKCE against a loopback redirect, caches the resulting session,
then applies the client configuration (the same work `configure` does).

```bash
governance-auth login --issuer https://auth.example/realms/platform --client-id governance-auth-cli
```

Prints the authorize URL to stderr and waits. **It does not open your browser** unless
`--open-browser` is set (or the equivalent env var / config key) — see
[issue #141](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/141). An SSH
session, a container, a CI runner and a VM all inherit a `DISPLAY`/`xdg-open` that either
fails or hijacks an unrelated desktop, and the URL is printed either way, so auto-opening
was wrong more often than it was right.

On success: `Logged in; session cached, expires in <n>s.`

**PKCE is unconditional.** `code_challenge_method=S256` is always sent and there is
deliberately no flag, env var or config key to turn it off — this is a public client with no
secret, so the verifier is the only thing binding the authorization code to this process.
`tests/pkce_authcode.rs` pins it.

### `login --device-code`

The device-authorization flow instead of the loopback flow. Prints a verification URL and a
user code to stderr and polls until you complete it on another device. This is the flow for
a headless box — an SSH session or a cloud dev workspace with no local browser at all.

`--open-browser` has no effect here: there is nothing to open, the verification URL is
meant to be visited elsewhere.

### Telemetry wiring on `login`

`login` calls the same writers `configure` does, but treats their failure as a **warning**
(`warning: could not configure telemetry: …`) rather than an error — a developer who
authenticated successfully should not be told the whole command failed because one dotfile
was unwritable. `configure` run on its own treats the identical failure as an error,
because there the wiring is the entire point.

---

## `token`

Prints a currently-valid access token to stdout. **This is the command to wire into
`apiKeyHelper` and `[model_providers.*.auth] command`.**

```bash
governance-auth token
```

What it does:

1. Acquires the per-(issuer, client-id) file lock, so two clients invoking it at the same
   moment on a cold store don't both try to refresh.
2. Loads the cached session. Absent → `no cached session for this issuer/client; run
   'governance-auth login' first`, non-zero exit, nothing on stdout.
3. If the session is inside the expiry skew, refreshes it and stores the result. A rejected
   refresh is a hard failure — it never retries with a stale credential and never emits one.
4. If token exchange is enabled, exchanges the token (see
   [`token-exchange.md`](./token-exchange.md)). A failed exchange is a hard failure; it
   never falls back to the un-exchanged upstream token.
5. Prints the token. One line, no trailing commentary.

Non-interactive by construction: it will not launch a browser, ever. If the session is
unusable the answer is a non-zero exit, and the fix is a human running `login`.

---

## `otel-headers`

The same token, wrapped in the JSON object Claude Code's `otelHeadersHelper` hook requires:

```json
{"Authorization": "Bearer …"}
```

Same refresh path, same lock, same fail-closed contract as `token` — this *is* `token`, in
the shape the hook expects. That is what makes telemetry auth self-renewing instead of
depending on someone rotating a long-lived key by hand, and it is why a 300-second access
token is the right credential here where it is the wrong one for the static
`OTEL_EXPORTER_OTLP_HEADERS` variable (see [`files.md`](./files.md)).

---

## `configure`

Re-applies the client configuration for an existing session, without re-running the
interactive login. Run this after installing Claude Code or Codex for the first time, or
when the collector endpoint or the OTLP ingest token changed.

```bash
governance-auth configure --gateway-url https://api.example --otel-endpoint https://otel.example
```

Requires a cached session (it reads the access token to derive the identity resource
attributes). Requires at least one of `--gateway-url` or `--otel-endpoint`: with neither,
there is nothing to write and it fails loudly rather than reporting success for a no-op.

Reports each file: `Configured: <path>` or `Skipped: <dir> not present.` A tool whose
config directory doesn't exist is skipped, never created — most developers have one of the
three, not all of them.

Which files, and which keys inside them, is [`files.md`](./files.md).

---

## `status`

```bash
governance-auth status
```

Prints to **stderr**, one line:

- `session cached, fresh, expires in <n>s`
- `session cached, needs refresh, expires in <n>s`
- `no cached session`

Exit status is 0 in all three cases — this reports state, it does not assert it. A real
request from Claude Code or Codex is the actual proof that onboarding worked; `status`
rules out half the failure modes before you go looking at the tools.

---

## `logout`

Revokes the refresh token at the authorization server, **then** clears local state.

The order matters. Deleting the local file alone — what this used to do — leaves the
refresh token valid at the server until its offline-session lifetime expires, while telling
the developer `session cleared`. A logout that reports success and leaves a usable
credential live is worse than one that fails loudly, because nobody goes back to check.

### ⚠️ Logout is not immediate cutoff

**An access token already issued keeps working until its own `exp` — up to 300 seconds after
you log out.** `logout` stops *new* tokens being minted; it does not reach out and kill the
one already in flight.

Measured, both against the live deployment at the same moment, with the same token:

| Request | Result |
|---|---|
| `GET /userinfo` at the issuer | **401** — the session really is revoked |
| `POST /anthropic/v1/messages` at the gateway | **200** |
| `POST /v1/chat/completions` at the gateway | **200** |

The mechanism is that the gateway validates the JWT by **signature and `exp`**. It does not
introspect, and it does not check session liveness — so a revoked session is invisible to it
until the token expires on its own.

That is a deliberate trade, not an oversight. Per-request introspection at the Authorino step
is exactly what was disabled in production on 2026-07-02: the ext_authz timeout is shorter
than the lookup takes (see the estate's `AGENTS.md` — *never add a database lookup to the
Authorino step itself*). The access-token lifetime **is** the mitigation, which is the real
argument for keeping it at 300s and for keeping
`otel_headers_debounce_ms`/`CLAUDE_CODE_API_KEY_HELPER_TTL_MS` underneath it.

What follows for an operator:

- For a routine logout, none of this matters.
- For a **suspected compromise**, `logout` alone leaves a window of up to the remaining token
  lifetime. If the credential is broadly scoped, treat the window as real and act at the
  identity provider — end the user's sessions, and rotate or disable the account — rather than
  assuming the CLI's `session cleared` ended access.

The size of that window is the whole reason the scope of the token matters. A CLI credential
carrying realm-administration or impersonation rights makes 300 seconds a meaningful blast
radius; one scoped to inference does not.

---

## `self-update`

Replaces this binary with the latest GitHub release for this platform, from
`ADORSYS-GIS/lightbridge-governance`.

```bash
governance-auth self-update
governance-auth self-update --check   # report only, change nothing
```

Unlike every other subcommand, this one **does not resolve the OAuth config** — it talks
only to the GitHub releases API. Resolving first used to make `self-update` fail with
`--issuer … is required` on a machine that had no config yet, which is precisely the
machine most likely to be updating.

### Trust model, stated plainly

The download is checked against the release's own `.sha256`, fetched from the same release.
That proves the asset wasn't **corrupted or truncated in transit**. It does *not* prove the
release is authentic: anyone who could replace the asset could replace the checksum beside
it. TLS to `api.github.com` plus GitHub's account controls are what establish authenticity
today. This is weaker than the container images, which are cosign-signed. Signing these
binaries the same way is the right next step; until then the word used here is
**checksummed**, never "verified".

### Assets

Raw binaries, one per platform — not tarballs. `asset_name()` in `update.rs` and the
release workflow's build matrix must stay in lockstep; a mismatch shows up as *"no asset for
your platform"*.

| Platform | Asset |
|---|---|
| Linux x86_64 (musl, static) | `governance-auth-x86_64-unknown-linux-musl` |
| Linux aarch64 (musl, static) | `governance-auth-aarch64-unknown-linux-musl` |
| Linux x86_64 (glibc) | `governance-auth-x86_64-unknown-linux-gnu` |
| Linux aarch64 (glibc) | `governance-auth-aarch64-unknown-linux-gnu` |
| macOS x86_64 | `governance-auth-x86_64-apple-darwin` |
| macOS arm64 | `governance-auth-aarch64-apple-darwin` |

⚠️ The musl/gnu split is load-bearing, not cosmetic. A musl-built binary that asked for the
`-gnu` asset would update itself onto a build that cannot start on the distro it is running
on — the glibc floor is the entire reason the musl assets exist.

### `--version` and the self-update loop

A released binary reports its **release tag**, injected at build time via
`GOVERNANCE_AUTH_RELEASE_VERSION`; a locally-built one falls back to `CARGO_PKG_VERSION`.
Both `--version` and the version `self-update` compares against read the same constant.

This is not a detail. When they disagreed, a binary that had just updated still reported the
old version, decided it was out of date, and updated again — forever. Two tests pin it: one
proves a version-misreporting binary never terminates, and one asserts the release workflow
still sets that environment variable, because losing it would silently reintroduce the loop
on a real release only.
