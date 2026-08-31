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

**The callback port is fixed, and it is a cross-repo contract.** `login` binds the first free
port of `17452-17456` and builds `http://127.0.0.1:<port>/callback`. All five are registered
as `redirect_uris` on the `governance-auth-cli` client; the authorization server matches
redirect URIs by exact string equality, so a port that is not registered is refused with
`400 invalid redirect_uri`.

There is no flag to change the port, deliberately: changing it unilaterally would break login
rather than customise it. Both sides move together — `CALLBACK_PORTS` in
`app/governance-auth/src/oauth/callback_port.rs`, and `redirect_uris` in `ai-helm-values`
`environments/prod/values/lightbridge-app.yaml` — registration first.

⚠️ This is a **workaround for a server-side spec violation**, not a design choice. RFC 8252
§7.3 says an authorization server **MUST** accept any port for a loopback redirect, precisely
so a native app can take an ephemeral one from the OS; `authkestra-op` does not implement that
exemption ([upstream #291](https://github.com/marcjazz/authkestra/issues/291)). When it lands,
the CLI goes back to an ephemeral port and the extra registrations are deleted. See
[ADR-0015](../adr/0015-pin-the-loopback-callback-to-a-registered-port-block.md).

If all five ports are held, `login` **refuses and names them** rather than falling back to an
ephemeral port — a fallback would bind fine and then fail at `/authorize`, pointing at the
server instead of the local collision. Use `--device-code`, which needs no local listener.

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

## `copilot-push`

Drains VS Code Copilot Chat's OTel **spool file** and exports it to the collector over
OTLP/HTTP.

```bash
governance-auth copilot-push --otel-endpoint https://otel.example
governance-auth copilot-push --dry-run      # parse and report; post nothing, move nothing
```

`--otel-endpoint` (or `GOVERNANCE_AUTH_OTEL_ENDPOINT`, or `otel_endpoint` in a config file —
ADR-0012 Decision 2's usual five layers) is **required**: with no collector configured the
command errors before it reads anything. `configure --otel-endpoint …` writes it to the
per-user config file, after which the bare `governance-auth copilot-push` in the timer units
below works.

### Why this command exists

Claude Code and Codex export telemetry themselves. Copilot Chat can too — but through an
HTTP exporter it only re-reads at window start, with no credential-helper hook, so a
300-second bearer stops working minutes into a session and fails silently. Its *file*
exporter has no such problem: it appends records to disk and something else ships them.
Nothing in VS Code is that something else. This is.

Turn it on in VS Code's `settings.json`:

```jsonc
"github.copilot.chat.otel.enabled": true,
"github.copilot.chat.otel.exporterType": "file",
"github.copilot.chat.otel.outfile": "~/.local/state/governance-auth/copilot-otel.jsonl"
```

`configure` does **not** write `exporterType: "file"` — it writes `otlp-http`, which is the
right default for anyone who has exported `OTEL_EXPORTER_OTLP_HEADERS` by hand. This command
is opt-in.

### ⚠️ The file is not OTLP

It is the OpenTelemetry **JS SDK's internal object graph**, serialised one JSON object per
line, private `_`-prefixed fields and all — `_body`, `_rawAttributes`, `hrTime` as
`[seconds, nanoseconds]`, and `dataPointType`/`aggregationTemporality` as the *JS SDK's*
enum integers, which are not OTLP's. `copilot-push` translates all of it.

None of that is a wire format anybody promised to keep stable, so the parser degrades:
a record it cannot read is **skipped and counted**, never fatal. Every run that finds
something to drain prints the tally (a run with nothing new prints `Nothing new in …`
instead and stops there):

```
49 metric record(s), 27 log record(s); 22 empty; discarded 0 (0 unparsable, 0 unrecognised, 0 unsupported metric(s), 0 bad data point(s))
```

`empty` is normal and not an error — Copilot's exporter really does write empty `{}` records
(22 of 98 on the file this parser was built against), and they carry nothing to lose. They are
counted apart from `discarded` for exactly that reason: if they were folded in, a healthy
install would show permanent loss and nobody would look at the number again.

**`discarded` is the one that matters.** It counts records that were consumed and will never
reach the collector, it is persisted in the checkpoint, and `status` shows it — see below. A
number that *starts climbing* after a VS Code update is the signal that the shapes moved.

### Fail-closed, stated exactly

**No valid token means no data is consumed.** The bearer is obtained first — before the spool
is opened, before the checkpoint is read, and including under `--dry-run`. A run that cannot
authenticate exits non-zero having advanced nothing, discarded nothing, and posted nothing.

`--dry-run` is deliberately *not* an offline preview. An offline mode would be a second path
that reads the spool without a credential, and "there is exactly one such path and it starts
with authentication" is a far easier property to keep true.

### Idempotency, and why the spool is never truncated

Progress is a **byte offset** in `<state_dir>/copilot-push.json`, advanced only after the
collector has returned 2xx. Re-running with nothing new appended posts nothing and changes
nothing. A rejected or unreachable collector leaves the offset where it was, so the same
bytes are retried next run.

There are **two** offsets, one per signal, plus the shared `offset` (the smaller of the two)
that the next drain starts from. Metrics and logs go to different endpoints and can be
accepted or refused independently; with a single offset, a run where `/v1/metrics` returned
200 and `/v1/logs` returned 503 re-posted the *accepted* metrics on every later wake, forever.
A checkpoint written by an older build carries only `offset`, and both signals resume from it.

Only one drain runs at a time. `<state_dir>/copilot-push.lock` guards the whole
read → POST → write sequence, because the timer and a developer running the command by hand
(which the `status` row below tells them to do) otherwise read the same offset and ship the
same records twice. A run that finds the lock held waits rather than failing.

### When a record is given up on

The drain is allowed to discard a record. It is not allowed to do it quietly, and the two
places it can happen are:

- **The parser cannot read it** — an unparsable line, an unrecognised shape, a metric kind
  this build does not translate, or a data point whose value changed type. Counted, the
  offset moves past it.
- **The collector permanently refuses it** — HTTP 400, 413 or 422 only. The batch is split in
  half and re-offered, down to single records, until the one responsible is isolated; it is
  then dropped and the rest go through. Every other failure (401, 403, 404, 408, 429, any 5xx,
  any network error) is retried on the next wake and advances nothing, because those say
  something about the moment or the deployment rather than about the payload.

The guard on the second rule: a record is only dropped once the collector has **accepted
something else from the same batch**. A collector misconfigured to reject everything is a
configuration fault, not a spool full of bad records, so that case advances nothing and
discards nothing. The cost is that a batch of exactly one refused record waits until
something acceptable arrives beside it — self-healing, not stuck.

Both counts land in the checkpoint as `discarded_total`/`last_discard_unix`, and `status`
shows them until they age out. The alternative — never advancing — is not the safe option it
looks like: one record the collector will never take would stop the stream at that byte
offset permanently, and take every record written after it with it.

The spool itself is never written to. VS Code holds it **open for append** for the life of
the window; truncating a file another process holds at offset N does not move that process's
offset, so the next append lands at N and the kernel zero-fills the gap — the file grows a
hole of NUL bytes and every later parse is garbage. That is true on Linux and macOS alike, so
there is no safe truncation to implement and none is attempted. Reclaiming disk is Copilot's
job (it rotates its own outfile) or a human's, with VS Code closed.

If the file *is* shorter than the recorded offset, that is a rotation: the drain restarts at
byte 0 and says so on stderr, so a duplicated push is explicable rather than mysterious.

A spool that is **not there at all** is neither a rotation nor an error. The checkpoint is
left exactly where it was and the run exits 0 — a path that does not exist says nothing about
how far the real spool was drained, and the reasons to be pointed at one are mundane (a
typo'd `--copilot-spool-path`, an edited config, a home directory not mounted yet, a run
before VS Code has recreated the file).

Only whole lines are consumed. A drain that lands mid-append sees half a JSON object, so the
offset never advances past the last newline actually read.

### Where the spool lives

Five layers, same precedence as everything else (ADR-0012 Decision 2): `--copilot-spool-path`
→ `GOVERNANCE_AUTH_COPILOT_SPOOL_PATH` → per-user config `copilot_spool_path` → machine-wide
config → `<state_dir>/copilot-otel.jsonl`.

### Running it on a timer

**This binary does not install these, deliberately** — writing to a developer's systemd or
launchd tree is a bigger claim on their machine than writing a dotfile, and it is out of
scope for this command. Copy them yourself.

`~/.config/systemd/user/governance-auth-copilot-push.service`:

```ini
[Unit]
Description=Drain the GitHub Copilot OTel spool to the governed collector

[Service]
Type=oneshot
# Absolute path: a user timer does not inherit your shell's PATH.
ExecStart=%h/.local/bin/governance-auth copilot-push
```

`~/.config/systemd/user/governance-auth-copilot-push.timer`:

```ini
[Unit]
Description=Drain the GitHub Copilot OTel spool every 5 minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
# NOTE: `Persistent=true` does NOT belong here and is not merely redundant —
# systemd.timer(5) defines it only for calendar (`OnCalendar=`) timers, so on a
# monotonic timer like this one it is silently ignored. It was here with a comment
# promising suspend/resume catch-up, which it never delivered. Switch the two
# monotonic lines above for `OnCalendar=*:0/5` if you want that behaviour, and
# then `Persistent=true` is real.

[Install]
WantedBy=timers.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now governance-auth-copilot-push.timer
systemctl --user list-timers governance-auth-copilot-push.timer
journalctl --user -u governance-auth-copilot-push.service -n 50
```

macOS — `~/Library/LaunchAgents/digital.camer.ai.governance-auth.copilot-push.plist`.

⚠️ `ProgramArguments` must be the **same** `~/.local/bin` path the systemd unit uses:
ADR-0012 makes `~/.local/bin` the per-user install location on both platforms, and a plist
pointing at `/usr/local/bin` fails on every wake for anyone who installed normally. launchd
does not expand `~`, so it is written out in full below — replace `YOUR-USERNAME`, or generate
the file with `sed "s|\$HOME|$HOME|"`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>digital.camer.ai.governance-auth.copilot-push</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOUR-USERNAME/.local/bin/governance-auth</string>
    <string>copilot-push</string>
  </array>
  <key>StartInterval</key>
  <integer>300</integer>
  <key>RunAtLoad</key>
  <true/>
  <!-- Not /tmp: that is world-writable, so the name is predictable and another
       local user can pre-create or replace the file. ~/Library/Logs is the
       per-user location Console.app already reads. Nothing rotates it, so
       either trim it occasionally or add a newsyslog.d entry. -->
  <key>StandardErrorPath</key>
  <string>/Users/YOUR-USERNAME/Library/Logs/governance-auth-copilot-push.log</string>
</dict>
</plist>
```

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/digital.camer.ai.governance-auth.copilot-push.plist
launchctl print gui/$(id -u)/digital.camer.ai.governance-auth.copilot-push
```

⚠️ A timer you installed is a timer nothing monitors. `status` carries a **copilot spool**
row for exactly that reason — see below.

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

At a terminal it also prints a table. Those three lines are the contract; the table is an
addition for a human and never a replacement, because `status` may be piped.

### The `copilot spool` row

| Shown | Colour | Means |
|---|---|---|
| `checkpoint unreadable` | red | `<state_dir>/copilot-push.json` will not parse |
| `not enabled` | yellow | no spool file — Copilot's file exporter is off |
| `<n> record(s) discarded` | **red** | data was consumed and never delivered, within the last 24h |
| `<n> record(s) discarded` | yellow | the same, but the last loss was more than 24h ago |
| `up to date (<n> bytes)` | green | everything written has been pushed, and nothing was lost |
| `<n> bytes pending` | yellow | a backlog, and a push has succeeded before |
| `<n> bytes pending` | **red** | a backlog and **no push has ever succeeded** |
| `unknown` | yellow | the state directory could not be resolved at all |

Rows are checked in that order, so a discard outranks a backlog and outranks green.

Two red rows, for the two ways this can fail silently:

- **`<n> bytes pending`, never pushed.** A user timer that was never enabled, or a launchd
  agent that fails on every wake, is indistinguishable from a working one everywhere else a
  developer looks — bytes climbing while `last push` stays at `never pushed` is the only
  visible difference. "Pending" after a successful push is *not* red on purpose: it is the
  ordinary state between wakes, and a row that cries wolf is one people stop reading.
- **`<n> record(s) discarded`.** A Copilot release renames the private fields this parser
  dispatches on; every record classifies as unrecognised, both payloads come out empty, no
  POST is made, and the checkpoint advances over the lot. Nothing is pending afterwards, so
  without this row the table said `up to date`, in green, while the whole spool went in the
  bin. It fades to yellow after a day because the counter is cumulative and there is no
  command to clear it — recent loss is an alarm, old loss is a note, neither is green.

`copilot-push --dry-run` prints the same tally the discard came from, which is where to look
for *what* this build cannot read.

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
