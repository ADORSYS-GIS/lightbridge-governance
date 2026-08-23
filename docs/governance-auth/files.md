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

### OTLP credential for the shell

```
~/.config/governance-auth/otel.env     # POSIX shells
~/.config/governance-auth/otel.fish    # fish
```

Both at mode `0600`. These exist for one reason: VS Code Copilot has **no settings key for
OTLP headers** (see below), so the only way to authenticate it is an exported environment
variable.

The token deliberately does **not** go into `.bashrc`. Those files are routinely mode `0644`
and routinely committed to a dotfiles repo — writing a bearer token there is how a credential
ends up in someone's public GitHub. The secret lives in the `0600` file and the rc file gets
a one-line `source` of it, so a committed `.bashrc` leaks nothing.

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

⚠️ `OTEL_EXPORTER_OTLP_HEADERS` is a **global** OpenTelemetry variable, not a Copilot-scoped
one. Once exported, every OTLP exporter started from that shell attaches this `Authorization`
header to whatever collector it targets. VS Code offers no scoped alternative, so this is
inherent to authenticating Copilot at all rather than a choice made here — but it should be
said out loud to anyone being onboarded.

---

## Claude Code — `~/.claude/settings.json`

Keys owned, in the `env` block and at the root:

| Key | Written when | Value |
|---|---|---|
| `apiKeyHelper` (root) | `--gateway-url` set | `<abs path> token …` |
| `otelHeadersHelper` (root) | `--otel-endpoint` set | `<abs path> otel-headers …` |
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
"github.copilot.chat.otel.exporterType": "otlp-http",
"github.copilot.chat.otel.otlpEndpoint": "https://otel.example",
"github.copilot.chat.otel.captureContent": false
```

⚠️ **There is no VS Code setting for OTLP headers.** The documented surface exposes endpoint,
protocol and content-capture as settings, but authentication *only* through the
`OTEL_EXPORTER_OTLP_HEADERS` environment variable, which must be present in the environment
VS Code itself was launched from. No `settings.json` key can supply it and neither can this
binary. Against an authenticating collector that means Copilot telemetry is rejected until
that variable is exported — which is what the `otel.env` + rc-file machinery above is for,
and why `login` says so out loud rather than writing a config that looks complete and
silently drops every span.

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
