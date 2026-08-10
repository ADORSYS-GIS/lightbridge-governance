# AI client support matrix

What `governance-auth` can and cannot configure for each client, and why.
Every "no" here was **measured against the real client or the real
endpoint**, not inferred from documentation — several of them contradict what
the docs imply.

Last verified: 2026-08-10, against Claude Code 2.1.223, codex-cli 0.146.1,
and `api.ai.camer.digital`. The opencode column is read from the org's own
working configuration (`ai-helm` `charts/librechat-opencode-wellknown/
values.yaml`), not from opencode's docs — it is already in production use
against this gateway.

## Matrix

| Capability | Claude Code | Codex CLI | opencode | GitHub Copilot (VS Code) |
|---|---|---|---|---|
| **Inference endpoint** | ✅ `ANTHROPIC_BASE_URL` | ⚠️ `model_providers.*` — blocked, see below | ✅ `provider.<id>.options.baseURL` | ❌ no supported override |
| **Inference auth** | ✅ `apiKeyHelper`, refreshes | ✅ `auth.command`, `refresh_interval_ms` | ✅ **full OAuth2 + refresh**, via `opencode-oauth2` | ❌ |
| **Telemetry endpoint** | ✅ `OTEL_EXPORTER_OTLP_ENDPOINT` | ✅ `otel.exporter.otlp-http.endpoint` | ❌ no OTEL support | ✅ `github.copilot.chat.otel.otlpEndpoint` |
| **Telemetry auth, refreshing** | ✅ `otelHeadersHelper` | ❌ static only | ❌ n/a | ❌ static only |
| **Telemetry auth, static** | ✅ | ✅ `otel.exporter.otlp-http.headers` | ❌ n/a | ⚠️ env var only — no setting exists |
| **Model context windows** | ✅ `modelOverrides` (not yet wired) | — | ✅ **already consumes `/v1/models/info`** | — |
| **Config file is safely mergeable** | ✅ JSON | ✅ TOML via `toml_edit` | ⚠️ JSONC — same hazard as VS Code | ⚠️ JSONC — refused if it has comments |

✅ works · ⚠️ works with a caveat · ❌ no mechanism exists

## The caveats, in order of how much they bite

### Codex cannot reach this gateway at all

Not an auth or config problem. codex-cli 0.146.1 **requires**
`wire_api = "responses"` and no longer accepts `"chat"` — it refuses to load
a config containing it. This gateway (Envoy AI Gateway) implements
`/v1/chat/completions` and `/anthropic/v1/messages`; `/v1/responses` is a
confirmed 404, and the Responses API is not well supported upstream.

So the provider block is written but **inert**. Nothing in `governance-auth`
can fix this; it needs either gateway support for `/v1/responses` or a Codex
version that accepts chat-completions again.

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

### VS Code Copilot has no setting for OTLP auth headers

Endpoint, protocol and content-capture are settings; authentication is
`OTEL_EXPORTER_OTLP_HEADERS` only. Hence the shell-rc wiring — and note that
variable is **global**: every OTLP exporter started from that shell attaches
the header to whatever collector it targets. Copilot's `COPILOT_OTEL_*`
overrides cover endpoint/protocol/capture but not headers, so this is
inherent to authenticating Copilot at all.

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
verified live, the warning still prints. Discovery populates the `/model`
picker; the window comes from `modelOverrides` or
`CLAUDE_CODE_MAX_CONTEXT_TOKENS`.

### opencode is the most complete client, and it is not configured here

Its `opencode-oauth2` plugin does a full OAuth2 **device-code** flow against
the same Keycloak realm `governance-auth` uses, with its own refresh loop
(`syncIntervalMinutes: 60`) — so for inference it needs nothing from
`governance-auth` at all. Config lives at
`~/.config/opencode/opencode.json`; the org's working shape is in `ai-helm`
`charts/librechat-opencode-wellknown/values.yaml`:

```jsonc
"provider": { "camer-digital": { "options": {
  "baseURL": "https://api.ai.camer.digital/v1",
  "oauth2": { "issuer": "https://auth.verif.fyi/realms/camer-digital",
              "clientId": "opencode-cli", "authFlow": "device_code" },
  "meta":   { "modelsInfoUrl": "models/info" } } } }
```

**It has no OpenTelemetry support** — not a config gap, the feature doesn't
exist. Its plugin system is the only seam (the same seam `opencode-oauth2`
uses), so instrumenting it means writing a plugin, not writing config.
That's why `governance-auth` does not configure opencode today: the half it
could help with is already solved better, and the half we want (telemetry)
isn't configurable at all.

⚠️ Its config is **JSONC**, so anything editing it faces the same
comment-preservation hazard as VS Code's `settings.json` — and the org's own
file is heavily commented.

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

- **Claude Code — complete.** Inference and telemetry both work, both
  credentials refresh, verified end-to-end against the live gateway.
- **opencode — inference complete, telemetry impossible.** Already in
  production with its own OAuth2 device-code refresh and its own
  `/v1/models/info` consumption; OTEL would need a plugin.
- **Codex — telemetry config only.** Inference blocked on `/v1/responses`.
- **VS Code Copilot — telemetry only**, and its auth needs a shell env var.

The two clients that solved model metadata (opencode) and telemetry auth
(Claude Code) did it in completely different ways, which is the argument for
reading model windows from `/v1/models/info` rather than hard-coding them:
opencode already treats that endpoint as the catalogue, so Claude Code
picking up the same source keeps one source of truth rather than two.
