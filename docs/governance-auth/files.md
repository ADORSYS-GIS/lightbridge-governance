# Files: what this binary reads, and what it writes

`governance-auth` edits other people's dotfiles. That is unusual enough to be worth an
exhaustive inventory — which files, which keys inside them, and what it deliberately leaves
alone.

**Every foreign config file is merged, never rewritten.** A developer's `settings.json`
carries their theme and permissions; their `config.toml` carries project trust levels and
hand-written comments. Only the keys listed below are touched. Writes are tmp-then-rename at
mode `0600`, because a crash mid-write must not leave either tool with an unparseable config
— Codex in particular *refuses to start* on a malformed `config.toml`, it does not degrade.

---


## The callback page comes from another repository

The page the browser lands on after the loopback redirect is **built in
[`ADORSYS-GIS/converse-frontends`](https://github.com/ADORSYS-GIS/converse-frontends)**
(`apps/governance-auth`, React + Vite) and vendored here as one self-contained file:

| File | What it is |
|---|---|
| `src/oauth/callback_page/callback.html` | the built artifact, ~566 KiB, everything inlined |
| `src/oauth/callback_page/callback.source.json` | which commit built it, and its sha256 |

It is the only surface of `governance-auth` a developer ever *sees*, so it composes the same
design primitives as the console and the auth plane rather than approximating them in
hand-written markup.

### Refreshing it

```bash
scripts/vendor-callback-page.sh <converse-frontends-commit-sha>
```

That pulls `ghcr.io/adorsys-gis/governance-auth-callback:sha-<sha>` with `oras`, re-checks the
self-containment properties on arrival, and rewrites both files. `callback.source.json` then
records exactly which upstream commit this binary serves — which is the whole point of pinning a
SHA rather than `latest`.

⚠️ **The pull is deliberately not part of `cargo build`.** `include_str!` runs at compile time,
so fetching during the build would put the network on the path of every build, break offline and
air-gapped builds, and make the binary's contents depend on *when* it was compiled rather than on
what is committed. Refreshing is an explicit act that produces a reviewable diff.

`the_vendored_page_matches_its_recorded_digest` fails if `callback.html` is edited by hand, which
is the one failure the upstream build gate cannot see.

### Trying it without touching GHCR

The pull path is exercisable against a local registry, so you can verify the whole loop before
anything is published — and so a refresh is not something only CI can do:

```bash
docker run -d --name reg -p 5555:5000 registry:2

# push, exactly as the workflow does
cd <converse-frontends>/apps/governance-auth/dist
oras push --plain-http 127.0.0.1:5555/governance-auth-callback:sha-$SHA \
  --artifact-type application/vnd.adorsys.governance-auth-callback.v1 \
  index.html:text/html

# pull, through the real script
cd <lightbridge-governance>
GOVERNANCE_AUTH_CALLBACK_ARTIFACT=127.0.0.1:5555/governance-auth-callback \
  scripts/vendor-callback-page.sh $SHA
cargo test -p governance-auth --bin governance-auth callback_page
```

The script adds `--plain-http` for loopback registries only — `127.0.0.1`, `localhost`, `[::1]` —
which is the same HTTPS-or-loopback rule [`security.rs`](../../app/governance-auth/src/security.rs)
applies to the issuer URL, and for the same reason: a local registry has no certificate to present,
while a remote one downgraded to plaintext is an attack.

⚠️ Provenance always records the **canonical** GHCR location, never the registry a given run
happened to pull from — the field answers *where does this come from*, and a `127.0.0.1` ref
committed into it would be a lie about the supply chain. A run using the override says so on
stderr.

### Why it can carry `default-src 'none'`

The response sets a Content-Security-Policy whose hashes are **derived from the page itself** at
request time, so re-vendoring cannot leave it stale. Because the artifact provably fetches nothing,
the policy is not a compromise — it is the narrowest thing that still renders, and it turns
"self-contained" from a property a test asserts upstream into one the browser enforces here.

`frame-ancestors 'none'` and `form-action 'none'` matter more than usual on this page: it is the
redirect target of an authorization code flow, so the URL carries a `code`. Framing it or letting
it submit a form anywhere are precisely how that value leaves the machine.

## Files this binary owns

### Session state

```
$XDG_STATE_HOME/governance-auth/<sha256(issuer\0client_id)>.json      # Linux
~/.local/state/governance-auth/<hash>.json                            # Linux fallback
~/Library/Application Support/governance-auth/<hash>.json             # macOS
```

Mode `0600`, in a directory created at `0700`. The directory mode is defence in depth — the
files are already `0600` — but it stops the *directory listing*, which leaks the set of
issuer/client pairs this developer holds sessions for, from being world-readable.

The filename is a hash of the issuer and client id, so several deployments coexist without
colliding.

**Why state, not cache.** This file holds a refresh token, so deleting it logs the developer
out. That makes it state by the XDG spec's own definition, not cache. It used to live under
`$XDG_CACHE_HOME` / `~/Library/Caches`, which is actively dangerous rather than merely
untidy: macOS treats `~/Library/Caches` as **purgeable** and may evict it under disk pressure
with no warning, and every "free up disk space" tool does the same to `~/.cache` on Linux.
The consequence would not be a re-login prompt at a convenient moment — `token` fails closed
*inside* a running session, and Codex responds to that by proceeding unauthenticated.

macOS deliberately gets `~/Library/Application Support` rather than `~/.local/state`: the
whole reason for the move was that the OS may purge the old location, and Application Support
is Apple's non-purgeable per-user directory. Config stays at `~/.config` on both platforms.
One convention per *kind* of data, not one per platform.

A session found at the legacy cache path is migrated once, on read — copy, verify, unlink,
rather than `fs::rename`, because the two directories are frequently on different filesystems
(`EXDEV`), and a migration that silently fails is a logout.

### Lock file

```
<state dir>/governance-auth/<hash>.lock
```

Held across the read-refresh-write critical section. Claude Code and Codex can both invoke
`token` at nearly the same moment on a cold store; without the lock they would both refresh,
and one would store a session the other had already superseded.

The lock file records the holder's PID. Two failure modes were paid for here:

- An **empty** lock file — a crash between create and write — used to be read as
  "undeterminable", which meant waiting out the full 300-second stale timeout on every
  invocation. An empty file is now treated as *confirmed dead*, and the writer removes the
  file if it fails to record its own PID.
- Non-empty garbage stays undeterminable, which is the correct conservative answer for a
  file that might belong to a live process.

### Copilot drain checkpoint

```
<state dir>/governance-auth/copilot-push.json
```

Mode `0600`, written tmp-then-rename. Records how far into the Copilot spool
[`copilot push`](./commands.md#copilot-push) has got:

| Key | Meaning |
|---|---|
| `offset` | the byte the next drain starts at: the lesser of the two below |
| `metrics_offset` | bytes `/v1/metrics` has accepted |
| `logs_offset` | bytes `/v1/logs` has accepted |
| `last_push_unix`, `last_push_records` | the last delivery; untouched by a run that delivered nothing |
| `discarded_total`, `last_discard_unix` | records consumed that will never reach the collector |
| `quarantine` | records the collector refused on their own, keyed by a truncated SHA-256 of the line |

Two offsets rather than one because the signals go to different endpoints and are accepted
independently; a single offset re-posted an accepted metrics batch on every wake for as long
as logs kept failing. A file written by an older build has neither key, and both signals
resume from `offset` — defaulting them to 0 would re-export the whole spool after an upgrade.
`quarantine` is likewise absent from an older file and defaults to empty.

`discarded_total` is what makes loss visible rather than merely logged: the drain is allowed
to give up on a record it cannot translate or the collector will not take (otherwise one bad
record stops the stream permanently), and this is the count `status` turns non-green on.

`quarantine` is what stops a *single* refusal being enough to give up on a record — a 400 from
a proxy is not a statement about the payload, and a drain that treated it as one deleted valid
telemetry. Each entry counts the separate wakes that refused one record and whether the
collector has been shown to accept anything meanwhile; entries expire after a week and the
table is capped, so it cannot grow without bound. **The key is a digest, never the record**:
`AGENTS.md` bans writing a payload anywhere, and this one is prompt-adjacent telemetry.

A sibling `copilot-push.lock` guards the whole read-drain-post-write sequence, so the timer
and a hand-run command cannot both ship the same records. Same PID-liveness stale-lock
recovery as the session lock above, plus a two-minute ceiling on waiting for a drain that is
still running — a timer has nothing to wait for, and one stuck process must not wedge every
later wake behind it. The ceiling gives up; it never steals a valid lock.

State, not cache, for the same reason the session is — losing it does not log anyone out, but
it does mean the next run re-pushes the whole spool, which is duplicate usage data at the
collector.

Deliberately **not** named by `sha256(issuer\0client_id)` the way session files are. The spool
belongs to this machine's VS Code install, not to whichever identity happens to be pushing it;
two checkpoints against one spool would each skip the other's bytes.

An unparseable checkpoint is a **hard error**, not a silent restart. Defaulting to offset 0
would re-push everything already sent; defaulting to the current size would discard everything
not yet sent. Both are wrong in a way nobody would notice, so the command names the file and
stops — deleting it is a decision for whoever is looking.

### Copilot OTel spool

```
<state dir>/governance-auth/copilot-otel.jsonl      # the compiled default
```

**Written by VS Code, never by this binary.** It is only the default location; the path comes
from `--copilot-spool-path` / `GOVERNANCE_AUTH_COPILOT_SPOOL_PATH` / `copilot_spool_path` /
this default. `configure` writes the *resolved* value into
`github.copilot.chat.otel.outfile` and into the drain's own schedule, so the two always match.

**Reclaimed by `copilot push`, above 1 MiB and only when it is caught up.** Growth was measured
at 73 KB → 315 KB in six minutes of ordinary use, and the file reached **164 MB** on the machine
that reported [#230](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/230) /
[#241](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/241).

A wake truncates it to zero when its size is **exactly** the checkpoint offset — every byte
delivered — and leaves it alone otherwise. The bound is honest rather than hard: 1 MiB plus
whatever accrues between the wake that crosses it and the next fully caught-up wake.

⚠️ This page previously said the file is never truncated, because truncating underneath a live
writer strands its offset and zero-fills the gap. That is true of a plain `O_WRONLY` handle and
false for this one: Copilot opens the spool `O_APPEND`, so every write seeks to EOF atomically.
Confirmed on macOS by three descriptor offsets tracking EOF in lockstep, and on Linux directly
from `/proc/PID/fdinfo` (`flags=02102001`). See
[`commands.md`](./commands.md#reclaiming-the-spool).

⚠️ "Fully caught up" is a precondition a backlogged machine could not reach while a wake drained
only 8 MiB: the 164 MB spool measured on 2026-09-02 was moving 8,385,060 bytes per wake, about
27 KB/s, so the spools with the most to reclaim were the ones that never presented the
precondition. A wake now repeats the drain until the spool is caught up or one of its bounds
stops it (see [commands.md](commands.md) → *A wake drains a backlog, not 8 MiB*), which is what
makes this reclaim reachable at all on those machines — 23.6 MB drained and truncated in a
single wake in `copilot_push_backlog.rs`.

### Copilot drain schedule

```
~/.config/systemd/user/governance-auth-copilot-push.service        # Linux
~/.config/systemd/user/governance-auth-copilot-push.timer          # Linux
~/Library/LaunchAgents/digital.camer.ai.governance-auth.copilot-push.plist   # macOS
```

The agent's `StandardErrorPath` is **not** a file of its own: it points at the same rotating
log described below, so the drain's stderr and the drain's own log lines end up in one place
and one bound. Builds before that reconciliation used
`~/Library/Logs/governance-auth-copilot-push.log`, which nothing rotated; `configure` deletes
it on the next run.

**Written and activated by `configure`.** Copilot's file exporter appends to the spool and
stops; nothing in VS Code ships it. Without this the exporter `configure` just turned on would
fill a disk and export nothing, which from inside the editor looks exactly like a working
install.

Rewritten on every `configure`, so edits are lost. `configure` with **no** `--otel-endpoint`
removes them and stops the timer — the same retraction rule the config keys follow.

The units carry every flag explicitly (`--issuer`, `--client-id`, `--otel-endpoint`,
`--copilot-spool-path`) rather than relying on the config file the same `configure` writes.
Both work today; only the explicit form keeps working after someone edits that file, and a
wake failing every five minutes because a key moved is precisely the silent failure this
exists to remove.

Removing them by hand:

```bash
systemctl --user disable --now governance-auth-copilot-push.timer
```

```bash
launchctl bootout gui/$(id -u)/digital.camer.ai.governance-auth.copilot-push
```

⚠️ **A machine with no user systemd session** — a container, WSL without systemd, a CI runner
— gets a warning, not a failed `configure`. The config files are already written and
`copilot push` still runs by hand.

`governance-auth status` carries a **copilot drain** row for exactly this: a schedule that was
never activated, or that stopped, is otherwise invisible.

### Log

```
~/.local/state/governance-auth/logs/governance-auth.log            # Linux ($XDG_STATE_HOME honoured)
~/Library/Logs/governance-auth/governance-auth.log                 # macOS
```

Every command appends here, at `info` by default, `0600`. Not a second copy of the UX: stderr
stays the place a human reads while `login` runs, and this is the place anyone reads
afterwards — `token` and `otel headers` are spawned by Claude Code and Codex with their stderr
swallowed, and the drain wakes on a timer with nobody watching at all. `GOVERNANCE_AUTH_LOG`
raises or lowers it independently of `RUST_LOG`, which still controls stderr alone.

**Linux is `$XDG_STATE_HOME`** because the XDG basedir spec names "actions history (logs, …)"
as an example of what belongs there, so this is one segment below the session's own directory
and inherits its `0700`. **macOS is `~/Library/Logs`** because that is what Console.app reads
and where the launchd agent already wrote.

⚠️ **Bounded, unlike the Copilot spool above.** Past 1 MiB the live file is rotated to
`.log.1`, `.log.1` to `.log.2`, and so on for 3 generations — **4 MiB, for ever**. Rotation is
**copy-truncate**, not rename: launchd and any concurrently running `governance-auth` hold
open handles on this file, and renaming it would leave all of them writing into `.log.1` while
the file everyone reads stays empty. Truncation keeps the inode, and every writer is
`O_APPEND`, so they resume at zero rather than leaving a zero-filled hole. Two processes
cannot rotate at once — the same lock the session uses guards it.

**No secret is ever written here.** A token on stderr is gone when the terminal scrolls; a
token in this file is a credential at rest. `tests/logging_redaction.rs` runs the real binary
at `trace` with a sentinel token and fails if it appears in the file.

### Shell environment

```
~/.config/governance-auth/otel.env     # POSIX shells
~/.config/governance-auth/otel.fish    # fish
```

Both at mode `0600`. They carry exactly three things — `GOVERNANCE_AUTH_ISSUER`,
`GOVERNANCE_AUTH_CLIENT_ID`, and (with `--gateway-url`) `ANTHROPIC_BASE_URL` — so this binary
works from any terminal with no flags and a subprocess that does not inherit them can still
resolve.

⚠️ **No OTLP configuration is written here, and that is deliberate.** There is one collector
per audience: `otel.ai.camer.digital`'s OIDC gate accepts `governance-auth-cli` only, and
`otel-opencode.ai.camer.digital`'s accepts `opencode-cli` only. `OTEL_EXPORTER_OTLP_ENDPOINT`
and its siblings are *generic* OpenTelemetry variables — once sourced from an rc file they
apply to every OTLP exporter on the machine, and SDKs read the environment **ahead of** their
own configured default. So exporting one client's endpoint machine-wide makes every other
client's correct default unreachable. That is not hypothetical: on 2026-09-02 OpenCode
(`@vymalo/opencode-otel`, which resolves `env.OTEL_EXPORTER_OTLP_ENDPOINT || opts.endpoint`)
silently exported to the Claude Code collector on every machine that had run
`governance-auth`, and every span `401`'d. The endpoint is per-client, so it lives in each
client's own config file: `~/.claude/settings.json`, `~/.codex/config.toml`, and VS Code's
`settings.json`.

No credential goes in either file now — the OTLP bearer went there to authenticate the
endpoint that is no longer exported. They stay `0600`, and the rc file still only gets a
one-line `source`, because `.bashrc` is routinely mode `0644` and routinely committed to a
dotfiles repo.

**A machine configured by an older build is fixed by the next run**: both files are rewritten
wholesale and the rc block is replaced between its markers, so the stale `OTEL_*` exports
disappear without anyone editing a dotfile by hand.

### Config files

Read, never written: see [`configuration.md`](./configuration.md).

---

## Shell rc files

`~/.bashrc`, `~/.zshrc`, `~/.profile`, `~/.bash_profile`, `~/.config/fish/config.fish`

**Only files that already exist are edited.** Creating a `.zshrc` for someone who doesn't run
zsh changes which startup path their shell takes.

The edit is a single line, wrapped in markers:

```sh
# >>> governance-auth otel (managed) >>>
[ -f "$HOME/.config/governance-auth/otel.env" ] && . "$HOME/.config/governance-auth/otel.env"
# <<< governance-auth otel (managed) <<<
```

Everything between the markers is replaced wholesale on each run; everything outside is never
touched. Without markers the only idempotent options are "append every time" (the block grows
forever) or "rewrite the file" (destroys the developer's own config). The path is rendered as
`$HOME/…` so the line stays correct in a dotfiles repo shared between machines with different
usernames.

The block sources a file that carries **no** `OTEL_*` variable. `OTEL_EXPORTER_OTLP_ENDPOINT`
and `OTEL_EXPORTER_OTLP_HEADERS` are global OpenTelemetry variables — once exported, every
OTLP exporter started from that shell picks them up, including clients this binary does not
configure and whose collector expects a different audience. See the warning under
[Shell environment](#shell-environment) above.

---

## Claude Code — `~/.claude/settings.json`

Keys owned, in the `env` block and at the root:

| Key | Written when | Value |
|---|---|---|
| `apiKeyHelper` (root) | `--gateway-url` set | `<abs path> token …` |
| `otelHeadersHelper` (root) | `--otel-endpoint` set | `<abs path> otel headers …` |
| `ANTHROPIC_BASE_URL` | `--gateway-url` set | `<gateway>/anthropic` |
| `CLAUDE_CODE_API_KEY_HELPER_TTL_MS` | always | the debounce value |
| `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` | always | `1` |
| `CLAUDE_CODE_ENABLE_TELEMETRY` | `--otel-endpoint` set | `1` |
| `OTEL_METRICS_EXPORTER` / `OTEL_LOGS_EXPORTER` | `--otel-endpoint` set | `otlp` |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `--otel-endpoint` set | `http/protobuf` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `--otel-endpoint` set | the collector base |
| `OTEL_RESOURCE_ATTRIBUTES` | `--otel-endpoint` set | `service.namespace=ai-cli,user.email=…,user.id=…,user.name=…` |
| `CLAUDE_CODE_OTEL_HEADERS_HELPER_DEBOUNCE_MS` | helper in use | the debounce value |
| `OTEL_EXPORTER_OTLP_HEADERS` | **only** when no helper | `Authorization=Bearer …` |

Claude Code appends `/v1/messages` to `ANTHROPIC_BASE_URL` itself, which is why the value
ends at `/anthropic`.

**The helper wins outright over the static header.** When `otelHeadersHelper` is written, the
static `OTEL_EXPORTER_OTLP_HEADERS` is *removed* rather than left in place. A stale static
value sitting alongside a refreshing one, and silently winning, is the exact failure this
whole mechanism exists to remove.

`CLAUDE_CODE_API_KEY_HELPER_TTL_MS` matters because Claude Code caches helper output for five
minutes by default — the exact lifetime of an access token here — so the cache can hand it a
token that expired moments ago. Claude Code re-runs the helper on a 401, so this self-heals,
but only *after* a failed request. Keeping the TTL under the token lifetime avoids the
failure instead of recovering from it.

**Setting the TTL below the token lifetime is necessary but not sufficient**, which is the
half that had to be learnt in production (2026-09-02, ~30 `oidc: token is expired` rejections
per 15-minute token). What matters is not the age of the token when it is handed over but
whether it survives to the *end* of the window it is handed into. A 900s token with 31s left
passed the old freshness check — "more than the 30s clock skew" — and was then sent by Claude
Code for the next 240s, dead for 209 of them.

So the rule these two variables imply is enforced on this side as well: `token` and `otel
headers` refresh whenever the cached session has less than **the debounce plus the 30s skew**
(270s at the default) of life left, so anything they print outlives the cache it is going
into. `crate::freshness` owns that decision; `copilot push` and `status`, which use the token
themselves and hand it to nothing that caches it, keep the plain 30s skew.

`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` exists because this gateway serves model names
Claude Code doesn't ship in its built-in list; without discovery they never appear in the
`/model` picker at all. It does **not** silence the "not a model this version recognizes"
warning — that one is about the assumed 200k context window and is only addressed by
`modelOverrides` or `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, both of which would mean hard-coding
each gateway model's real window into this binary, which has no way to know it and would
silently rot. That belongs in the values repo, where the model list already lives.

---

## Codex — `~/.codex/config.toml`

Edited through `toml_edit` rather than parse-and-reserialize, so existing comments and key
order survive — this file is hand-maintained.

**`[otel]`** — written only when `--otel-endpoint` is set:

```toml
[otel]
environment = "prod"
log_user_prompt = false

[otel.exporter.otlp-http]
endpoint = "https://otel.example"
protocol = "binary"
[otel.exporter.otlp-http.headers]
Authorization = "Bearer …"

# …and the same shape again under [otel.metrics_exporter.otlp-http]
```

⚠️ `otel.exporter` is a **tagged enum**, not a string: the exporter kind is the table *name*
and its settings are that table's contents. Writing `exporter = "otlp-http"` with the settings
in a sibling table parses as valid TOML but Codex rejects it at load time — and Codex refuses
to start at all on a config it can't load, so getting this wrong bricks the tool rather than
just disabling telemetry. The shape above was confirmed by loading it in codex-cli 0.146.1,
not inferred from the reference docs.

`log_user_prompt = false`: the collector's own redaction is the authoritative control, but a
client that never sends raw prompts is one fewer place they can leak.

**`[model_providers.governance]`** — written only when `--gateway-url` is set. A stable
provider id, so re-running `configure` updates the same block instead of accumulating one per
run; any differently-named provider written by hand is left strictly alone.

```toml
[model_providers.governance]
name = "governance"
base_url = "https://api.example/v1"
wire_api = "responses"

[model_providers.governance.auth]
command = "/abs/path/to/governance-auth token …"
refresh_interval_ms = 240000
```

Two traps, both measured live rather than inferred:

- **The command must be an absolute path.** Codex spawns it directly rather than through a
  shell, so it does not inherit the login shell's `PATH`. A bare `governance-auth` fails with
  `No such file or directory (os error 2)` and the provider silently falls back to
  unauthenticated. Claude Code happens to resolve a bare name because it goes through a
  shell — which is exactly why this trap shows up on only one of the two clients, and why
  both are built from the same absolute-path helper.
- **`wire_api = "responses"` is the only accepted value.** `wire_api = "chat"` is rejected
  outright at config load ("no longer supported").

⚠️ The provider block is written with a comment saying it is **inert** until the gateway
serves `/v1/responses`: codex-cli requires `wire_api = "responses"` and this gateway
implements `/v1/chat/completions` (verified — `/v1/responses` 404s upstream,
`/v1/chat/completions` returns 200). The auth wiring is correct and tested; only the endpoint
is missing. See [#144](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/144).

---

## VS Code Copilot — `<flavour>/User/settings.json`

Flavours checked: `Code`, `Code - Insiders`, `VSCodium`. Under `~/.config/` on Linux and
`~/Library/Application Support/` on macOS — VS Code does not follow `XDG_CONFIG_HOME` on
macOS.

```json
"github.copilot.chat.otel.enabled": true,
"github.copilot.chat.otel.exporterType": "file",
"github.copilot.chat.otel.outfile": "/home/you/.local/state/governance-auth/copilot-otel.jsonl",
"github.copilot.chat.otel.captureContent": false
```

⚠️ **The exporter is `file`, not `otlp-http`, and that is the point.** Copilot's direct HTTP
exporter has no header this binary is willing to write. `github.copilot.chat.otel.headers`
exists, but it is a *static* map and `settings.json` is covered by Settings Sync — writing a
bearer there syncs it off-machine. The `otlp-http` this used to write carried no header at
all, so an authenticating collector returned **401 on every span** while the config looked
complete.

The file exporter has neither problem. Copilot appends to `outfile`;
[`copilot push`](./commands.md#copilot-push) drains it on the schedule `configure` installs,
authenticating with a bearer it refreshes itself. That makes Copilot the **second**
self-renewing client after Claude Code, and leaves Codex as the only one still needing a
long-lived `--otel-token`.

`outfile` is written as an absolute path, resolved once, and the same resolved path is passed
to the drain as `--copilot-spool-path`. They cannot disagree — which they could, and did, when
both were copy-pasted out of a runbook.

⚠️ **Upgrading from a build that wrote `otlp-http`** leaves `github.copilot.chat.otel.otlpEndpoint`
behind. One `configure` removes it: the key is in the managed-key manifest, so
the managed-key manifest retracts it — but only if its value still hashes to what we
wrote, so a developer who edited it keeps their edit.

**A JSONC file is refused, not rewritten.** VS Code's `settings.json` legally contains
comments and trailing commas, and developers really do use them. `serde_json` can't parse
that, and both tempting fixes are destructive. So a file that can't be round-tripped losslessly
is left untouched and the exact settings are printed for the developer to paste. Declining to
edit beats silently eating someone's annotated config.

---

## Resource attributes

`OTEL_RESOURCE_ATTRIBUTES` carries a fixed `service.namespace=ai-cli`, plus `user.id`,
`user.email` and `user.name` read from the `sub`, `email` and `preferred_username` claims of
the access token.

The signature is **deliberately not verified** when reading them, and these values must never
inform an authorization decision. The token came from the token endpoint over TLS moments
earlier and is only being read to label this machine's own outgoing telemetry; the collector
re-derives trusted identity itself and never trusts these attributes — RFC-0002's trust
boundary is that tenant context comes from the authenticated credential, never from the
payload body. A token shaped differently, or one that isn't a JWT at all, yields no
attributes rather than an error: failing `login` over a cosmetic label would be the wrong
trade.

Attributes are rendered from a sorted map so the output is deterministic. An unstable
ordering would make every `login` rewrite the config with a spurious diff.
