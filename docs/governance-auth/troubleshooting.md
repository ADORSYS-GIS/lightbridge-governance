# Troubleshooting

Failures this has actually produced, with the mechanism rather than the verdict. Errors all
go to **stderr** — if you are seeing nothing at all, check that you aren't discarding it.

`RUST_LOG=debug governance-auth …` turns on tracing, also on stderr.

---

## Authentication

**`no cached session for this issuer/client; run 'governance-auth login' first`**

Exactly what it says: the store was empty (first run, or `logout`), and `token` correctly
refused to launch an interactive browser from an unattended re-invoke.

Note *"for this issuer/client"* — the session filename is a hash of both. Logging in with one
`--issuer`/`--client-id` pair and then running `token` with a different pair, or with the flags
absent and a config file supplying different values, looks identical to never having logged
in. `governance-auth status` with the same flags is the check.

**`token` fails after previously working**

The refresh token was rejected (`invalid_grant`): the session was revoked, it expired past its
offline-session lifetime, or the client registration changed. Run `login` again. This binary
never retries with a stale credential.

**`cached session has no refresh token`**

The login didn't request `offline_access`, or the server didn't grant it. Check `--scopes` —
the compiled default includes it, but any explicit value replaces the default wholesale rather
than adding to it.

**I logged out, but requests still succeed**

Expected, for up to 300 seconds. `logout` revokes the session at the issuer — verifiable, in
that `/userinfo` starts returning 401 immediately — but the gateway validates tokens by
signature and `exp` without introspecting, so an already-issued access token keeps working
until it expires on its own. See *"Logout is not immediate cutoff"* in
[`commands.md`](./commands.md). If you are logging out because a credential leaked, that
window is real: act at the identity provider, not just here.

**`login` prints a URL and appears to hang**

That is the design. It is waiting for you to visit the URL; it does not open a browser unless
`--open-browser` is set. On a box with no browser at all, use `--device-code` instead.

**`every loopback callback port is already in use: 17452, 17453, …`**

Exactly what it says, and it is refusing on purpose. The browser flow can only use those five
ports because the authorization server matches redirect URIs exactly and only those are
registered, so there is no other port it could legally fall back to. Free one
(`ss -ltnp | grep 174` shows the holder), or use `--device-code`, which needs no local
listener at all.

It refuses rather than quietly taking another port because a fallback would bind fine and then
fail at `/authorize` with `invalid redirect_uri` — moving the error away from its cause and
making a local port collision look like a broken server or a bad registration.

**`400 invalid redirect_uri` from the authorization server**

The port the CLI bound is not registered on the client. These two lists are a contract and
have drifted:

- `CALLBACK_PORTS` in `app/governance-auth/src/oauth/callback_port.rs`
- `redirect_uris` on `governance-auth-cli` in `ai-helm-values`
  `environments/prod/values/lightbridge-app.yaml`

Matching is byte-for-byte — no normalisation, no port exemption — so `http` vs `https`,
`127.0.0.1` vs `localhost`, or a trailing slash all fail the same way. Fix by adding the port
to the registration **first**, then to the binary: a registration the CLI does not use is
inert, an unregistered CLI port is a hard failure. Background:
[ADR-0015](../adr/0015-pin-the-loopback-callback-to-a-registered-port-block.md).

**The browser tab says "Sign-in failed" but I did sign in**

Trust the terminal, and read the error there. The tab reports the *outcome*, which includes
the `state` check and the token exchange that happen after the redirect — so a completed
Keycloak sign-in can still end in a failed login (a mismatched `state`, or a code the token
endpoint rejects).

This was previously the other way round and worse: the page said "You're signed in" as soon as
a `code` parameter was present, before either check, so a forged callback or a rejected code
produced a success page contradicting the terminal. Fixed in
[#204](https://github.com/ADORSYS-GIS/lightbridge-governance/pull/204).

---

## Configuration

**`--issuer (or GOVERNANCE_AUTH_ISSUER, or 'issuer' in a config file) is required`**

None of the four layers supplied it. Note that an env var set in *your* shell is not
necessarily set in the environment Claude Code or Codex spawns the helper in — that is exactly
why the flags are accepted after the subcommand, so the helper command string can carry them
explicitly.

**`error: unexpected argument '--issuer' found`**

You are on a build predating the `global = true` fix. Options now work on either side of the
subcommand; before that they had to precede it, which broke the one string that matters most
(`governance-auth token --issuer …`, as written in both vendors' docs).

**A config file key has no effect**

Three candidates, in order of likelihood: it is being overridden by a higher layer (a flag or
an env var); the file is at a path that isn't consulted (`$XDG_CONFIG_HOME` shifts the
per-user path); or the key is misspelled — in which case you would have got a hard parse error,
because unknown keys are rejected rather than ignored. If a *maintainer* is asking why a new
option ignores its config file, read the `default_value` trap in
[`configuration.md`](./configuration.md).

**`<path> sets both 'otel_token' and 'otel_token_file'; keep only one`**

Deliberate. Silently preferring one would be a misconfiguration nobody would ever notice.

**A config file inlining `otel_token` is refused**

It is group- or world-readable. Either `chmod 600` it, or move the secret out to a
`otel_token_file` pointing at MDM/ESO-managed material — which is the right answer for the
machine-wide file, since `/etc/governance-auth/config.toml` is reasonably world-readable.

**An HTTPS complaint about a URL you are sure is fine**

Every URL option must be `https://` or loopback. A config-file value gets the same check as a
flag, applied at resolve time. This is not overridable and there is deliberately no "allow
insecure" escape hatch.

---

## Locking and disk

**`token` hangs for ~5 minutes, then works**

An empty lock file, left by a crash between create and write, was being read as
"undeterminable" and waiting out the full 300-second stale timeout on every invocation. Fixed —
an empty lock is now treated as confirmed dead, and a writer that fails to record its PID
removes the file. If you see this on an old build, delete `<state dir>/governance-auth/*.lock`
and update.

**Writes fail, or a session vanishes**

Check free disk. A full filesystem produces `ENOSPC` on the tmp-then-rename write, and the
symptom presents as authentication failures rather than as a disk error, because the layer
that reports it is the one asking for a token.

**A session disappears after a "clean up disk space" run**

You are on a build old enough to store the session under `~/.cache` / `~/Library/Caches`.
Those locations are purgeable by the OS and by every cleanup tool. Update — the session moved
to state, and one migration on read moves an existing session with it.

---

## Token exchange

**`token exchange failed; refusing to fall back to the un-exchanged upstream token`**

Working as designed. The underlying cause is in the context chain below that line. The usual
one is audience: the subject token must carry your `--exchange-client-id` in its `aud`, and
the exchange server checks the audience twice against two different values. See
[`token-exchange.md`](./token-exchange.md).

**`missing field 'authorization_endpoint'`**

An old build refusing a discovery document that legitimately omits the field. Fixed in
[#145](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/145); update.

(This entry used to add "which `lightbridge-authz` does, having no `/authorize` route".
That is no longer true — `authz-idp` serves `/authorize` and advertises
`authorization_endpoint`. The fix still stands for issuers that genuinely omit it.)

**Exchange succeeds but the gateway returns 403**

Not this binary. Look at what the gateway's authorization rules make of the minted token's
claims — introspection returning `{"active": false}` for an ephemeral `api_key_id` produced
exactly this, and it was resolved by rescoping the rules to the issuer rather than by changing
anything here.

---

## Client wiring

**Another AI CLI's telemetry started 401-ing against the wrong collector**

Any build before the fix exported the generic OpenTelemetry variables —
`OTEL_EXPORTER_OTLP_ENDPOINT`, `_PROTOCOL`, `_HEADERS`, `OTEL_METRICS_EXPORTER`,
`OTEL_LOGS_EXPORTER`, `OTEL_RESOURCE_ATTRIBUTES` — into
`~/.config/governance-auth/otel.env`, which every shell sources. Those are machine-global and
OpenTelemetry SDKs read them *ahead of* their own configured default, so any other OTLP
exporter on the laptop silently retargeted at this binary's collector and got `401` there,
because each collector's OIDC gate accepts one audience only. Observed on OpenCode, whose
plugin resolves `env.OTEL_EXPORTER_OTLP_ENDPOINT || opts.endpoint`.

Fix: **`governance-auth configure`** after updating. `self update` replaces the binary but
touches no config — the stale exports live in `otel.env`/`otel.fish`, and only a `configure`
run rewrites them. Confirm with:

```bash
grep OTEL_ ~/.config/governance-auth/otel.env ~/.config/governance-auth/otel.fish
```

No output is the fixed state. Then open a new shell (or `unset` the variables in the current
one — sourcing the new file cannot remove what the old one already exported).

**Codex proceeds unauthenticated**

Almost always the helper path. Codex spawns the auth command **directly, not through a
shell**, so it does not inherit the login shell's `PATH`; a bare `governance-auth` fails with
`No such file or directory (os error 2)` and the provider silently falls back to
unauthenticated. The written config uses an absolute path for this reason. If you hand-edited
it, put the absolute path back.

This is also why the same mistake is invisible on Claude Code, which resolves a bare name
because it goes through a shell.

**Codex refuses to start after `configure`**

Codex does not degrade on a config it can't load — it exits. The historical cause is the
`otel.exporter` shape: it is a tagged enum (`[otel.exporter.otlp-http]`), and
`exporter = "otlp-http"` with settings in a sibling table is valid TOML that Codex rejects at
load time. The current writer emits the correct shape; a hand-edit may not.

**Codex talks to the gateway but nothing works**

⚠️ This entry previously said the provider block was **inert** because `/v1/responses` 404d.
That is no longer the whole picture: as of 2026-08-31 the gateway **routes and auth-gates**
`/v1/responses` (it returns `401`, where a genuinely absent path returns `404`), and
`governance-auth` now makes this provider the default via `model_provider`.

What is **not** verified is whether the *upstream* serves it. The `401` is returned before
upstream is reached, so an unauthenticated probe cannot tell; the previously-recorded failure
was a well-formed body 404ing **from upstream**. If every Codex call errors, check that first,
and `--config model_provider=openai` reverts for a single run.

Historical detail, still useful: codex-cli requires `wire_api = "responses"` and this gateway
implements `/v1/chat/completions`. The auth
wiring is correct and tested; the endpoint is the missing piece —
[#144](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/144).

**Claude Code: a 403 with an HTML body, mentioning a WAF**

Not this binary. Claude Code's prompts contain XML-ish tags and source code that can trip
body-inspection WAF rules on `/v1/messages`. That path needs an exemption on the gateway side.

**Claude Code: "not a model this version recognizes"**

Expected, and not fixed by gateway model discovery — that warning is about the assumed 200k
context window, and only `modelOverrides` or `CLAUDE_CODE_MAX_CONTEXT_TOKENS` addresses it.
Neither is written here, because it would mean hard-coding each gateway model's real window
into this binary. See [#151](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/151).

**Claude Code ignores `apiKeyHelper`**

`apiKeyHelper` cannot coexist with `forceLoginMethod`/`forceLoginOrgUUID` managed settings. If
your org pushes those, this developer needs an explicit carve-out.

**No Copilot telemetry is arriving at the collector**

Copilot does not export over the network at all: `configure` sets
`github.copilot.chat.otel.exporterType` to `file` and `governance-auth copilot push` ships the
spool on a timer. Check the two `status` rows in order, because they answer different
questions:

- **`copilot drain`** — is the schedule installed and running? `not scheduled` means run
  `governance-auth configure`; `installed, not running` prints the command that starts it;
  `installed` (yellow) means the scheduler could not be asked, which is normal in a container.
- **`copilot spool`** — is the drain keeping what it reads? `not enabled` means Copilot has
  written nothing yet: **restart VS Code** (it reads these settings at window start) and send
  one chat turn.

⚠️ Upgrading from a build that wrote `exporterType: "otlp-http"` needs one `configure` to
retract it — and a **VS Code restart** after that, since the old exporter is still live in the
running window.

⚠️ `configure` cannot edit a JSONC `settings.json` (see below), so on those machines the
exporter is still whatever it was. The error names the exact keys to paste.

**`<path> is not plain JSON`**

Your `settings.json` uses JSONC comments or trailing commas. This binary declines to edit it
rather than round-tripping it and deleting your comments permanently; the exact settings to
paste are printed with the error.

---

## Self update

**`no asset for your platform`**

`asset_name()` and the release workflow's build matrix have drifted, or you are on a platform
with no published asset. The matrix is listed in [`commands.md`](./commands.md).

**`No published release found for this repository`**

An ordinary state, not a failure — `releases/latest` 404s when a repo has published none.

**`self update` updates every time you run it**

A binary reporting a version older than the one it just installed asks again, forever. This is
fixed by injecting the release tag at build time, and pinned by two tests — but if you see it,
check whether the release workflow still sets `GOVERNANCE_AUTH_RELEASE_VERSION`. Losing that
reintroduces the loop on real releases only, never locally.

**`self update` fails asking for `--issuer`**

An old build resolving the OAuth config before dispatching. `self update` talks only to the
GitHub releases API and needs none of it — which matters precisely because the machine most
likely to be updating is the one with no config yet.

---

## Upgrading across the command rename

The flat commands (`copilot-push`, `otel-headers`, `self-update`) were reorganised into scoped
subcommands (`copilot push`, `otel headers`, `self update`). This is a hard cutover — the old
names are gone, there is no alias — so a binary that upgrades in place can leave stale
invocations lying around in config it wrote under the old scheme.

**What keeps working, unaffected:**

- `~/.claude/settings.json`'s `apiKeyHelper` and `~/.codex/config.toml`'s `auth.command` both
  run `token`. `token` did not move.
- The VS Code extension spawns `token` directly. Also unaffected — `configure` cannot reach
  into the extension's own compiled invocation, which is the whole reason `token` was frozen
  rather than folded into a scope.

**What breaks silently on the next wake, not on upgrade itself:**

- `otelHeadersHelper` in `~/.claude/settings.json` still runs `otel-headers`, written by a
  pre-upgrade `configure` or `login`. That name no longer exists, so Claude Code's next
  telemetry-header refresh fails and it stops exporting telemetry — quietly, because the
  helper's stderr is swallowed by the caller.
- The systemd unit and the launchd plist installed by a pre-upgrade `configure` still invoke
  `copilot-push` as a single argument. Every wake now fails with clap's unrecognised-subcommand
  error, the checkpoint stops advancing, and the spool grows unbounded.

Both are fixed the same way: **`governance-auth configure`**. It rewrites the helper command
and regenerates the unit/plist with the new two-word invocation.

`governance-auth status` detects both conditions rather than leaving them to be found by
accident:

- The telemetry row goes red with **wiring was written by an older version**.
- The **copilot drain** row goes red with **out of date**.

So a developer who ran only `self update` — updating the binary but never re-running
`configure` — is told to fix it, rather than discovering it only when Copilot Chat's spool
stops draining or Claude Code's telemetry goes silent.

**Typing the old command by hand:**

```
$ governance-auth self-update
error: unrecognized subcommand 'self-update'

tip: a similar subcommand exists: 'self'
```

clap's suggestion names the new top-level scope, not the full new command. The fix is
`governance-auth self update`.
