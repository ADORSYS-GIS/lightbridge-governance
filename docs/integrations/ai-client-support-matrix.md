# AI client support matrix

What `governance-auth` can and cannot configure for each client, and why.
Every "no" here was **measured against the real client or the real
endpoint**, not inferred from documentation — several of them contradict what
the docs imply.

Last verified: 2026-08-11, against Claude Code 2.1.223, codex-cli 0.146.1,
and `api.ai.camer.digital`, re-run end-to-end inside the `lgb-claude` /
`lgb-codex` multipass VMs. The opencode column is read from the org's own
working configuration (`ai-helm` `charts/librechat-opencode-wellknown/
values.yaml`), not from opencode's docs — it is already in production use
against this gateway.

**Sequence diagrams for every row below live in
[`ai-client-flows.md`](ai-client-flows.md)** — this file says *whether* a
capability works per client; that one says *how*, and exactly where it breaks.

## Matrix

| Capability | Claude Code | Codex CLI | opencode | GitHub Copilot (VS Code) |
|---|---|---|---|---|
| **Inference endpoint** | ✅ `ANTHROPIC_BASE_URL` | ⚠️ `model_providers.*` — blocked, see below | ✅ `provider.<id>.options.baseURL` | ❌ no supported override |
| **Inference auth** | ✅ `apiKeyHelper`, refreshes | ⚠️ `auth.command` — needs an ABSOLUTE path, see below | ✅ **full OAuth2 + refresh**, via `opencode-oauth2` | ❌ |
| **Written by `governance-auth configure`** | ✅ with `--gateway-url` | ✅ with `--gateway-url`, and set as the **default** provider | ❌ not configured here | ⚠️ telemetry only |
| **Telemetry endpoint** | ✅ `env.OTEL_EXPORTER_OTLP_ENDPOINT` in `settings.json` | ✅ `otel.exporter.otlp-http.endpoint` | ✅ `@vymalo/opencode-otel`, its OWN collector — see below | ✅ **not used** — `exporterType: file` + `outfile`, drained by `copilot push` |
| **Telemetry auth, refreshing** | ✅ `otelHeadersHelper` | ❌ static only | ❌ n/a | ✅ **out of band** — Copilot holds no credential; the drain refreshes its own |
| **Telemetry auth, static** | ✅ | ✅ `otel.exporter.otlp-http.headers` | ❌ n/a | n/a — deliberately not used, see below |
| **Model context windows** | ✅ `modelOverrides` (not yet wired) | — | ✅ **already consumes `/v1/models/info`** | — |
| **Config file is safely mergeable** | ✅ JSON | ✅ TOML via `toml_edit` | ⚠️ JSONC — same hazard as VS Code | ⚠️ JSONC — refused if it has comments |

✅ works · ⚠️ works with a caveat · ❌ no mechanism exists

## The caveats, in order of how much they bite

### The endpoint is per-client, so it is never a shell variable

Each collector's OIDC gate accepts exactly **one** audience:
`otel.ai.camer.digital` takes `governance-auth-cli`, and
`otel-opencode.ai.camer.digital` (chart values `opencodeOtel`) takes
`opencode-cli`. So "the collector" is not one address — it is one per client.

`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`,
`OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER` and
`OTEL_RESOURCE_ATTRIBUTES` are **generic** OpenTelemetry variables. Exported from
a shell rc they reach every OTLP exporter on the machine, and SDKs consult the
environment *before* their own configured default — so a machine-global value
silently overrides each client's correct one.

Measured 2026-09-02: `governance-auth` wrote the generic set into
`~/.config/governance-auth/otel.env`, sourced from every rc file. OpenCode's
plugin resolves `env.OTEL_EXPORTER_OTLP_ENDPOINT || opts.endpoint`, so on every
machine that had ever run `governance-auth` it started with
`endpoint=https://otel.ai.camer.digital`, logged `otel_export_failed status=401`
on each export, and left 112 `token verification failed` lines on the *other*
collector in 25 minutes.

**`governance-auth` now writes no `OTEL_*` variable at all.** Every client it
configures has its own file for telemetry: Claude Code
`~/.claude/settings.json`, Codex `[otel]` in `~/.codex/config.toml`, Copilot its
file exporter. opencode is not configured here and keeps whatever its well-known
document pins — the per-signal `endpoints` that were pinned as the immediate
unblock are no longer load-bearing against this binary, though they remain the
more precise thing to pin.

### A command written into a config must be an ABSOLUTE path

Codex spawns `[model_providers.*.auth] command` **itself, not through a
shell**, so it never inherits the login shell's `PATH`. With
`governance-auth` installed to `~/.local/bin` (the documented location), a
bare command name fails:

```
ERROR codex_login::auth::manager: Failed to resolve external auth: provider auth
command `governance-auth --issuer … token` failed to start:
No such file or directory (os error 2)
```

Codex then proceeds **unauthenticated** rather than stopping, so this reads
as a confusing downstream API error, not as "the helper never ran."

Claude Code resolves a bare name fine — it goes through a shell — so this
trap is completely invisible if you only test that client. `governance-auth`
now builds every command it writes from `otel::binary_path()`
(`std::env::current_exe()`), pinned by two tests: one on the writer, one on
`binary_path` itself, because the writer test alone still passes if
`binary_path` regresses to its bare-name fallback.

### Codex cannot reach this gateway at all

Not an auth or config problem. codex-cli 0.146.1 **requires**
`wire_api = "responses"` and no longer accepts `"chat"` — it refuses to load
a config containing it.

Measured 2026-08-11, and the failure is split across two layers:

| Request | Result |
|---|---|
| `POST /v1/chat/completions` | **200** |
| `POST /v1/responses` (well-formed) | **404** `{"detail":"Not Found"}` — from **upstream** |
| `POST /v1/responses` (Codex's own body) | **400** `malformed request: … unknown tool type` — from **Envoy** |

So Envoy AI Gateway *does* route `/v1/responses` and tries to translate it
(hence its own 400 on the tool schema), but the **upstream model backend
doesn't implement it** and 404s. Earlier notes here said "the gateway 404s
it"; that's imprecise — the gateway routes it, the upstream refuses it. The
practical outcome is unchanged: Codex inference is blocked.

⚠️ **Re-probed 2026-08-31 and the gateway side has changed.** `POST /v1/responses` now
returns **401**, while near-miss paths (`/v1/responses-nope`, `/v1/respons`) return **404** —
so the route is served and auth-gated rather than absent. `governance-auth` therefore now sets
this provider as Codex's default (`model_provider`).

The **upstream** half below was NOT re-verified: a `401` is returned before upstream is
reached, so an unauthenticated probe cannot reproduce the well-formed-body case. Treat the
paragraph that follows as the last confirmed upstream behaviour, not as current fact — it needs
one authenticated request to settle.

Historically: the provider block was written but **inert**, and `governance-auth` marked
it as such with an inline comment when it writes one. Nothing in
`governance-auth` can fix this; it needs either upstream support for
`/v1/responses` or a Codex version that accepts chat-completions again.

### Codex's `otel.exporter` is a tagged enum, and getting it wrong bricks Codex

`exporter = "otlp-http"` is valid TOML and matches the published reference.
codex-cli rejects it (`invalid type: unit variant, expected struct variant`)
and then **refuses to start** — so the mistake doesn't disable telemetry, it
takes the tool out until someone hand-edits the file. The accepted shape:

```toml
[otel.exporter.otlp-http]
endpoint = "https://otel.ai.camer.digital"
```

Pinned by a unit test (`codex_exporter_is_a_struct_variant_not_a_bare_string`).

### `otelHeadersHelper` beats `OTEL_EXPORTER_OTLP_HEADERS` — the env var is ignored

Measured 2026-08-11 in `lgb-claude`: with `otelHeadersHelper` present in
`settings.json`, running Claude Code with `OTEL_EXPORTER_OTLP_HEADERS` set
to a **valid** lightbridge-authz key still exported using the **helper's**
credential — the collector logged
`issuer: https://auth.verif.fyi/realms/camer-digital` for that run, i.e. the
Keycloak token, and rejected it.

Consequences, both load-bearing:

- The static-token workaround **cannot** rescue Claude Code telemetry while
  the helper is configured. The helper must be *removed* from
  `settings.json` for a static key to take effect — an env var alone does
  nothing. This constrains how [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84)
  can be fixed.
- `governance-auth`'s existing "delete the static header when a helper is
  set" behaviour is aligned with the client's real precedence, not merely
  tidy — leaving both would be genuinely misleading, since the one you can
  see in `env` is the one being ignored.

### VS Code Copilot cannot hold a refreshing OTLP credential, so it holds none

`github.copilot.chat.otel.headers` **does** exist (verified live 2026-08-31,
correcting RFC-0003's *Risks* section), but it is a **static** map and
`settings.json` is covered by Settings Sync — a long-lived bearer written there
syncs off-machine. The other channel, `OTEL_EXPORTER_OTLP_HEADERS`, is a
**global** OpenTelemetry variable that a desktop-launched VS Code never sees
anyway.

So neither is used. `configure` writes `exporterType: "file"` + `outfile`
instead, and `governance-auth copilot push` — on a systemd user timer or a
launchd agent that `configure` installs — ships the spool with a bearer it
refreshes per wake. Copilot never holds a credential, which removes the problem
rather than choosing between two bad answers to it.

⚠️ The cost is a spool file nothing bounds
([#230](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/230)),
measured growing 73 KB → 315 KB in six minutes of ordinary use.

### A JSONC `settings.json` is refused, not rewritten

VS Code's settings file legally allows comments and trailing commas.
`serde_json` can't represent them, and stripping them to parse would delete a
developer's annotations permanently. `governance-auth` declines and prints
the settings to add by hand; the file comes back byte-for-byte identical.

### Claude Code's assumed context window is wrong for this gateway

Claude Code assumes **200 000** tokens for a model it doesn't recognise.
`GET /v1/models/info` reports `adorsys-coder` at **196 608**. The assumption
is *larger* than reality, so auto-compact would let a session run past what
the model actually accepts.

`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` does **not** fix this —
verified live again 2026-08-11, the warning still prints. Discovery
populates the `/model` picker; the window comes from `modelOverrides` or
`CLAUDE_CODE_MAX_CONTEXT_TOKENS`. Claude Code 2.1.223 also suggests a
`[1m]` model-name suffix and
`CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT=1` in the warning
itself; neither is written here, for the same reason as before — the real
window belongs to the gateway, not hard-coded into a binary.

⚠️ **You must pass `--model` explicitly.** Claude Code's built-in default
(`claude-opus-5[1m]`) does not exist on this gateway, and the failure is a
hard stop, not a fallback:

```
There's an issue with the selected model (claude-opus-5[1m]).
It may not exist or you may not have access to it.
```

`claude --model adorsys-coder -p "…"` works. Wiring `modelOverrides` from
`GET /v1/models/info` would fix both this and the window assumption at once.

### opencode is the most complete client, and it is configured through its well-known document, not here

Its `@vymalo/opencode-oauth2` plugin does a full OAuth2 **device-code** flow
against **lightbridge's own IdP, `authz-idp`** — not Keycloak — with its own
refresh loop (`syncIntervalMinutes: 60`), so for inference it needs nothing
from `governance-auth` at all. The org's configuration is not a local file:
opencode fetches `https://ai.camer.digital/opencode/.well-known/opencode` at
every launch, and that document is rendered from `ai-helm`
`charts/librechat-opencode-wellknown/values.yaml`. Its working shape, as of
2026-09-02:

```jsonc
"provider": { "camer-digital": { "options": {
  "baseURL": "https://api.ai.camer.digital/v1",
  "oauth2": { "issuer": "https://auth.ai.camer.digital",
              "clientId": "opencode-cli", "authFlow": "device_code",
              "scopes": ["openid", "profile", "email", "offline_access"] },
  "meta":   { "modelsInfoUrl": "models/info" } } } }
```

The issuer matters for telemetry: `authz-idp` stamps `aud` = the requesting
`client_id` (lightbridge-authz ADR-0011 Decision 5), so every opencode token
carries `aud: opencode-cli`. An earlier revision of this page recorded the
Keycloak realm as the issuer; it was wrong by the time it was written, and a
live collector rejection (`expected audience "governance-auth-cli" got
["opencode-cli"]`, which the oidc extension only reaches after issuer and
signature have already validated) proved the token comes from
`auth.ai.camer.digital`.

**It has OpenTelemetry support**, through the org-published
`@vymalo/opencode-otel` plugin in that same well-known document: traces,
metrics and logs over OTLP/HTTP, no prompt or response content. Two things
about how it is wired are easy to get wrong:

- It exports to the **second** public collector, `otel-opencode.ai.camer.digital`
  (`aud: opencode-cli`), because the Claude Code collector accepts only
  `governance-auth-cli` — see "Two public collectors, one audience each" in
  [`ai-client-flows.md`](./ai-client-flows.md). The document pins the
  **per-signal** `endpoints`, deliberately not the base `endpoint`: the plugin
  resolves the base as `env.OTEL_EXPORTER_OTLP_ENDPOINT || opts.endpoint`, and
  `governance-auth` writes that generic variable machine-wide, so a base
  endpoint silently lost on every machine that had run `governance-auth`
  (observed 2026-09-02: opencode exporting to `otel.ai.camer.digital` and
  401ing on every push).
- Its credential is a `tokenCommand` helper that only **reads** the access
  token `@vymalo/opencode-oauth2` already persisted and never refreshes — a
  second process presenting a single-use refresh token trips `authz-idp`'s
  reuse cascade and logs the user out (lightbridge-opencode-toolbeit#104).

That is why `governance-auth` still does not configure opencode: both halves
are already solved by the well-known document, and the one thing
`governance-auth` could do to it — export a global OTLP endpoint — has now
stopped. `shell_exports` (`app/governance-auth/src/otel.rs`) writes no `OTEL_*`
variable; see the first caveat on this page.

⚠️ A developer's local `~/.config/opencode/opencode.json` is **JSONC**, so
anything editing it faces the same comment-preservation hazard as VS Code's
`settings.json`.

### opencode independently corroborates the Codex blocker

That config carries `responseApi: false`, with the reason recorded inline:
routing through the **Responses API** required SSE index-repair for
`output_index`/`content_index` fields "our Envoy AI Gateway omits", and it
was enabled then reverted.

So the gateway's Responses-API support is known-incomplete from a second,
independent direction — not just the 404 measured here. Codex 0.146.1
*requires* that path, which is why it can't be made to work by configuration.

## `GET /v1/models/info` — the source for model metadata

Auth-gated (401 without a token), returns 19 models. Per entry:

```json
{
  "id": "adorsys-coder",
  "name": "Adorsys Coder (MiniMax M2.7)",
  "context_length": 196608,
  "top_provider": { "context_length": 196608, "max_completion_tokens": 131072 },
  "supported_parameters": ["tools", "tool_choice", "reasoning", "structured_outputs", "..."],
  "pricing": { "prompt": "0.0000003750", "completion": "0.0000015000", "input_cache_read": "0.0000000750" }
}
```

This is enough to drive, per model:

- `modelOverrides` context window ← `context_length`
- `CLAUDE_CODE_MAX_OUTPUT_TOKENS` ← `top_provider.max_completion_tokens`
- tool-use support ← `supported_parameters` containing `tools`

⚠️ **Not yet wired into `governance-auth`.** It is the right source — it
means the windows are read from the gateway rather than hard-coded into a
binary where they would silently rot as models change.

## What each client is actually usable for today

- **Claude Code — inference complete, telemetry blocked.** Re-verified
  2026-08-11 in `lgb-claude`: an expired (22h old) session refreshed
  silently, `claude -p` returned a real answer through the gateway, and a
  direct `POST /anthropic/v1/messages` returned 200. **Telemetry 401s** —
  `otel headers` mints a Keycloak token while the collector validates
  against lightbridge-authz
  ([#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84),
  now confirmed live rather than predicted; the collector logs the wrong
  issuer by name). The collector itself is healthy: the same push with a
  lightbridge-authz key returns 200 and reaches Alloy.
- **opencode — inference complete, telemetry impossible.** Already in
  production with its own OAuth2 device-code refresh and its own
  `/v1/models/info` consumption; OTEL would need a plugin.
- **Codex — telemetry config only.** Inference blocked upstream on
  `/v1/responses`. Its telemetry path is static-token, so unlike Claude
  Code it is *not* blocked by
  [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84) —
  a lightbridge-authz key in `otel.exporter.otlp-http.headers` is accepted
  by the collector today (verified: 200, span reached Alloy).
- **VS Code Copilot — telemetry only**, via the file exporter plus a drain
  `configure` schedules. It is the only client whose telemetry credential lives
  entirely outside the editor, which is also why it is the only one Settings
  Sync cannot leak.

The two clients that solved model metadata (opencode) and telemetry auth
(Claude Code) did it in completely different ways, which is the argument for
reading model windows from `/v1/models/info` rather than hard-coding them:
opencode already treats that endpoint as the catalogue, so Claude Code
picking up the same source keeps one source of truth rather than two.
