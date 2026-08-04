# Codex Telemetry Rollout

This document specifies the configuration for OpenAI Codex telemetry emission to
`otel.ai.camer.digital` and the critical step of disabling the default statsig exporter.

## Purpose

Codex ships with `metrics_exporter` defaulting to `statsig`, meaning metrics are sent to OpenAI
unless explicitly disabled. This rollout accomplishes two goals:

1. **Stop the outflow**: Disable statsig on all installs (governance finding)
2. **Enable ingestion**: Configure OTLP export to our governance endpoint

## Coverage Limitation

⚠️ **Advisory, not enforceable**: Unlike Claude Code, Codex's admin configuration (`requirements.toml`)
cannot pin the `[otel]` section. Developers can modify their `~/.codex/config.toml` to change or
disable telemetry settings. Coverage is best-effort, not guaranteed.

This means:
- Dashboards showing "Codex spend per engineer" measure users who have telemetry enabled
- Epic #30's Codex success metric should be expressed as a proportion of participating users
- The statsig disable is a governance finding that ships independently of telemetry ingestion

## Configuration

### Step 1: Disable Statsig (Critical)

This must ship even if telemetry ingestion is delayed. It stops metrics from leaving to OpenAI.

Add to `~/.codex/config.toml`:

```toml
[analytics]
enabled = false
```

Or explicitly set the metrics exporter:

```toml
[otel]
metrics_exporter = "none"
```

### Step 2: Enable OTLP Export (Telemetry Ingestion)

Add to `~/.codex/config.toml`:

```toml
[otel]
environment = "production"
exporter = "otlp-http"
metrics_exporter = "none"  # Redundant with [analytics] enabled = false, but explicit
trace_exporter = "otlp-http"

[otel.exporter.otlp-http]
endpoint = "https://otel.ai.camer.digital"
protocol = "json"
headers = { Authorization = "Bearer <per-developer-token>" }
```

### Complete Configuration

The complete `~/.codex/config.toml` for telemetry:

```toml
# Disable OpenAI's default analytics
[analytics]
enabled = false

# Configure OTLP export to governance endpoint
[otel]
environment = "production"
exporter = "otlp-http"
metrics_exporter = "none"
trace_exporter = "otlp-http"
log_user_prompt = false  # Never enable - privacy policy

[otel.exporter.otlp-http]
endpoint = "https://otel.ai.camer.digital"
protocol = "json"
headers = { Authorization = "Bearer <per-developer-token>" }
```

## Token Issuance

Each developer receives a **per-developer ingest token** issued by the governance registry.
The token:

- Binds identity to the token's subject (Keycloak `sub`), not the payload's `user.email`
- Enables server-side identity resolution via `identity_maps`
- Triggers mismatch alerts if the payload's `user.email` disagrees with the token's subject

Token issuance and identity binding are implemented in Story #35.

## Distribution Channels

1. **Coder workspace template** (recommended): Inject the config into the workspace template.
   Developers on Coder workspaces get it automatically.

2. **Dotfiles** (advisory): Distribute via `~/.codex/config.toml` for developers running Codex
   on personal machines. This is advisory and can be overridden.

3. **Manual**: Developers can manually add the configuration to their `~/.codex/config.toml`.

## Identity Considerations

Codex identity attributes (`user.email`, `user.account_id`) are populated **only under ChatGPT
sign-in**. Under API-key or custom-provider auth, these fields are `None`.

This makes the per-developer ingest token **load-bearing** for Codex:
- Identity comes from the token, not the payload
- The payload's `user.email` is a cross-check, not the source of truth
- Mismatch alerts fire if the payload email disagrees with the token subject

## Token Counts and codex exec

⚠️ **Known limitation**: `codex exec` does not export the `codex.turn.token_usage` metric
([openai/codex#33668](https://github.com/openai/codex/issues/33668), open). Token counts appear
only as span/log attributes (`input_token_count`, `output_token_count`, etc.).

The Codex normalizer extracts token counts from span attributes, so `codex exec` runs are
captured correctly. However, this is a workaround for an upstream bug.

## Content Capture

**Off by default.** The configuration sets `log_user_prompt = false`. Token counts and cost
calculation need no prompt content. Enabling content capture would violate RFC-0002's
`metadata_only` default.

## Verification

### Manual Test: Statsig Disabled

1. Apply the configuration to a developer's machine
2. Run `codex` and perform some operations
3. Check network traffic or Codex logs to verify no metrics are sent to statsig.com
4. Verify metrics are sent to `otel.ai.camer.digital` instead

Expected: No outbound traffic to statsig.com; telemetry arrives at governance endpoint.

### Manual Test: End-to-End Attribution

1. Issue a per-developer ingest token for `dev@example.com`
2. Apply the configuration with the token in the headers
3. Use Codex in a session (interactive or exec)
4. Verify in the governance dashboard:
   - Execution record appears with `internal_user_id` = Keycloak `sub`
   - `user.email` from payload matches token subject (no mismatch alert)
   - Cost is calculated in integer micro-USD from token counts and model pricing

### Manual Test: codex exec Token Counts

1. Run `codex exec "list files in current directory"`
2. Verify the execution record includes token counts
3. Verify cost is calculated (not unknown)

Expected: Token counts extracted from span attributes; cost calculated correctly.

### Manual Test: Mismatch Alert

1. Issue a token for `dev@example.com`
2. Modify the configuration to use a different token (or simulate a payload with a different
   `user.email`)
3. Verify the governance API logs a mismatch warning

## Dependencies

- **Story #31**: Provider-agnostic OTLP ingest (generalized push connector)
- **Story #35**: Per-developer ingest tokens with identity binding and mismatch alerting
- **Story #36**: Grafana dashboards for per-engineer spend

## References

- [Codex Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference)
- [Codex Advanced Configuration](https://learn.chatgpt.com/docs/config-file/config-advanced)
- `docs/rfc/sources/claude-codex-usage-investigation.md` §4.3
- `docs/spikes/spike-0008-codex-otel-admin-config.md` (spike #34 findings)
- Story #33 acceptance criteria