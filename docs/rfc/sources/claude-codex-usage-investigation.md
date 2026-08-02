# Per-user usage telemetry for Claude Code and OpenAI Codex

**Investigation · 2026-08-02 · read-only research, no repo changes.**
Question: how can the adorsys-gis AI-governance platform (`ai-helm` + `lightbridge-governance`)
capture per-user usage/telemetry for developers' **Claude Code** and **OpenAI Codex** activity?

Every claim is either **[V]** verified against an official vendor doc / vendor source repo /
a repo file:line cited inline, or **[?]** explicitly flagged as unconfirmed. Nothing inferred
is presented as fact.

---

## 1. Executive summary

1. **Yes for both — and the cheapest route is one that neither #680 nor #679 proposes.** Both
   Claude Code and Codex ship a **native OpenTelemetry exporter** that pushes per-session token,
   cost and tool telemetry to *any* OTLP endpoint, and both stamp **`user.email`** on it. Claude
   Code's can be **enforced by admins** through managed settings.
2. **We have already decided to build the endpoint they'd push to.** `lightbridge-governance`
   ADR-0006 + RFC-0002 specify `otel.ai.camer.digital` — core-gateway listener, third
   host-indexed Authorino AuthConfig, per-token quota, an `OpenTelemetryCollector` fanning out
   to Tempo/Loki/Mimir + the governance API — for Microsoft Foundry. Claude Code and Codex are
   simply two more producers on it. Nothing in that path is Foundry-specific.
3. **Route 1 (make them gateway clients) is real but each product has one blocking unknown.**
   Claude Code: AIEG's Anthropic-format ingress is documented as routing only to
   *Anthropic-family* backends, and our `claude-sonnet-5` is a DeepInfra `schema: OpenAI` backend.
   Codex: `wire_api = "responses"` is now the **only** supported value, and this repo already
   documents that our `/v1/responses` SSE omits `output_index`/`content_index`.
4. **Route 2 (vendor usage APIs) is per-user for Anthropic** (`usage_report/claude_code`, daily,
   actor **email**, cost in cents) but is **mutually exclusive with Route 1** and has an open,
   5-month-unanswered bug where OAuth/subscription users never appear. For Codex it is weaker:
   OpenAI's platform Usage API does support `group_by=user_id` but never sees ChatGPT-seat Codex,
   and the Codex Analytics/Compliance APIs are **Enterprise/Edu-only** with a login-gated schema.
5. **The decisive constraint is contractual.** Anthropic explicitly forbids routing requests
   through Free/Pro/Max subscription credentials on behalf of users, and reserves the right to
   enforce "without prior notice". Any gateway route must front a **Console API key** — never
   proxied subscription OAuth tokens.

**One-line recommendation:** build the OTLP ingest first (Route 3 — covers both products, needs
no vendor contract, ~80% already decided), keep #680/#679 as the *authentication* tickets they
are, and treat vendor usage APIs as optional later reconciliation.

---

## 2. The four routes

The brief posed two. Research surfaced two more that are strictly cheaper.

| # | Route | Claude Code | Codex |
|---|---|---|---|
| 1 | **Gateway client** — point the tool at `api.ai.camer.digital`; existing per-user observability just works | Viable; blocked on a backend-schema question | Viable; blocked on a `/v1/responses` question |
| 2 | **Vendor usage-API pull** (Copilot-connector shape) | Yes — per-user by email, daily, free | Platform Usage API (`group_by=user_id`) for API-key Codex only; ChatGPT-seat Codex needs **Enterprise/Edu** Analytics/Compliance APIs, schema login-gated |
| 3 | **Native client OTLP push** → our own collector ⭐ | Yes, first-class, admin-enforceable | Yes, first-class |
| 4 | **Anthropic's own `claude gateway`** (self-hosted, Keycloak OIDC, OTLP fan-out, free) | Yes | N/A |

---

## 3. Claude Code

### 3.1 Route 1 — Claude Code as a gateway client

| Fact | | Evidence |
|---|---|---|
| Custom base URL: `ANTHROPIC_BASE_URL` | **[V]** | [llm-gateway-connect](https://code.claude.com/docs/en/llm-gateway-connect) |
| Credential → header: `ANTHROPIC_AUTH_TOKEN` → `Authorization: Bearer`; `ANTHROPIC_API_KEY` → `x-api-key`; `apiKeyHelper` → **both** | **[V]** | same page |
| **`apiKeyHelper`** runs a script; output cached 5 min (`CLAUDE_CODE_API_KEY_HELPER_TTL_MS`), re-run on HTTP 401 — a Keycloak token-minting script is directly supported | **[V]** | [authentication](https://code.claude.com/docs/en/authentication) |
| Auth precedence: cloud provider → `ANTHROPIC_AUTH_TOKEN` → `ANTHROPIC_API_KEY` → `apiKeyHelper` → `CLAUDE_CODE_OAUTH_TOKEN` → subscription login | **[V]** | same page |
| Endpoints called: `POST {base}/v1/messages?beta=true`; optional `POST {base}/v1/messages/count_tokens`; optional `GET {base}/v1/models?limit=1000` | **[V]** | [llm-gateway-protocol](https://code.claude.com/docs/en/llm-gateway-protocol) |
| **Only the Anthropic Messages format is supported** — there is no OpenAI-compatible gateway format | **[V]** | same page |
| Gateway must **stream**, must forward `anthropic-version` + `anthropic-beta` **verbatim as an open list**, must not rewrite request bodies or wrap error bodies | **[V]** | same page |
| Metering headers already sent: `x-claude-code-session-id`, `x-claude-code-agent-id`, `x-claude-code-parent-agent-id` — docs warn the **agent id is not a user identifier** | **[V]** | same page |
| Model discovery via `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`, but **only ids starting with `claude` or `anthropic` are accepted** | **[V]** | same page |
| Lost behind a gateway credential: Remote Control, voice dictation, Claude Code on web/Slack; fast-mode check still hits `api.anthropic.com` | **[V]** | [llm-gateway-connect](https://code.claude.com/docs/en/llm-gateway-connect) |
| **Hard incompatibility:** `forceLoginMethod`/`forceLoginOrgUUID` managed settings *cannot coexist* with `ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_API_KEY`/`apiKeyHelper` | **[V]** | same page, troubleshooting table |
| ⚠️ WAF trap: Claude Code prompts contain XML-ish tags and source code that trip XSS body rules → `403` with an HTML body while the gateway logs nothing. Exempt `/v1/messages` from body inspection | **[V]** | same page |

**Against our gateway (repo evidence):**

- We run **AIEG v1.0.0** (`charts/apps/values.yaml:496-565`). AIEG v1.0 serves inbound Anthropic
  Messages at **`POST /anthropic/v1/messages`** — not `/v1/messages`
  (`envoyproxy/ai-gateway` `site/docs/capabilities/llm-integrations/supported-endpoints.md`,
  "Anthropic Messages"). Claude Code appends `/v1/messages` to the base URL, so
  `ANTHROPIC_BASE_URL=https://api.ai.camer.digital/anthropic` lines up exactly. Both halves
  **[V]**; **[?]** that the concatenation works live — one curl settles it.
- ⚠️ **The blocker.** That AIEG doc lists the supported providers for `/anthropic/v1/messages` as
  **Anthropic, GCP Anthropic, AWS Anthropic, AWS Bedrock** — *not* "any OpenAI-compatible
  provider", which it does list for `/v1/chat/completions`. Our `claude-sonnet-5` /
  `claude-fable-5` are **DeepInfra backends with `schema: OpenAI`**
  (`charts/ai-models/values.yaml:1734-1810`, and `schema: OpenAI` throughout from `:205`).
  The AIEG **v1.0 release notes** nonetheless advertise "Anthropic `/v1/messages` to OpenAI
  `/v1/chat/completions`" translation. **The two vendor docs contradict each other — [?] unresolved.**
  If the supported-providers list wins, Route 1 needs a real `Anthropic`-schema
  `AIServiceBackend` (i.e. an Anthropic API key), which changes the cost model entirely.
- AIEG serves no `/v1/messages/count_tokens`; Claude Code documents that it then "estimates
  context usage locally" — acceptable **[V]**.
- Auth is free: the AuthConfig is **host-indexed, not path-indexed**
  (`ai-helm-values environments/prod/values/security-policies.yaml:59` `hosts:`), so any new path
  under `api.ai.camer.digital` is already Keycloak-JWT-gated. Authorino stamps `x-oidc-*`
  (ADR-0011) → Envoy access log → Alloy → Loki labels `user_id` / `email`
  (`docs/patterns/per-user-observability.md`). **Per-user observability genuinely does "just work"**
  the moment traffic arrives.
- One-line win if Route 1 ships: add `claude_code_session_id: "%REQ(X-CLAUDE-CODE-SESSION-ID)%"` to
  the access-log JSON at `charts/core-gateway/templates/envoy-proxy.yaml:164-190`.

**Cost:** a Keycloak client + an `apiKeyHelper` script distributed to developers, probably an
Anthropic-schema backend, plus permanent maintenance of an **open-ended** header/field
pass-through contract — Anthropic warns explicitly that pinning to an observed list breaks on
the next release.

### 3.2 Route 2 — Anthropic's usage APIs

| Fact | | Evidence |
|---|---|---|
| `GET /v1/organizations/usage_report/claude_code` → **one record per user per day**: `actor.email_address`, `model_breakdown[].tokens`, `estimated_cost.amount` (cents), sessions, LoC, commits, PRs, tool accept/reject, `terminal_type` | **[V]** | [Claude Code Analytics API](https://platform.claude.com/docs/en/manage-claude/claude-code-analytics-api) |
| Auth: **Admin API key** `sk-ant-admin01-…` in `x-api-key`. Free. Unavailable for individual accounts | **[V]** | same page |
| ~1 h freshness; one day per request; cursor pagination; no stated deletion period | **[V]** | same page |
| ⚠️ **"This API only tracks Claude Code usage on the Claude API"** — not Bedrock, Foundry, Vertex, or Claude Platform on AWS | **[V]** | same page, FAQ |
| ⚠️ **Open bug: OAuth/subscription users never appear.** `anthropics/claude-code#27780` opened 2026-02-23, **still OPEN**, last activity 2026-07-30, no engineering resolution; `#20819` closed as duplicate. Anthropic support in-thread: *"This is an architectural limitation"*, and *"that's actually incorrect information, I'll flag that to the docs team"* about the docs claiming coverage | **[V]** | [#27780](https://github.com/anthropics/claude-code/issues/27780), [#20819](https://github.com/anthropics/claude-code/issues/20819) |
| Claude **Enterprise** (claude.ai) orgs use a *different* API + *different* key (Analytics API key, `read:analytics`, primary owner only); data only from 2026-01-01; 60 req/min | **[V]** | [Analytics APIs](https://platform.claude.com/docs/en/manage-claude/analytics-api) |
| General **Usage API** `group_by`: `account_id`, `api_key_id`, `context_window`, `inference_geo`, `model`, `service_account_id`, `service_tier`, `speed`, `workspace_id` — `account_id` is **null for non-OAuth requests** | **[V]** | [usage report ref](https://platform.claude.com/docs/en/api/admin-api/usage-cost/get-messages-usage-report) |
| **Cost API** groups only by `description` / `workspace_id` — **no per-user dimension at all** | **[V]** | [cost report ref](https://platform.claude.com/docs/en/api/admin-api/usage-cost/get-cost-report) |

**⚠️ Routes 1 and 2 are mutually exclusive for the same request.** Through our gateway, Anthropic
sees one Console API key: `account_id` is null and the Analytics API shows nothing. Direct to
Anthropic, our gateway sees nothing. This is the single most important structural fact in this
report.

### 3.3 Route 3 — Claude Code's native OTLP exporter ⭐

| Fact | | Evidence |
|---|---|---|
| `CLAUDE_CODE_ENABLE_TELEMETRY=1` + `OTEL_METRICS_EXPORTER` / `OTEL_LOGS_EXPORTER` (`otlp`), `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` \| `http/json` \| `http/protobuf`), `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer …"` | **[V]** | [monitoring-usage](https://code.claude.com/docs/en/monitoring-usage) |
| Metrics: `claude_code.session.count`, `claude_code.token.usage`, **`claude_code.cost.usage` (USD)**, `claude_code.lines_of_code.count`, `claude_code.commit.count`, `claude_code.pull_request.count`, `claude_code.code_edit_tool.decision`, `claude_code.active_time.total` | **[V]** | same page |
| Events: `user_prompt`, `assistant_response`, `api_request`, `api_error`, `api_refusal`, `tool_decision`, `tool_result`, `auth`, `mcp_server_connection`, `plugin_*`, … | **[V]** | same page |
| Identity on every signal: `user.id`, **`user.email`** (when OAuth-authenticated), `user.account_uuid`, `organization.id`, `session.id`, `terminal.type`; events add `prompt.id` | **[V]** | same page |
| **Admins can enforce it** via the managed-settings `env` block — "cannot be overridden by users"; conflicting developer `OTEL_*` vars are removed at startup | **[V]** | same page |
| Content capture opt-in and off by default (`OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_TOOL_DETAILS`, `OTEL_LOG_TOOL_CONTENT`, `OTEL_LOG_RAW_API_BODIES`) | **[V]** | same page |
| Works **regardless of which route serves inference** — the OTLP endpoint config is independent of `ANTHROPIC_BASE_URL` | **[V]** | same page (telemetry config is orthogonal to auth/base-URL config) |

### 3.4 Route 4 — Anthropic's own `claude gateway`

Not in the brief, but it is Route 1's direct competitor and deserves an explicit decision.

| Fact | | Evidence |
|---|---|---|
| Self-hosted, **shipped inside the `claude` binary** (`claude gateway --config gateway.yaml`); **no separate licence or per-seat fee** | **[V]** | [claude-apps-gateway](https://code.claude.com/docs/en/claude-apps-gateway), [deploy](https://code.claude.com/docs/en/claude-apps-gateway-deploy) |
| Signs developers in with **any OIDC IdP — Keycloak named explicitly**; authorization-code flow, `allowed_email_domains`, session `ttl_hours` (default 1 h, bounds deprovisioning latency) | **[V]** | [claude-apps-gateway](https://code.claude.com/docs/en/claude-apps-gateway) |
| Relays **OTLP/HTTP** to your own collectors (`telemetry.forward_to[].url` + `headers`), per-signal opt-in, default metrics-only | **[V]** | [config](https://code.claude.com/docs/en/claude-apps-gateway-config) |
| **Stamps `user.id` = IdP subject, `user.email`, `user.groups`, `identity.source=gateway-oidc`** on every export — per-developer attribution keyed on the **Keycloak `sub`**, zero developer-side config | **[V]** | [monitoring-usage](https://code.claude.com/docs/en/monitoring-usage) |
| Also pushes managed settings + model allowlists per IdP group; Postgres-backed; Kubernetes deployment documented | **[V]** | [deploy](https://code.claude.com/docs/en/claude-apps-gateway-deploy) |
| ⚠️ **Upstream provider enum is closed: `anthropic`, `bedrock`, `anthropicAws`, `vertex`, `foundry`.** No arbitrary/OpenAI-compatible upstream; `base_url` only relocates a *named* provider | **[V]** | [config](https://code.claude.com/docs/en/claude-apps-gateway-config) |
| Not supported: SAML/LDAP, OTLP/gRPC, multiple OIDC issuers per instance, server-side web search, 1-hour cache TTL | **[V]** | [claude-apps-gateway](https://code.claude.com/docs/en/claude-apps-gateway) |

Route 4 gives the *exact* identity join we want (IdP `sub` == Keycloak `sub` == our existing
`user_id` Loki label) for free — but it **cannot route to our own model fleet**. It is a
Claude-only, Anthropic-billed path that sits *beside* `api.ai.camer.digital`, not inside it.

---

## 4. OpenAI Codex

### 4.1 Route 1 — Codex as a gateway client

| Fact | | Evidence |
|---|---|---|
| `[model_providers.<id>]` keys: `name`, `base_url`, `env_key`, `env_key_instructions`, `wire_api`, `http_headers`, `env_http_headers`, `query_params`, `requires_openai_auth`, `request_max_retries`, `stream_max_retries`, `stream_idle_timeout_ms` | **[V]** | [config-advanced](https://learn.chatgpt.com/docs/config-file/config-advanced) |
| **Credential helper exists:** `[model_providers.<id>.auth]` with `command`, `args`, `timeout_ms`, `refresh_interval_ms`. "The auth command receives no `stdin` and must print the token to stdout… refreshes proactively at `refresh_interval_ms`". **Cannot** be combined with `env_key`, `experimental_bearer_token`, or `requires_openai_auth` | **[V]** | same page |
| Full key set also includes `experimental_bearer_token`, `supports_websockets`, `auth.cwd`. Provider ids `openai`, `ollama`, `lmstudio` are reserved | **[V]** | same page |
| Config at `~/.codex/config.toml` (user) or `.codex/config.toml` (project) — but **`model_provider`/`model_providers` only take effect at user level** | **[V]** | [config-basic](https://learn.chatgpt.com/docs/config-file/config-basic) |
| ⚠️ **`wire_api`: "`responses` is the only supported value, and it is the default when omitted."** Source carries a hard error: ``"`wire_api = \"chat\"` is no longer supported. How to fix: set `wire_api = \"responses\"`…"``; removal landed **February 2026** | **[V]** | [config-reference](https://learn.chatgpt.com/docs/config-file/config-reference), [discussion #7782](https://github.com/openai/codex/discussions/7782) |
| **ChatGPT sign-in + a third-party `base_url` is not realistic**: `requires_openai_auth` is mutually exclusive with `[auth]`, `env_key` and `experimental_bearer_token`; the only documented ChatGPT-auth-with-custom-`base_url` case is OpenAI's own data-residency host | **[V]** | [config-advanced](https://learn.chatgpt.com/docs/config-file/config-advanced), source comment on `requires_openai_auth` |
| Codex CLI is **Apache-2.0**, open source; OpenAI's own docs document Mistral, Ollama, LM Studio, Azure and Bedrock as custom providers, plus a `--oss` local mode | **[V]** | `gh api repos/openai/codex` → `license.spdx_id = Apache-2.0`; [config-advanced](https://learn.chatgpt.com/docs/config-file/config-advanced) |

This is a direct, first-class answer to **#679**: a small script that mints a Keycloak token (the
same script `apiKeyHelper` would run for Claude Code) wired into `[model_providers.camer.auth]`.
No static API key, real OAuth2, server-side revocation.

**⚠️ The `wire_api` blocker.** Because `responses` is the only value, Codex *must* use the OpenAI
Responses API. AIEG does serve `POST /v1/responses` (`supported-endpoints.md`) — **but this repo
has already been burnt by it**: our `/v1/responses` streaming omits `output_index` /
`content_index`, and opencode aborts with `text part <id> not found` unless a client-side plugin
repairs the SSE stream — `docs/integrations/opencode-well-known.md:353-360`. Codex ships no such
repair plugin. Whether Codex tolerates the omission is **[?] unverified** and is the highest-risk
unknown on the Codex side. There is no `wire_api = "chat"` fallback.

### 4.2 Route 2 — OpenAI's usage APIs

| Fact | | Evidence |
|---|---|---|
| **Platform Usage API** — `GET https://api.openai.com/v1/organization/usage/{completions,embeddings,images,moderations,audio_speeches,audio_transcriptions,vector_stores,code_interpreter_sessions,file_search_calls,web_search_calls}`. `group_by` on `completions` = **`user_id`**, `project_id`, `api_key_id`, `model`, `batch`, `service_tier`; `bucket_width` ∈ `1m`\|`1h`\|`1d` (default `1d`); auth = **Admin key** `sk-admin-…` | **[V]** | [openai/openai-openapi `openapi.yaml`](https://github.com/openai/openai-openapi/blob/master/openapi.yaml), [cookbook](https://developers.openai.com/cookbook/examples/completions_usage_api) |
| ⚠️ **`GET /v1/organization/costs` cannot group by user** — `group_by` = `project_id`, `line_item`, `api_key_id` only; `bucket_width` = `1d` only. So per-user **dollars** must be derived from per-user **tokens** by pricing them ourselves | **[V]** | same spec |
| ⚠️ **ChatGPT-seat Codex never appears in the platform APIs.** `codex-rs/model-provider-info/src/lib.rs` routes auth modes `Chatgpt \| ChatgptAuthTokens \| Headers \| AgentIdentity \| PersonalAccessToken` to `CHATGPT_CODEX_BASE_URL = "https://chatgpt.com/backend-api/codex"`, falling back to `https://api.openai.com/v1` otherwise | **[V]** (source) | `openai/codex` `codex-rs/model-provider-info/src/lib.rs` |
| **[?]** Whether `usage/completions` even aggregates `/v1/responses` traffic (Codex's actual wire API) — the endpoint is named "completions" and no doc states the mapping | **[?]** | — |
| **Codex analytics is Enterprise/Edu ONLY.** The official pricing matrix shows `—` for Plus, Pro, **Business** and API-key on all three of "Analytics dashboard", "Analytics API", "Compliance API and audit logs" | **[V]** | [Codex pricing matrix](https://learn.chatgpt.com/docs/pricing) |
| **Codex Analytics API** exists: "results are scoped to a ChatGPT workspace, but requests authenticate with a Platform organization API key." OpenAI states the *authenticated* reference at `https://chatgpt.com/codex/cloud/settings/apireference` "is the source of truth for… routes, request and response schemas… This page doesn't duplicate that contract" | **[V]** | [Analytics API](https://learn.chatgpt.com/docs/enterprise/analytics-api), [workspace analytics](https://learn.chatgpt.com/docs/enterprise/workspace-analytics) |
| ⚠️ **Its routes/scopes/schema are login-gated and could not be read.** Publicly circulating paths (`api.chatgpt.com/v1/analytics/codex`, scope `codex.enterprise.analytics.read`, daily/weekly buckets, 90-day lookback) are **secondary-source only — do not design against them** | **[?]** | — |
| **Compliance API does cover Codex, at per-user grain.** Base paths verified from OpenAI's own cookbook: `API_BASE="https://api.chatgpt.com/v1/compliance"`, then `{API_BASE}/{workspaces\|organizations}/{id}/logs` and `.../logs/{id}`, `Authorization: Bearer $COMPLIANCE_API_KEY`, params `event_type`/`limit`/`after`, JSONL. Retention **30 days** | **[V]** for base paths + Codex coverage | [openai-cookbook `logs_platform.ipynb`](https://github.com/openai/openai-cookbook/blob/main/examples/chatgpt/compliance_api/logs_platform.ipynb), [compliance-api](https://learn.chatgpt.com/docs/enterprise/compliance-api) |
| A `CODEX_LOG` event type reportedly carries `prompt_text`, `response_text`, `token_usage`, `tool_input`, `client_id` (`CODEX_CLI`, `CODEX_IDE_VSCODE`, …) with an actor envelope of `user_id`/`user_email`, plus a `COSTS` event with per-user-per-hour credits — **field-level schema is second-hand** (the admin OpenAPI spec 403s to automated fetch) | **[?]** | — |
| OpenAI's own guidance on the Compliance API: *"It's not a productivity dashboard. Don't use it to infer code quality or individual performance."* | **[V]** | [compliance-api](https://learn.chatgpt.com/docs/enterprise/compliance-api) |
| ⚠️ **Discrepancy:** the pricing matrix says Enterprise/Edu-only; a Global Admin Console help article reportedly says "Business, Enterprise, and Edu". Verify against our own plan before designing | **[?]** | — |

**Net:** for Codex the vendor-API route is **plan-gated to ChatGPT Enterprise/Edu**, and the one
API whose schema we can actually read (Compliance) is explicitly *not* intended as a usage
dashboard and keeps only 30 days. That is a materially weaker position than the Anthropic side,
and a strong argument for not depending on it.

### 4.3 Route 3 — Codex's native OTLP exporter ⭐

| Fact | | Evidence |
|---|---|---|
| `[otel]` in `~/.codex/config.toml`: `environment` (default `"dev"`), `exporter` (`none`\|`otlp-http`\|`otlp-grpc`), `metrics_exporter` (`none`\|`statsig`\|`otlp-http`\|`otlp-grpc`), `trace_exporter`, `log_user_prompt`; per-exporter `.<id>.endpoint`, `.<id>.headers`, `.<id>.protocol` (`binary`\|`json`), `.<id>.tls.{ca-certificate,client-certificate,client-private-key}` | **[V]** | [config-reference](https://learn.chatgpt.com/docs/config-file/config-reference) |
| ⚠️ **Syntax:** the exporter is an **inline table**, not a `[otel.exporter.otlp-http]` section header — `exporter = { otlp-http = { endpoint = "…", protocol = "binary", headers = { … } } }` | **[V]** | same page |
| ⚠️ **`metrics_exporter` defaults to `statsig`** — i.e. Codex ships metrics to OpenAI unless changed. Disable with `[analytics] enabled = false` | **[V]** | same page |
| Events: `codex.conversation_starts`, `codex.api_request`, `codex.sse_event`, `codex.websocket_request`, `codex.websocket_event`, `codex.user_prompt`, `codex.tool_decision`, `codex.tool_result` (+ in source `codex.startup_phase`, `codex.turn_ttft`, `codex.auth_recovery`, `codex.sandbox_outcome`) | **[V]** | same page + `codex-rs/otel` |
| Metrics: `codex.api_request{,.duration_ms}`, `codex.sse_event{,.duration_ms}`, `codex.websocket.{request,event}{,.duration_ms}`, `codex.tool.call{,.duration_ms}`, **`codex.turn.token_usage`**, `codex.turn.e2e_duration_ms`, `codex.turn.ttft.duration_ms`, `codex.guardian.review.token_usage`, `codex.hooks.run` | **[V]** | doc table + `codex-rs/otel/src/metrics/names.rs` |
| **Opt-in, off by default**; prompts redacted unless `log_user_prompt = true` | **[V]** | [config-advanced](https://learn.chatgpt.com/docs/config-file/config-advanced) |
| ⭐ **Identity, on LOG events only:** `log_event!` attaches `conversation.id`, `app.version`, `auth_mode`, `originator`, **`user.account_id`**, **`user.email`**, `terminal.type`, `model`, `slug` | **[V]** (source, undocumented) | `openai/codex` `codex-rs/otel/src/events/shared.rs:13-17` |
| ⚠️ **`trace_event!` emits the same list minus `user.account_id` and `user.email`; metric tags carry NO identity at all** (`auth_mode`, `session_source`, `originator`, `service_name`, `model`, `app.version`). Resource attributes are `service.version`, `env`, `host.name` | **[V]** | `shared.rs:33-35`, `codex-rs/otel/src/metrics/tags.rs`, `provider.rs` |
| ⚠️⚠️ **`account_id`/`email` come from `CodexAuth::get_account_id`/`get_account_email`, read from the stored ChatGPT id_token — so they are populated under ChatGPT sign-in and `None` under API-key / custom-provider auth.** `auth_mode` ∈ `swic \| api \| unknown` | **[V]** (source) | `codex-rs/login/src/auth/manager.rs` |
| Earlier gap ([#12913](https://github.com/openai/codex/issues/12913): `codex exec` no metrics, `codex mcp-server` no telemetry) fixed by [PR #13083](https://github.com/openai/codex/pull/13083), merged 2026-02-28. **But [#33668](https://github.com/openai/codex/issues/33668) is OPEN** — `codex exec` still does not export `codex.turn.token_usage`; token counts appear only as span/log attributes (`input_token_count`, `output_token_count`, `cached_token_count`, `reasoning_token_count`, `tool_token_count`) as of 0.144.x | **[V]** | issue/PR threads |
| Codex has no documented equivalent of Claude Code's *enforced* managed-settings telemetry. An admin `requirements.toml` layer exists (`allow_managed_hooks_only`) but whether it can pin `[otel]` is **[?]** | **[?]** | `openai/codex` `docs/config.md` |

**The headline:** Codex emits **`user.email`** — the *same attribute name* Claude Code uses. One
Alloy pipeline, one label-promotion stage and one join key serve both products.

**⚠️ But Routes 1 and 3 interact badly for Codex.** Identity attributes are populated only under
ChatGPT sign-in; pointing Codex at our gateway requires `env_key` / `experimental_bearer_token` /
`auth.command` (all mutually exclusive with `requires_openai_auth`), so `auth_mode` becomes `api`
and `user.account_id` / `user.email` go empty. **If we do both, Codex telemetry has no payload
identity at all** — which is precisely why identity must be bound *server-side by the per-developer
ingest token* rather than read off the payload. Same conclusion as §5, but here it is mandatory
rather than merely safer. Note also that token counts live on **metrics**, which never carry
identity — so the token must be the join key regardless.

---

## 5. The identity-join problem

Four key spaces; they do **not** all reconcile.

| Source | Key emitted | Join to internal identity |
|---|---|---|
| Gateway traffic (today) | Keycloak `sub` → `x-oidc-user-id` → Loki `user_id`, plus a verified `email` label | **Native** — `docs/patterns/per-user-observability.md`; Keycloak `user_entity` datasource (ai-helm ADR-0063) |
| Claude apps gateway OTLP (Route 4) | `user.id` = **IdP subject** = the same Keycloak `sub` | **Native, no mapping table** |
| Claude Code native OTLP | `user.email`, `user.account_uuid`, `organization.id` | By **verified email** |
| Codex native OTLP | `user.email`, `user.account_id` | By **verified email** |
| Anthropic Analytics API | `actor.email_address` (or `api_key_name`) | By **verified email** |
| OpenAI Usage API | OpenAI `user_id` (org-scoped opaque) | Mapping table only |

**`identity_maps` fits unchanged.**
`lightbridge-governance/crates/governance-core/migrations/postgres/20260802000939_init/up.sql:26-40`
already carries `(tenant_id, provider, provider_user_id, internal_user_id, team_id,
cost_center_id, mapping_source, valid_from, valid_to)`. `provider` takes `anthropic` / `openai` /
`claude-code` / `codex` exactly as it takes `github`. RFC-0001's rule applies verbatim: join on
**verified email**, **never** on display name.

**One structural improvement over email-matching.** If telemetry arrives on our own
Authorino-gated OTLP endpoint bearing a **per-developer integration token**, identity is
established *server-side by the token* and stamped as a trusted header — exactly the mechanism
RFC-0002 already specifies (`response.success.headers` → `governance.*`). The client-supplied
`user.email` then becomes a cross-check rather than the source of truth. That matters: a
client-set OTLP resource attribute is otherwise trivially forgeable.

---

## 6. What I could not verify

1. **[?] Whether AIEG v1.0 actually translates inbound `/anthropic/v1/messages` to an
   `OpenAI`-schema backend.** The supported-endpoints doc lists only Anthropic-family providers
   for that endpoint; the v1.0 release notes advertise Anthropic→OpenAI translation. **This gates
   the entire Claude-Code-as-gateway-client design.** One curl against the live gateway settles it.
2. **[?] Whether `ANTHROPIC_BASE_URL=https://api.ai.camer.digital/anthropic` concatenates to
   `/anthropic/v1/messages?beta=true`.** Documented on both sides; never tested together.
3. **[?] Whether Codex tolerates our `/v1/responses` SSE stream** (missing `output_index` /
   `content_index`). opencode did not, and there is no `wire_api` fallback.
4. **[?] The ChatGPT Enterprise Codex Analytics API's routes, scope name, granularity and
   response schema — including whether it carries per-user identity.** The authoritative
   reference is behind workspace authentication. Everything circulating publicly is
   secondary-source and should not be designed against. Same for the `CODEX_LOG` / `COSTS`
   Compliance-API field schemas (the admin OpenAPI spec 403s to automated fetch).
5. **[?] Whether ChatGPT-seat Codex usage is visible on a Business plan.** The official pricing
   matrix says Enterprise/Edu only; a help article reportedly says Business too. Contradiction
   unresolved.
6. **[?] Whether `usage/completions` aggregates `/v1/responses` traffic** — Codex's actual wire
   API. The endpoint name suggests Chat Completions and no doc states the mapping.
7. **[?] Whether Codex's admin `requirements.toml` layer can pin `[otel]`** the way Claude Code's
   managed settings can. If it cannot, Codex telemetry is *opt-in per developer* and enforcement
   is a policy problem, not a technical one.
8. **[?] Whether openai/codex#33668 (`codex exec` missing `codex.turn.token_usage`) affects our
   usage** — it does if any CI/non-interactive Codex runs are in scope.
9. **[?] Any OpenAI terms clause permitting or prohibiting a self-hosted gateway.**
   `openai.com/policies/service-terms` and `/usage-policies` 403 to automated fetch. The
   documented feature set is strong implicit permission; the absence of an explicit clause is
   worth a legal read if it is load-bearing.
10. **[?] Claude Code Analytics API rate limits** — not stated on either doc page.
11. **[?] Whether Team/Enterprise seat OAuth proxying falls under the same prohibition** as
    Free/Pro/Max. The Claude Code legal page names Free/Pro/Max explicitly; Consumer Terms §2
    credential-sharing and "ordinary, individual usage" still apply. Treat as prohibited until
    counsel says otherwise.
12. **[?] Distribution mechanism for laptops.** Coder workspaces are a proven vector here —
    `docs/integrations/coder-platform-integration.md:436-483` already injects opencode config and
    Keycloak credentials into workspace templates — but developers running these tools on their own
    machines need MDM or a dotfiles convention. No MDM is in evidence in these repos.

---

## 7. Recommendation

### Do this

**Phase A — one endpoint, both products (the 80/20).**
Generalise `lightbridge-governance` RFC-0002's OTLP ingest from "the Foundry connector" to "**the
push connector**", with per-provider normalizers. Claude Code and Codex become two more producers
on `otel.ai.camer.digital`, authenticated by a **per-developer integration token** issued by the
existing registry.

- Normalize `claude_code.*` metrics/events and Codex's session events into the same
  execution / model-call / tool-call records; money in **integer micro-USD** (governance ADR-0008).
- Distribute config: Claude Code via **managed settings** (`env` block — developers cannot
  override), Codex via `[otel]` in `~/.codex/config.toml`. Seed from the Coder workspace template
  first, dotfiles second.
- `identity_maps` rows: `provider='claude-code'` / `'codex'`, `provider_user_id` = the token's
  subject, `internal_user_id` = the Keycloak `sub`. Cross-check against the emitted `user.email`;
  alert on mismatch rather than trusting it.

Why first: provider-agnostic, does not touch the request path, works whether or not the tools ever
point at our gateway, needs no Anthropic/OpenAI commercial relationship, and the hard part
(public TLS host + AuthConfig #3 + collector + quota) is **already designed and decided**.

**Phase B — finish #680 and #679 as the authentication tickets they are.**
Both have a clean, documented, non-hacky answer neither issue currently names:

- **#680** → `apiKeyHelper`: a script that mints a Keycloak token; 5-minute cache; re-run on 401.
- **#679** → `[model_providers.camer.auth] command = …`: the same script, native Codex support.

Gate #680 on the AIEG Anthropic-ingress curl (§6.1) and #679 on the `/v1/responses` SSE question
(§6.3). Both spikes are under an hour.

**Phase C — optional reconciliation, only if we buy seats.**
If the org ends up with an Anthropic Console Team/Enterprise org, add a *pull* connector for
`/v1/organizations/usage_report/claude_code` modelled directly on `governance-copilot` — same
6-hourly CronJob, same `ingest_manifest` high-water mark, same S3 raw archive. It is per-user,
daily and free, and gives LoC/commit/PR metrics the OTLP path also gives but from the vendor's
own books. Treat it as a **cross-check**, never the system of record, because of #27780. Do **not**
plan the Codex equivalent until someone can read the authenticated Enterprise reference (§6.4);
if Codex ever runs on *API keys* rather than ChatGPT seats, the platform Usage API with
`group_by=user_id` is a simpler cross-check — but price the tokens yourself, because
`/v1/organization/costs` cannot group below `api_key_id`.

### Do NOT do this

- ❌ **Do not proxy Claude subscription (Free/Pro/Max) OAuth credentials through the gateway.**
  Anthropic: it "does not permit third-party developers to offer Claude.ai login or to route
  requests through Free, Pro, or Max plan credentials on behalf of their users", and "reserves the
  right to take measures to enforce these restrictions… without prior notice"
  ([legal-and-compliance](https://code.claude.com/docs/en/legal-and-compliance)); Consumer Terms §2
  separately forbids sharing account credentials
  ([consumer terms](https://www.anthropic.com/legal/consumer-terms)). A gateway upstream must be a
  **Console API key**. This is the one finding that could quietly sink the project.
- ❌ **Do not make the Anthropic Analytics pull connector the primary source.** Its
  subscription/OAuth blind spot (#27780) has been open ~5 months with no engineering response,
  and it is void the moment traffic moves to our gateway.
- ❌ **Do not plan on getting both gateway metering and vendor-side analytics for the same
  request.** They are mutually exclusive; choose per workload.
- ❌ **Do not deploy Anthropic's `claude gateway` as a replacement for `api.ai.camer.digital`.**
  Its upstream enum is closed to five named providers and cannot reach our own fleet. It deserves
  a *separate* ADR only if the org decides Claude Code should be billed to Anthropic directly —
  in which case it is the best per-user attribution available anywhere in this report, and free.
- ❌ **Do not hand-maintain an allowlist of `anthropic-beta` values** if Route 1 ships. Anthropic
  warns explicitly that a gateway pinned to an observed list breaks on the release that introduces
  the next capability.
- ❌ **Do not use `x-claude-code-agent-id` as a user key.** The protocol docs say so outright.
- ❌ **Do not design against the secondary write-ups of the Codex Enterprise Analytics API.** The
  endpoint paths and scope names circulating publicly are unverified; getting them wrong is a
  silently-empty connector.
- ❌ **Do not turn on content capture** (`OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_TOOL_CONTENT`,
  `log_user_prompt`) as part of a usage-metering rollout. RFC-0002's `metadata_only` default and
  its Loki per-stream-retention warning apply verbatim; token counts need none of it.
- ❌ **Do not leave Codex's `metrics_exporter` at its default.** It defaults to **`statsig`** —
  metrics go to OpenAI. Any rollout must set it explicitly (or `[analytics] enabled = false`).
  This is a governance finding in its own right, independent of whether we ingest anything.
- ❌ **Do not rely on Codex's payload identity if Codex is also pointed at our gateway.** The
  `user.account_id` / `user.email` attributes are populated only under ChatGPT sign-in, and Codex
  metrics carry no identity at any time. Bind identity to the per-developer ingest token.

### Licensing / ToS summary

| Constraint | Verdict |
|---|---|
| Routing Claude Code to a **non-Claude** model via a gateway | **"Not supported", not prohibited.** Exact wording: "Anthropic doesn't endorse, maintain, or audit third-party gateway products, and doesn't support routing Claude Code to non-Claude models through any gateway" ([llm-gateway](https://code.claude.com/docs/en/llm-gateway)). A compatibility statement on a docs page, not a contract clause. Operational risk, not legal. |
| Proxying **subscription** credentials | **Prohibited.** See above. |
| Reselling / redistributing access | **Prohibited** — Commercial Terms §D.4 ([commercial-terms](https://www.anthropic.com/legal/commercial-terms)). Internal use behind a gateway fronting a Console key is not reselling. |
| Pointing **Codex** at a non-OpenAI endpoint | **Explicitly supported product feature** — OpenAI's own docs walk through Mistral, Ollama, LM Studio, Azure and Bedrock as custom `model_providers`, plus a `--oss` local mode; the CLI is Apache-2.0. **[?]** No clause either way in `openai.com/policies/service-terms` or `/usage-policies` — both 403 to automated fetch. Worth a human read if load-bearing. |

---

## 8. New epics, or extensions of #680 / #679?

**Both, cleanly separated.**

- **#680 and #679 stay as they are.** They are *authentication* tickets ("complete an OAuth2 flow
  against our issuer, then talk to our gateway") and their source-of-truth links are correct. They
  need only (a) a comment naming the mechanism each vendor actually documents — `apiKeyHelper`;
  `[model_providers.<id>.auth] command` — and (b) a blocking spike each (AIEG Anthropic ingress;
  Codex `/v1/responses`). **Do not widen them into telemetry tickets** — they would then depend on
  a route we may not take.

- **One new epic**, filed against `lightbridge-governance`: *"Per-user usage telemetry for
  developer AI clients (Claude Code, Codex)"*. That is where the connector, the registry, the
  `identity_maps` table and the OTLP ingest live. It should produce:
  - **RFC-0003** — "client-side OTLP ingestion: Claude Code and Codex", sibling to RFC-0002;
  - an **ADR** generalising ADR-0006's endpoint from Foundry-specific to provider-agnostic;
  - an **ai-helm ADR** for the `otel-https` listener + AuthConfig #3 + collector. ⚠️ RFC-0002's own
    warnings apply verbatim: create the `ssegning-aws` property and confirm `SecretSynced=True`
    **before** the AuthConfig references it (a `sharedSecretRef` to a missing Secret fails
    AuthConfig readiness and **404s the whole gateway** — the OPA-removal outage), and the new
    listener **must** be added to the SecurityPolicy's `sectionNames` or it is silently
    unauthenticated;
  - Grafana dashboards (governance ADR-0003: the dashboards *are* the product).

- **One small, independent ai-helm ticket:** add
  `claude_code_session_id: "%REQ(X-CLAUDE-CODE-SESSION-ID)%"` to the Envoy access-log JSON at
  `charts/core-gateway/templates/envoy-proxy.yaml:164-190`. One line; useful only if Route 1
  ships, harmless otherwise.

---

## Appendix — key sources

**Anthropic / Claude Code:** [authentication](https://code.claude.com/docs/en/authentication) ·
[llm-gateway](https://code.claude.com/docs/en/llm-gateway) ·
[llm-gateway-connect](https://code.claude.com/docs/en/llm-gateway-connect) ·
[llm-gateway-protocol](https://code.claude.com/docs/en/llm-gateway-protocol) ·
[monitoring-usage](https://code.claude.com/docs/en/monitoring-usage) ·
[claude-apps-gateway](https://code.claude.com/docs/en/claude-apps-gateway) ·
[claude-apps-gateway-config](https://code.claude.com/docs/en/claude-apps-gateway-config) ·
[claude-apps-gateway-deploy](https://code.claude.com/docs/en/claude-apps-gateway-deploy) ·
[legal-and-compliance](https://code.claude.com/docs/en/legal-and-compliance) ·
[Claude Code Analytics API](https://platform.claude.com/docs/en/manage-claude/claude-code-analytics-api) ·
[Analytics APIs](https://platform.claude.com/docs/en/manage-claude/analytics-api) ·
[Usage & Cost API](https://platform.claude.com/docs/en/manage-claude/usage-cost-api) ·
[#27780](https://github.com/anthropics/claude-code/issues/27780) ·
[#20819](https://github.com/anthropics/claude-code/issues/20819)

**OpenAI / Codex:** [config-basic](https://learn.chatgpt.com/docs/config-file/config-basic) ·
[config-advanced](https://learn.chatgpt.com/docs/config-file/config-advanced) ·
[config-reference](https://learn.chatgpt.com/docs/config-file/config-reference) ·
[Enterprise admin setup](https://learn.chatgpt.com/docs/enterprise/admin-setup) ·
[Enterprise Analytics API](https://learn.chatgpt.com/docs/enterprise/analytics-api) ·
[Codex pricing matrix](https://learn.chatgpt.com/docs/pricing) ·
[Enterprise workspace analytics](https://learn.chatgpt.com/docs/enterprise/workspace-analytics) ·
[Compliance API](https://learn.chatgpt.com/docs/enterprise/compliance-api) ·
[openai/openai-openapi `openapi.yaml`](https://github.com/openai/openai-openapi/blob/master/openapi.yaml) ·
[usage-API cookbook](https://developers.openai.com/cookbook/examples/completions_usage_api) ·
[compliance-API cookbook](https://github.com/openai/openai-cookbook/blob/main/examples/chatgpt/compliance_api/logs_platform.ipynb) ·
`openai/codex` — `codex-rs/otel/README.md`, `codex-rs/otel/src/events/shared.rs`,
`codex-rs/otel/src/metrics/{names,tags}.rs`, `codex-rs/otel/src/provider.rs`,
`codex-rs/login/src/auth/manager.rs`, `codex-rs/model-provider-info/src/lib.rs`, `docs/config.md` ·
[#12913](https://github.com/openai/codex/issues/12913) ·
[PR #13083](https://github.com/openai/codex/pull/13083) ·
[#33668](https://github.com/openai/codex/issues/33668) ·
[discussion #7782](https://github.com/openai/codex/discussions/7782)

**Envoy AI Gateway:**
[supported endpoints](https://aigateway.envoyproxy.io/docs/capabilities/llm-integrations/supported-endpoints/)
(and `envoyproxy/ai-gateway` `site/docs/capabilities/llm-integrations/supported-endpoints.md`) ·
[v1.0 release notes](https://aigateway.envoyproxy.io/release-notes/v1.0/)

**Repos:** `ai-helm` — `charts/apps/values.yaml:496-565`, `charts/ai-models/values.yaml:1734-1810`,
`charts/core-gateway/templates/envoy-proxy.yaml:164-190`,
`docs/integrations/opencode-well-known.md:353-360`,
`docs/integrations/coder-platform-integration.md:436-483`, `docs/patterns/per-user-observability.md` ·
`ai-helm-values` — `environments/prod/values/security-policies.yaml:59`,
`environments/prod/deps/alloy/ciliumnetworkpolicy.yaml` ·
`lightbridge-governance` — `docs/adr/0005`, `docs/adr/0006`, `docs/rfc/0001`, `docs/rfc/0002`,
`crates/governance-core/migrations/postgres/20260802000939_init/up.sql:26-40`
