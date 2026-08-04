# Claude Code Managed Settings for Telemetry Rollout

This document specifies the managed settings configuration that enforces Claude Code telemetry
emission to `otel.ai.camer.digital`, ensuring per-developer usage attribution without requiring
opt-in.

## Purpose

Claude Code's telemetry is **admin-enforceable** via managed settings. Unlike Codex (where `[otel]`
is user-controlled), Claude Code's managed settings can pin the telemetry `env` block such that
developers cannot override it. This is what makes Story #32 viable: coverage is enforced, not
advisory.

## Configuration

### Managed Settings Template

The following configuration must be distributed via Coder workspace templates (enforced) or
dotfiles (advisory):

```json
{
  "env": {
    "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
    "OTEL_METRICS_EXPORTER": "otlp",
    "OTEL_LOGS_EXPORTER": "otlp",
    "OTEL_EXPORTER_OTLP_PROTOCOL": "http/protobuf",
    "OTEL_EXPORTER_OTLP_ENDPOINT": "https://otel.ai.camer.digital",
    "OTEL_EXPORTER_OTLP_HEADERS": "Authorization=Bearer <per-developer-token>"
  }
}
```

### Token Issuance

Each developer receives a **per-developer ingest token** issued by the governance registry
(Story #10). The token:

- Binds identity to the token's subject (Keycloak `sub`), not the payload's `user.email`
- Enables server-side identity resolution via `identity_maps`
- Triggers mismatch alerts if the payload's `user.email` disagrees with the token's subject

Token issuance is documented in Story #35.

### Distribution Channels

1. **Coder workspace template** (enforced): Inject the managed settings into the workspace
   template at `charts/apps/values.yaml` or equivalent. Developers cannot override enforced
   settings.

2. **Dotfiles** (advisory): Distribute via `~/.claude/settings.json` for developers running
   Claude Code on personal machines. This is advisory and can be overridden.

## Verification

### Manual Test: Override Blocked

To confirm a developer cannot override the managed telemetry setting:

1. Apply the managed settings template to a developer's workspace
2. Attempt to set `CLAUDE_CODE_ENABLE_TELEMETRY=0` in the developer's shell
3. Start Claude Code and verify telemetry is still emitted
4. Check `otel.ai.camer.digital` for incoming spans with the developer's token subject

Expected: telemetry is emitted regardless of the developer's shell environment.

### Manual Test: End-to-End Attribution

1. Issue a per-developer ingest token for `dev@example.com`
2. Apply managed settings with the token in `OTEL_EXPORTER_OTLP_HEADERS`
3. Use Claude Code in a session (run commands, generate code)
4. Verify in the governance dashboard:
   - Execution record appears with `internal_user_id` = Keycloak `sub`
   - `user.email` from payload matches token subject (no mismatch alert)
   - Cost is calculated in integer micro-USD from token counts and model pricing

### Manual Test: Mismatch Alert

1. Issue a token for `dev@example.com`
2. Modify the managed settings to use a different token (or simulate a payload with a different
   `user.email`)
3. Verify the governance API logs a mismatch warning and increments
   `governance_ingest_identity_mismatch_total`

## Content Capture

**Off by default.** The managed settings do **not** enable:

- `OTEL_LOG_USER_PROMPTS`
- `OTEL_LOG_TOOL_CONTENT`
- `OTEL_LOG_TOOL_DETAILS`
- `OTEL_LOG_RAW_API_BODIES`

Token counts and cost calculation need none of these. Enabling content capture would violate
RFC-0002's `metadata_only` default and trigger Loki per-stream-retention warnings.

## Dependencies

- **Story #31**: Provider-agnostic OTLP ingest (generalized push connector)
- **Story #35**: Per-developer ingest tokens with identity binding and mismatch alerting
- **Story #36**: Grafana dashboards for per-engineer spend

## References

- [Claude Code Monitoring & Usage](https://code.claude.com/docs/en/monitoring-usage)
- [Claude Code Authentication](https://code.claude.com/docs/en/authentication)
- `docs/rfc/sources/claude-codex-usage-investigation.md` §3.3
- Story #32 acceptance criteria