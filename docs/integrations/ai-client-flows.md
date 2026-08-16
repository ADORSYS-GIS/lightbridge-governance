# AI client flows

One sequence diagram per row of the
[AI client support matrix](ai-client-support-matrix.md). The matrix says
*whether* a capability works per client; this says *how*, and where it breaks.

Every flow here was traced against the live system unless a diagram is marked
**PROPOSED**. Where a step is a known trap, the trap is on the diagram rather
than in prose underneath it — the diagram is meant to be readable alone.

Actors are consistent throughout:

| Actor | What it is |
|---|---|
| `CLI` | Claude Code / Codex / VS Code Copilot on a developer laptop |
| `ga` | `governance-auth`, this repo's credential helper (ADR-0010) |
| `KC` | Keycloak — `auth.verif.fyi/realms/camer-digital`, client `governance-auth-cli` |
| `authz` | lightbridge-authz — `auth.ai.camer.digital`, self-signed API-key issuer |
| `GW` | Envoy AI Gateway — `api.ai.camer.digital` |
| `OTEL` | `aiCliOtel` collector — `otel.ai.camer.digital` |
| `Alloy` | cluster OTLP fan-out → Tempo / Loki / Mimir |

---

## Login — the one interactive ceremony

Runs once per developer. Everything else is non-interactive.

```mermaid
sequenceDiagram
    autonumber
    participant Dev as Developer
    participant ga as governance-auth
    participant KC as Keycloak

    Dev->>ga: governance-auth login [--device-code]
    ga->>ga: generate PKCE verifier + S256 challenge
    ga->>KC: GET /.well-known/openid-configuration
    KC-->>ga: authorization / token / device endpoints

    alt browser flow (default)
        ga->>KC: authorize + code_challenge (loopback redirect)
        Dev->>KC: authenticate in browser
        KC-->>ga: authorization code
    else --device-code (headless / SSH)
        ga->>KC: POST device_authorization + code_challenge
        Note over ga,KC: ⚠️ Keycloak requires PKCE on the DEVICE flow too.<br/>Without code_challenge_method it rejects with<br/>invalid_request "Missing parameter: code_challenge_method"
        KC-->>ga: user_code + verification_uri
        ga-->>Dev: prints code + URL (stderr)
        Dev->>KC: enters code in browser
        loop until approved or expired
            ga->>KC: POST token (device_code + code_verifier)
        end
    end

    KC-->>ga: access_token (~300s) + refresh_token
    ga->>ga: cache session, file mode 0600
    ga-->>Dev: logged in (nothing secret on stdout)
```

---

## Inference endpoint + inference auth

Matrix rows: **Inference endpoint**, **Inference auth**.

The credential helper is re-invoked by the CLI; `governance-auth` refreshes
silently against Keycloak, so the developer logs in once.

```mermaid
sequenceDiagram
    autonumber
    participant CLI as Claude Code
    participant ga as governance-auth token
    participant KC as Keycloak
    participant GW as Envoy AI Gateway

    CLI->>ga: spawn apiKeyHelper (cached CLAUDE_CODE_API_KEY_HELPER_TTL_MS)
    Note over CLI,ga: ⚠️ TTL set to 240s deliberately.<br/>Claude Code's default cache is 5 min — exactly the<br/>Keycloak token lifetime — so the cache can hand back<br/>a token that expired moments ago.

    ga->>ga: load cached session (FileLock)
    alt access token still valid
        ga-->>CLI: access_token
    else expired
        ga->>KC: POST token (grant_type=refresh_token)
        KC-->>ga: new access_token + refresh_token
        ga->>ga: rewrite cache 0600
        ga-->>CLI: access_token
    end

    CLI->>GW: POST /anthropic/v1/messages<br/>Authorization: Bearer {token}
    GW->>GW: Authorino validates iss/sig, stamps azp/email/model
    GW-->>CLI: 200 + completion

    Note over CLI,GW: ✅ Verified live: claude -p returned a real answer,<br/>and an expired 22h-old session refreshed silently.
```

### Codex spawns its helper differently — and that broke it

```mermaid
sequenceDiagram
    autonumber
    participant CX as Codex
    participant OS as OS exec
    participant ga as governance-auth token

    CX->>OS: spawn auth.command (NO shell)
    Note over CX,OS: ⚠️ No shell ⇒ no login-shell PATH.<br/>A bare `governance-auth` cannot resolve when the<br/>binary lives in ~/.local/bin.

    alt command is a bare name
        OS-->>CX: No such file or directory (os error 2)
        CX->>CX: proceeds UNAUTHENTICATED
        Note over CX: Surfaces later as a confusing API error,<br/>never as "the helper did not run".
    else command is an absolute path
        OS->>ga: exec /home/USER/.local/bin/governance-auth … token
        ga-->>CX: access_token
    end
```

Claude Code resolves a bare name (it uses a shell), so this trap is invisible
if only that client is tested. `governance-auth` now builds every command it
writes from `otel::binary_path()` (`std::env::current_exe()`).

---

## Telemetry endpoint + telemetry auth (refreshing)

Matrix rows: **Telemetry endpoint**, **Telemetry auth, refreshing**.

This is the path that is **currently broken** — see
[#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84).

```mermaid
sequenceDiagram
    autonumber
    participant CLI as Claude Code
    participant ga as governance-auth otel-headers
    participant KC as Keycloak
    participant OTEL as aiCliOtel collector

    CLI->>ga: spawn otelHeadersHelper (every 240s)
    Note over CLI,ga: ⚠️ otelHeadersHelper OVERRIDES<br/>OTEL_EXPORTER_OTLP_HEADERS. A valid static key in<br/>the env var is IGNORED while the helper exists —<br/>measured live. So no static workaround is possible here.

    ga->>KC: refresh if needed
    KC-->>ga: access_token (iss: auth.verif.fyi/…)
    ga-->>CLI: {"Authorization": "Bearer {keycloak token}"}

    CLI->>OTEL: POST /v1/traces  Authorization: Bearer …
    OTEL->>OTEL: oidc extension validates against<br/>auth.ai.camer.digital JWKS
    OTEL--x CLI: ❌ 401 — failed to verify id token signature
    Note over OTEL: Collector logs the wrong issuer BY NAME:<br/>issuer: https://auth.verif.fyi/realms/camer-digital
```

### Token exchange (RFC 8693, opt-in) — the same flow once `--token-exchange` is on

Ships in `governance-auth` behind `--token-exchange`/`GOVERNANCE_AUTH_TOKEN_EXCHANGE`
(OFF by default). See `oauth::exchange`'s module doc and `oauth::mod::emit_token`
for the source of truth this diagram is traced from — read those, not this
diagram, if the two ever disagree.

```mermaid
sequenceDiagram
    autonumber
    participant CLI as Claude Code
    participant ga as governance-auth otel-headers
    participant KC as Keycloak
    participant authz as lightbridge-authz
    participant OTEL as aiCliOtel collector

    CLI->>ga: spawn otelHeadersHelper

    ga->>ga: load cached upstream session
    alt upstream access token still valid
        Note over ga: no Keycloak round trip
    else expired, refresh token held
        ga->>KC: POST token (grant_type=refresh_token)
        KC-->>ga: new upstream access_token
    end

    Note over ga,authz: emit_token calls exchange::run FRESH on every<br/>invocation -- no caching of the exchanged token,<br/>and no independent refresh via authz's own<br/>refresh_token grant.
    ga->>authz: POST /oauth2/token<br/>grant_type=…:token-exchange<br/>subject_token={upstream access_token}<br/>subject_token_type=access_token<br/>[scope=… if --exchange-scopes set]
    authz->>KC: validate subject_token (upstream bearer)
    KC-->>authz: valid
    authz->>authz: sign with ApiKeyJwtSigner<br/>iss/aud from oauth2.signing.*
    authz-->>ga: access_token (~900s)

    ga-->>CLI: {"Authorization": "Bearer {authz token}"}
    CLI->>OTEL: POST /v1/traces
    OTEL->>OTEL: iss/aud now match aiCliOtel.oidc.*
    OTEL-->>CLI: ✅ 200 {"partialSuccess":{}}
```

Why this works with **no collector change**: the exchange is signed by the
same `ApiKeyJwtSigner` that mints today's API keys, and a token with
`iss: auth.ai.camer.digital` / `aud: lightbridge-api-key` is already verified
to return 200 from this collector.

What `governance-auth` sends on the exchange request, and what it doesn't:

- **No `project_id`.** Required by this deployment until upstream PR #309
  merged; now optional, so the exchange resolves to the subject's own
  auto-provisioned default project. There is no `--exchange-project-id` flag
  — adding one would just reintroduce a required field the server itself
  dropped.
- **No `audience`/`resource`.** This deployment's exchange handler never
  reads it — the minted token's `aud`/`azp` are always exactly the
  requesting `client_id` regardless of what's sent, so a config knob for it
  would silently do nothing.
- **No caching, no independent refresh.** `emit_token` calls `exchange::run`
  fresh on every `token`/`otel-headers` invocation when exchange is on —
  there is no cached exchanged token with its own expiry, and no separate
  `refresh_token` grant against authz. Each invocation re-derives the
  exchanged token from the CURRENT upstream (Keycloak) access token,
  refreshing that upstream token first only if it's stale (the same
  `current_session` refresh cycle every other flow already uses). The 240s
  `otelHeadersHelper` debounce is what bounds the request rate against
  authz, not caching.

---

## Telemetry auth (static)

Matrix row: **Telemetry auth, static**. This is the path that **works today**.

```mermaid
sequenceDiagram
    autonumber
    participant Dev as Developer
    participant ga as governance-auth configure
    participant CX as Codex / VS Code
    participant OTEL as aiCliOtel collector
    participant Alloy

    Dev->>ga: configure --otel-token {lightbridge-authz API key}

    alt Codex
        ga->>CX: write [otel.exporter.otlp-http.headers]<br/>Authorization = "Bearer …"
        Note over ga,CX: ⚠️ otel.exporter is a TAGGED ENUM.<br/>`exporter = "otlp-http"` is valid TOML, matches the<br/>published reference, and makes Codex REFUSE TO START.
    else VS Code Copilot
        ga-->>Dev: cannot write it — no settings key exists
        Note over ga,Dev: ⚠️ OTEL_EXPORTER_OTLP_HEADERS is the only<br/>mechanism, and it is GLOBAL to the shell: every OTLP<br/>exporter started there sends the header everywhere.
        Dev->>Dev: export it from shell rc (0600 credential file)
    end

    CX->>OTEL: POST /v1/traces  Authorization: Bearer …
    OTEL->>OTEL: iss auth.ai.camer.digital / aud lightbridge-api-key ✅
    OTEL->>Alloy: OTLP/gRPC :4317
    OTEL-->>CX: ✅ 200 {"partialSuccess":{}}

    Note over OTEL,Alloy: ✅ Verified live: a real span pushed with an API key<br/>returned 200 and reached Alloy with governance.source=ai-cli.
```

Fail-closed behaviour, also verified live: an unauthenticated POST returns
**401**, and a malformed bearer token returns **401**.

---

## Written by `governance-auth configure`

Matrix row: **Written by `governance-auth configure`**.

```mermaid
sequenceDiagram
    autonumber
    participant Dev as Developer
    participant ga as governance-auth configure
    participant FS as dotfiles

    Dev->>ga: configure [--gateway-url …] [--otel-token …]

    ga->>FS: read existing configs
    Note over ga,FS: Merged, never rewritten — settings.json carries a<br/>developer's theme/permissions, config.toml their<br/>project trust levels and comments.

    alt --gateway-url given
        ga->>FS: Claude: apiKeyHelper + ANTHROPIC_BASE_URL
        Note over ga,FS: Written as a PAIR or not at all — an apiKeyHelper<br/>minting gateway tokens while the base URL still points<br/>at api.anthropic.com would ship a Keycloak token there.
        ga->>FS: Codex: [model_providers.governance] + auth.command (ABSOLUTE path)
        Note over ga,FS: Block is written with an inline "inert" comment:<br/>codex-cli only accepts wire_api="responses" and<br/>/v1/responses 404s upstream.
    end

    ga->>FS: Claude: otelHeadersHelper + OTEL_* env
    ga->>FS: delete stale OTEL_EXPORTER_OTLP_HEADERS
    Note over ga,FS: ⚠️ Only ADDING keys leaves a stale static header<br/>beside the refreshing helper — the exact silent failure<br/>the helper exists to remove. Found on a real machine.

    alt VS Code settings.json contains comments (JSONC)
        ga-->>Dev: ❌ refuses to edit, prints the settings to add
        Note over ga,Dev: Stripping comments to parse would delete a<br/>developer's annotations permanently.
    end

    ga->>FS: write tmp → rename, mode 0600
    Note over ga,FS: Codex refuses to start on a malformed config, so a<br/>half-written file must never exist.
```

---

## Model context windows

Matrix row: **Model context windows**. **Not yet wired** — this is the
intended shape, not current behaviour.

```mermaid
sequenceDiagram
    autonumber
    participant CLI as Claude Code
    participant GW as Envoy AI Gateway

    Note over CLI: Today: no modelOverrides written.
    CLI->>CLI: assumes 200 000 tokens for an unknown model
    Note over CLI: ⚠️ adorsys-coder is really 196 608 — the assumption is<br/>LARGER than reality, so auto-compact lets a session run<br/>past what the model accepts.
    Note over CLI: ⚠️ The built-in default model does not exist on this<br/>gateway at all — `--model` must be passed explicitly<br/>or the run hard-fails.

    rect rgba(128,128,128,0.12)
        Note over CLI,GW: PROPOSED
        CLI->>GW: GET /v1/models/info (auth-gated)
        GW-->>CLI: per-model context_length,<br/>top_provider.max_completion_tokens, supported_parameters
        CLI->>CLI: modelOverrides ← context_length<br/>CLAUDE_CODE_MAX_OUTPUT_TOKENS ← max_completion_tokens
    end
```

Reading the window from the gateway is the point: hard-coding it into a
binary would silently rot as models change. opencode already consumes this
endpoint (`meta.modelsInfoUrl`), so it would be one source of truth, not two.

---

## Config file safely mergeable

Matrix row: **Config file is safely mergeable**. Covered by the `configure`
diagram above — the JSONC refusal branch and the tmp→rename write are the
whole mechanism.

---

## End-to-end: where each client actually stands

```mermaid
flowchart LR
    subgraph L[Developer laptop]
        CC["Claude Code"]
        CX["Codex"]
        VS["VS Code Copilot"]
    end
    GW["Envoy AI Gateway"]
    OTEL["aiCliOtel collector"]
    Alloy["Alloy → Tempo / Loki / Mimir"]

    CC -- "✅ inference (Keycloak, refreshing)" --> GW
    CX -- "❌ blocked: /v1/responses 404s upstream" --> GW
    VS -- "❌ no endpoint override" --> GW

    CC -- "❌ 401 — wrong issuer, see issue 84" --> OTEL
    CX -- "✅ static authz key" --> OTEL
    VS -- "✅ static key, shell env var only" --> OTEL
    OTEL --> Alloy
```

The asymmetry is the thing to remember: **Claude Code is the only client whose
telemetry is broken, and the only one whose inference works.**
