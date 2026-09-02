# Commands

Every subcommand, what it does, and — for the two that a machine parses — exactly what
lands on stdout.

**The stdout/stderr split is a contract, not a style choice.** `token` and `otel headers`
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

Accepts the same `--no-claude` / `--no-codex` / `--no-vscode` opt-outs as
[`configure`](#leaving-one-client-alone), because it does the same writing — and on a machine
being set up for the first time this is the only point before that writing happens.

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

## `refresh`

Forces a credential refresh right now, even when the cached session is still fresh.

```bash
governance-auth refresh
```

The difference from `token`: `token` only refreshes when the cached session is inside the
expiry skew, otherwise it prints the cached access token unchanged. `refresh` always goes to
the authorization server, regardless of how long the current token has left. Use it after a
server-side change — a role added, a scope edited — or when debugging, rather than waiting
for the cached token to age into the skew window.

**Prints nothing on stdout.** The refreshed token goes to the session cache, same as any
other refresh; `token` remains the only command that ever emits a credential. All reporting
is on stderr.

Requires an existing cached session with a refresh token. No cached session, or a cached
session with no refresh token, is a non-zero exit naming `login` — `refresh` never opens a
browser and never falls back to an interactive flow.

**A refused refresh leaves the cached session byte-identical.** If the authorization server
rejects the refresh (network failure, revoked session, refused grant), nothing already
cached is overwritten. A network blip while running `refresh` cannot log you out — the
existing session is left exactly as it was, and the command reports the failure on stderr and
exits non-zero.

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

### The `copilot drain` row

Is anything going to come and collect the spool? `configure` installs the schedule, so this
row reports something the binary owns rather than something it hoped a human did.

| Shown | Colour | Means |
|---|---|---|
| `every 300s` | green | the timer/agent is installed **and** the scheduler confirms it is running |
| `installed, not running` | **red** | the scheduler confirms it is stopped; the note names the command that starts it |
| `installed` | yellow | the scheduler could not be asked — **not** the same as stopped |
| `not scheduled` | **red** | Copilot's file exporter is on and nothing drains it: run `configure` |
| `not scheduled` | none | no collector configured, so there is nothing to schedule |
| `unknown` | yellow | the home directory could not be resolved |

⚠️ `active` is three-valued on purpose, and the reason is measured. `systemctl --user
is-active` exits non-zero for **both** a stopped timer and a machine with no user manager, so
the exit code cannot tell them apart — it would send every container user to debug a timer that
is fine. The **stdout** can: a stopped timer prints `inactive` (exit 3), while no reachable user
manager prints nothing at all (exit 1, with `Failed to connect to user scope bus …` on stderr).
That is what the row reads.

### The `copilot spool` row

| Shown | Colour | Means |
|---|---|---|
| `checkpoint unreadable` | red | `<state_dir>/copilot-push.json` will not parse |
| `not enabled` | yellow | no spool file yet — Copilot has not exported since `configure` |
| `<n> record(s) discarded` | **red** | data was consumed and never delivered, within the last 24h |
| `<n> record(s) discarded` | yellow | the same, but the last loss was more than 24h ago |
| `held, waiting for a later record` | yellow | the spool's **last** record is refused; see above |
| `up to date (<n> bytes)` | green | everything written has been pushed, and nothing was lost |
| `<n> bytes pending` | yellow | a backlog, and a push has succeeded before |
| `<n> bytes pending` | **red** | a backlog and **no push has ever succeeded** |
| `unknown` | yellow | the state directory could not be resolved at all |

Rows are checked in that order, so a discard outranks a hold, which outranks a backlog, which
outranks green. `held` has to beat `<n> bytes pending` because the bytes genuinely *are*
pending: nothing else in the row distinguishes the two, and the backlog row's advice ("run
`governance-auth copilot push`") is the one command that cannot help.

Two red rows, for the two ways this can fail silently:

- **`<n> bytes pending`, never pushed.** A schedule that fails on every wake is
  indistinguishable from a working one everywhere else a developer looks — bytes climbing
  while `last push` stays at `never pushed` is the only visible difference. The `copilot
  drain` row above catches the case where the schedule is *absent or stopped*; this one
  catches the case where it fires and the wake fails. "Pending" after a successful push is
  *not* red on purpose: it is the ordinary state between wakes, and a row that cries wolf is
  one people stop reading.
- **`<n> record(s) discarded`.** A Copilot release renames the private fields this parser
  dispatches on; every record classifies as unrecognised, both payloads come out empty, no
  POST is made, and the checkpoint advances over the lot. Nothing is pending afterwards, so
  without this row the table said `up to date`, in green, while the whole spool went in the
  bin. It fades to yellow after a day because the counter is cumulative and there is no
  command to clear it — recent loss is an alarm, old loss is a note, neither is green.

`copilot push --dry-run` prints the same tally the discard came from, which is where to look
for *what* this build cannot read.

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

### Leaving one client alone

```bash
governance-auth configure --no-codex     # wire Claude Code and VS Code; don't touch ~/.codex
```

`--no-claude`, `--no-codex` and `--no-vscode` each leave that client entirely alone. The same
three flags are accepted on [`login`](#login), which writes the same configuration — on a fresh
machine that is the only chance to keep a client untouched before it is first written.

A third report line distinguishes this from a tool that simply isn't here, because the two are
different facts and only one is actionable:

```
Configured: /home/dev/.claude/settings.json
Left alone (--no-codex): /home/dev/.codex
Skipped: /home/dev/.config/Code/User not present.
```

`--no-vscode` also leaves the Copilot drain timer as it is — it neither installs one nor
removes one. Removing it would strand a spool that Copilot is *still* writing, precisely
because the flag deliberately did not turn its exporter off.

Naming a client that isn't installed is a no-op, not an error. Naming all three is accepted
too, and says so — the shell environment file and this binary's own settings are not a
client's config and are still written, so there is real work left to report.

**These flags do not un-manage a client.** Keys written by an earlier run are kept *and* stay
recorded as ours, so a later run without the flag can still take them back. Why that
distinction is the whole difficulty, and what it costs to get wrong, is in
[`configuration.md`](./configuration.md#--no-claude---no-codex---no-vscode).

`status` is unaffected and deliberately so: it reports what is on disk against what this binary
would generate today, so an opted-out client reads as unconfigured — which is the truth about
that machine, and not something `status` should soften because of a flag on a different run.

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

## `otel headers`

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

## `copilot push`

Drains VS Code Copilot Chat's OTel **spool file** and exports it to the collector over
OTLP/HTTP.

```bash
governance-auth copilot push --otel-endpoint https://otel.example
governance-auth copilot push --dry-run      # parse and report; post nothing, move nothing
```

`--otel-endpoint` (or `GOVERNANCE_AUTH_OTEL_ENDPOINT`, or `otel_endpoint` in a config file —
ADR-0012 Decision 2's usual five layers) is **required**: with no collector configured the
command errors before it reads anything. `configure --otel-endpoint …` writes it to the
per-user config file, after which the bare `governance-auth copilot push` in the timer units
below works.

### Why this command exists

Claude Code and Codex export telemetry themselves. Copilot Chat can too — but through an
HTTP exporter it only re-reads at window start, with no credential-helper hook, and no header
this binary is willing to write (`github.copilot.chat.otel.headers` is static, and
`settings.json` is covered by Settings Sync). Against an authenticating collector that
exporter returns **401 on every span**, silently. Its *file* exporter has no such problem: it
appends records to disk and something else ships them, with a bearer that binary refreshes
itself. Nothing in VS Code is that something else. This is.

**Both halves are `configure`'s job now**, and neither is opt-in:

```jsonc
// written by `governance-auth configure` into every VS Code flavour present
"github.copilot.chat.otel.enabled": true,
"github.copilot.chat.otel.exporterType": "file",
"github.copilot.chat.otel.outfile": "<resolved spool path>"
```

…plus the timer that drains it — see [Running it on a timer](#running-it-on-a-timer). The
`outfile` and the timer's `--copilot-spool-path` come from **one** resolution of ADR-0012's
five layers, so they cannot disagree; previously they were two copy-pastes out of this
document.

Restart VS Code after `configure`: Copilot reads these at window start.

### ⚠️ The file is not OTLP

It is the OpenTelemetry **JS SDK's internal object graph**, serialised one JSON object per
line, private `_`-prefixed fields and all — `_body`, `_rawAttributes`, `hrTime` as
`[seconds, nanoseconds]`, and `dataPointType`/`aggregationTemporality` as the *JS SDK's*
enum integers, which are not OTLP's. `copilot push` translates all of it.

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

### Idempotency

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
same records twice. A run that finds the lock held waits rather than failing — but not for
ever: after **two minutes** behind a drain that is still running it gives up on that wake and
says so, rather than queueing behind a process that may be stuck. It never steals a valid
lock.

### Progress is monotonic, even when a wake fails

An offset moves over the records a wake **resolved**, counted from the front, whether or not
the wake as a whole succeeded. That is not a detail: what the collector accepted is recorded
as it happens, so a wake that stops half way costs a wake and never a duplicate. The split
below therefore works strictly left to right, because a byte offset can only ever say
"everything up to here is done".

Work per wake is bounded (512 requests per signal). Reaching that bound is not a failure of
the batch — the wake stops, the offset stands where it got to, and the next wake continues.

### A wake drains a backlog, not 8 MiB

The spool is read in 8 MiB chunks, which bounds the memory one read costs. That used to bound
the *wake* as well, and on 2026-09-02 a maintainer's 164 MB spool measured **8,385,060 bytes
per wake** — 27 KB/s at the five-minute interval, ~18 wakes and 1.5 hours to catch up, and
never at all if Copilot wrote faster than that. It also made the reclaim below unreachable on
exactly the machines that needed it, because that fires only once the spool is caught up.

A wake now repeats the read → export → checkpoint pass until the spool is caught up or one of
three things stops it:

- **A pass that could not resolve everything it read ends the wake.** This is correctness, not
  throttling: those records have already been offered, and offering them again in the same
  wake would count one wake's refusal twice against the two-separate-wakes rule.
- **60 seconds**, checked between passes. A quarter of `TimeoutStartSec=240`; half the 120s a
  second `copilot-push` waits for the lock, so a hand-run during a backlog drain still gets in;
  a fifth of the 300s interval, so wakes cannot queue.
- **64 passes**, i.e. 512 MiB — a bound that does not depend on how fast the machine is, which
  is what macOS needs, since launchd has no `TimeoutStartSec=` equivalent (see below).

Peak memory is unchanged: each pass still reads at most 8 MiB and the previous pass's records
are dropped before the next read. A wake that made more than one pass says so on stderr
(`Drained N sweeps of <path> in one wake: … byte(s), … record(s).`), including which bound
stopped it and how much is still pending; a healthy one-pass wake prints nothing extra, so a
backlogged machine is legible in the journal. Nothing is lost when a bound fires — every byte
the wake drained was checkpointed as it went.

**And the checkpoint is written as the prefix advances, not once at the end of the wake.**
This binary installs no signal handler, and a handler would not be enough anyway: it covers
SIGTERM and covers neither SIGKILL, nor the OOM killer, nor a laptop losing power. A wake that
was killed part way through used to throw away every acceptance it had obtained — the
collector had the records, this side had no record of it, and the next wake re-sent all of
them, for ever if the kill was recurring. So each advance of the resolved prefix is persisted
at the moment it happens, together with anything that advance moved past uncounted.

The cost is one small write per prefix advance, each of which follows an HTTP round trip that
is orders of magnitude slower. An ordinary wake writes the checkpoint **twice** (once per
signal); only a wake that bisects a refused batch writes more, and it does one request of work
between each. The writes are `tmp`-then-`rename`, which survives process death because the
page cache does; they are deliberately **not** `fsync`ed, so host power loss can still cost the
duplicates this otherwise prevents. That is a judgement call for a five-minute developer timer
and would not be the right one for anything with a stricter duty.

### When a record is given up on

The drain is allowed to discard a record. It is not allowed to do it quietly, and the two
places it can happen are:

- **The parser cannot read it** — an unparsable line, an unrecognised shape, a metric kind
  this build does not translate, a data point whose value changed type, or a record that
  parses but carries nothing this build can export (a log line with no `_body`, a metrics
  line that declared a metric and produced no data points). Counted, the offset moves past it.
- **The collector permanently refuses it** — HTTP 400, 413 or 422 only. The batch is split in
  half and re-offered, down to single records, until the one responsible is isolated; it is
  then dropped and the rest go through. Every other failure (401, 403, 404, 408, 429, any 5xx,
  any network error) is retried on the next wake and advances nothing, because those say
  something about the moment or the deployment rather than about the payload.

Two guards on the second rule:

- **A refusal is evidence, not a verdict.** A record is only given up on once it has been
  refused on its own across **two separate wakes**. An HTTP 400 is a deterministic function of
  the payload only if nothing sits in front of the collector; a WAF, a proxy or an upstream
  restart returns 400 for reasons of its own, and a drain that trusted a single one deleted
  valid telemetry. The first refusal *holds* the record and stops that wake there — everything
  before it is already delivered and recorded. On a five-minute timer a bad record therefore
  costs one extra wake, and a batch with several costs one wake each.
- **And only once the collector has been shown to accept something *in this pass*.** Either
  the split already delivered records before reaching this one, or — when the bad record is at
  the very front and there is nothing before it to prove anything with — the next record
  carrying that signal is offered on its own as a one-request probe. A collector misconfigured
  to reject everything refuses the probe too, so that case advances nothing and discards
  nothing.

  ⚠️ There is deliberately **no** fallback to "the checkpoint says a push succeeded within the
  last hour (`last_push_unix`)". That answer was implemented, tested and rejected: it is cheap
  and it is wrong in exactly the case the rule exists for — a collector that worked this
  morning and rejects everything now. With it, a five-minute config error empties the spool one
  record per wake for as long as the window lasts. The proof is obtained live, every time.

  ⚠️ **This narrows the loss, it does not eliminate it.** Measured against a gateway returning
  400 non-deterministically, 9 of 150 small-spool rounds still permanently discarded a *valid*
  record — down from 29 of 150 before the two-wake rule, and not zero. A record that a flaky
  transport happens to refuse on two separate wakes, while other records in the same wake
  succeed, is indistinguishable from a bad one with the evidence available here. `status` shows
  the discard; nothing recovers it.

  ⚠️ The quarantine counter is keyed on a digest of the record's **content**, so two
  byte-identical spool lines share one refusal count and a refusal of either counts against
  both. Real records carry nanosecond timestamps and span ids, so a collision is close to
  impossible, and the table's 7-day TTL bounds the consequence either way. Noted rather than
  designed around.

Both counts land in the checkpoint as `discarded_total`/`last_discard_unix`, and `status`
shows them until they age out. The alternative — never advancing — is not the safe option it
looks like: one record the collector will never take would stop the stream at that byte
offset permanently, and take every record written after it with it.

### The one stall that does not clear itself

There is an exception to "the next wake resolves it", and it is permanent. If the refused
record is the **last** one in the spool, there is nothing after it to offer as the probe, so
the second condition above can never be met. The record is held rather than discarded — which
is the correct choice, because discarding on no evidence is how a misconfigured collector
empties a spool — and every wake from then on reads it, refuses it, and exits 1.

It clears when Copilot writes another record, and only then. Re-running `copilot push` by hand
does exactly what the timer just did.

`status` reports this as its own row, **held, waiting for a later record** (yellow), rather
than as `N bytes pending … run governance-auth copilot push`. The byte counts are identical,
so nothing else in the row distinguishes them — and the backlog row's advice is a command that
reproduces the same failing wake.

### Reclaiming the spool

Nothing bounded this file until now. It was measured growing 73 KB → 315 KB in six minutes of
ordinary use and reached **164 MB** on one machine, still climbing.

A wake reclaims it when **both** of these hold, and never otherwise:

- the spool is over **1 MiB** — the same figure the log rotation uses, and small enough that a
  spool under it is not a disk problem worth acting on;
- its size is **exactly** the checkpoint's `offset` — every byte in the file has been delivered
  or counted.

It is then truncated to zero, the checkpoint is reset to byte 0 with the identity of the file
as it now stands, and the run says so. `--dry-run` never reclaims. A reclaim that fails is
reported and the wake continues: an oversized spool is a disk problem, and failing the wake
over it would make it a delivery one.

⚠️ **This document said the opposite, and the correction is the point.** It said the spool is
never written to, because truncating a file VS Code holds open at offset N leaves the next
append at N with the gap zero-filled. That is true of a plain `O_WRONLY` handle and **false for
this writer.**

Measured on macOS on 2026-09-02: `lsof -o` showed VS Code holding three write descriptors on
the spool, every one reporting an offset exactly equal to the file size and advancing in
lockstep — three independent `open()` calls cannot stay synchronised unless every write seeks
to EOF atomically, which is `O_APPEND`. The live spool was then truncated with VS Code running
and holding those descriptors: Copilot's next append started at byte 0, `od -c` showed the
record with **no NUL hole**, it parsed, and the next drain reported the truncation, restarted
at byte 0 and left `discarded_total` at 0.

Confirmed on Linux from the kernel rather than inferred — `/proc/PID/fdinfo` reports the open
flags directly:

```
pid=2081405 code  fd=60  flags=02102001  O_APPEND=1
pid=2081405 code  fd=62  flags=02102001  O_APPEND=1
pid=2081405 code  fd=64  flags=02102001  O_APPEND=1
```

`02102001` is `O_WRONLY | O_APPEND | O_LARGEFILE | O_CLOEXEC`. The [log
rotation](./files.md#log) had already written the same `O_APPEND` argument down in the
affirmative about the same OS behaviour; the two disagreed, and this was the wrong one.

**Conservation still binds.** A truncate destroys bytes instead of advancing over them, so it
may only ever destroy bytes the offset has already passed — hence the exact `size == offset`
precondition, re-read from the open descriptor immediately before the truncate. One byte past
it, including a half-written record, and the wake declines. That narrows the race with a
concurrent append; it does not close it, and no POSIX call does. The window and its measured
bound are in `app/governance-auth/src/copilot/spool/reclaim.rs`, along with why a
tail-preserving rewrite and a sparse hole-punch were both rejected. The bound on the file is
honest rather than hard: 1 MiB plus whatever accrues between the wake that crosses it and the
next fully caught-up wake.

A crash between the truncate and the checkpoint write needs no handling of its own: it leaves
a file shorter than the recorded offset, which is the truncation case below.

### Which file the offset belongs to

An offset is a byte count into *some* file, so the checkpoint records **which**: the spool's
inode and device, plus a SHA-256 of its first 4 KiB. Appending never changes either, so the
pair is a stable name for the same file; a mismatch in either means the drain restarts at byte
0 and says so on stderr.

Both halves are needed. An inode number can be reused, at which point a brand-new file
inherits the old one's identity; a digest cannot tell a file from a byte-identical copy, which
is what copy-truncate rotation produces.

⚠️ Size alone cannot answer this, and used to be all that was asked. `size < offset` catches a
truncation, and it is *false* for the ordinary case: VS Code recreates its outfile on restart,
the developer keeps working, and by the next timer wake the new file is already longer than
the offset the old one left behind. The drain then resumed into the middle of a file it had
never read. Measured: a 2,700-byte spool replaced by a 5,412-byte one lost six brand-new
records — not delivered, not counted, offset at the end — with `discarded_total` moving by one,
for the partial-line fragment at the resume point. The spool was separately measured growing
73 KB → 315 KB in six minutes of ordinary use, so outgrowing a stale offset inside one
five-minute window is the normal outcome of a restart, not a corner case.

A checkpoint written before this field existed carries no identity. That is **not** treated as
a mismatch — doing so would re-export every developer's whole spool on upgrade. The current
file's identity is adopted and the drain carries on from the recorded offset.

The `size < offset` check remains, and answers first: copy-truncate keeps the inode, so it is
the case that reads as a truncation rather than a replacement, and the two stderr messages
differ because they send the reader looking in different places.

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

**`configure` installs and activates these for you.** That reverses an earlier decision — the
claim on a developer's systemd or launchd tree is real — and it was reversed because the
alternative measured worse: `configure` already writes four config files across three tools,
and the one step it left to the human was the one without which none of the rest does anything
for Copilot.

What it runs:

| Platform | Files | Activation |
|---|---|---|
| Linux | `~/.config/systemd/user/governance-auth-copilot-push.{service,timer}` | `systemctl --user daemon-reload` then `enable --now …timer` |
| macOS | `~/Library/LaunchAgents/digital.camer.ai.governance-auth.copilot-push.plist` | `launchctl bootout` (ignored if not loaded) then `launchctl bootstrap gui/$(id -u)` |

⚠️ **The `bootout` before `bootstrap` is not defensive tidiness.** Measured: bootstrapping an
already-loaded label fails with `Bootstrap failed: 5: Input/output error` and leaves the *old*
argv running — so a changed endpoint would be written to disk and never used.

⚠️ **`ExecStart=` words are quoted *and* their `%` doubled.** Measured against a live systemd:
`"--copilot-spool-path" "/state/%h-cache/spool.jsonl"` is parsed into
`/state//root-cache/spool.jsonl` — `%h` is the home-directory specifier, quoting does not
suppress it, and `systemd-analyze verify` passes the unit without comment. The drain would then
read a file that does not exist and report nothing wrong. `%%` is the only escape.

A failure to activate — no user systemd session in a container, WSL without systemd, a CI
runner — is a **warning, not a failed `configure`**. Every config file is already written and
this command still runs by hand.

Uninstalling: run `configure` with no `--otel-endpoint` (it removes the units and stops the
timer, the same retraction rule the config keys follow), or by hand:

```bash
systemctl --user disable --now governance-auth-copilot-push.timer
```

```bash
launchctl bootout gui/$(id -u)/digital.camer.ai.governance-auth.copilot-push
```

The rest of this section is the generated content, kept here because reading it is how you
audit what was installed on your machine.

`~/.config/systemd/user/governance-auth-copilot-push.service`:

```ini
[Unit]
Description=Drain the GitHub Copilot OTel spool to the governed collector

[Service]
Type=oneshot
# Absolute path: a user timer does not inherit your shell's PATH.
ExecStart=%h/.local/bin/governance-auth copilot push
# ⚠️ NOT optional. systemd.service(5) defaults `TimeoutStartSec=` for a
# `Type=oneshot` unit to *infinity*, so without this line a wake that gets
# stuck is never killed — it just sits there. It is deliberately shorter than
# the timer interval so wakes cannot pile up.
#
# ⚠️ It is also a REAL kill, not a theoretical one, and the earlier claim that
# "the command should never come close to this" was wrong. The HTTP read
# timeout (15s) bounds one stall, not a wake; a wake that bisects a refused
# batch may make up to 512 requests per signal and can legitimately exceed
# four minutes. What makes that safe is not the number — it is that progress
# is now durable as it happens (see "Progress is monotonic" above), so a
# killed wake keeps everything it delivered and the next one resumes. Before
# that, a wake which consistently exceeded this never checkpointed at all and
# re-delivered its whole range for ever.
#
# 240 is kept rather than raised: with durable progress, being killed costs a
# wake and nothing else, while a longer timeout lets a genuinely stuck drain
# hold the lock for longer and delays the next wake behind it. Raise it only
# if you see wakes repeatedly killed mid-drain in the journal.
TimeoutStartSec=240
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
    <string>copilot</string>
    <string>push</string>
  </array>
  <key>StartInterval</key>
  <integer>300</integer>
  <key>RunAtLoad</key>
  <true/>
  <!-- ⚠️ launchd has NO equivalent of systemd's TimeoutStartSec=, and the
       plausible-looking keys are not it: ExitTimeOut bounds how long launchd
       waits after SIGTERM before SIGKILL, not how long the job may run.
       A StartInterval job that is still running when the interval fires is
       simply skipped, so a wedged wake does not pile up — it silently stops
       the drain instead, which is worse. On macOS the guarantee therefore
       comes entirely from the command itself: the HTTP read timeout, the
       copilot-push lock ceiling, and the wake's own 60s / 64-pass drain
       bounds. All are in-process, so they apply here exactly as they do
       under systemd.
       If you want an external backstop as well, install GNU coreutils and
       wrap the three ProgramArguments strings in
       `gtimeout 240 <path> copilot push`. -->
  <key>AbandonProcessGroup</key>
  <false/>
  <!-- Not /tmp: that is world-writable, so the name is predictable and another
       local user can pre-create or replace the file. ~/Library/Logs is the
       per-user location Console.app already reads. This is the SAME file
       governance-auth writes its own log to, which is what bounds it: the
       binary copy-truncates it past 1 MiB and keeps 3 generations, so no
       newsyslog.d entry and no hand-trimming is needed. -->
  <key>StandardErrorPath</key>
  <string>/Users/YOUR-USERNAME/Library/Logs/governance-auth/governance-auth.log</string>
</dict>
</plist>
```

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/digital.camer.ai.governance-auth.copilot-push.plist
launchctl print gui/$(id -u)/digital.camer.ai.governance-auth.copilot-push
```

⚠️ A schedule nothing monitors is a schedule that stops silently. `status` carries two rows
for that: **copilot drain** (is the timer installed and running?) and **copilot spool** (is it
keeping what it reads?). Independent answers, both needed — see below.

---

## `self update`

Replaces this binary with the latest GitHub release for this platform, from
`ADORSYS-GIS/lightbridge-governance`.

```bash
governance-auth self update
governance-auth self update --dry-run   # report only, change nothing
```

Unlike every other subcommand, this one **does not resolve the OAuth config** — it talks
only to the GitHub releases API. Resolving first used to make `self update` fail with
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

### `--version` and the self update loop

A released binary reports its **release tag**, injected at build time via
`GOVERNANCE_AUTH_RELEASE_VERSION`; a locally-built one falls back to `CARGO_PKG_VERSION`.
Both `--version` and the version `self update` compares against read the same constant.

This is not a detail. When they disagreed, a binary that had just updated still reported the
old version, decided it was out of date, and updated again — forever. Two tests pin it: one
proves a version-misreporting binary never terminates, and one asserts the release workflow
still sets that environment variable, because losing it would silently reintroduce the loop
on a real release only.
