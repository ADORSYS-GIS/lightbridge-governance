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

### Managed Settings File

Claude Code's **managed settings** live in a machine-global file that sits at the top of the
settings precedence chain and **cannot be overridden** by project or user settings:

- Linux / WSL: `/etc/claude-code/managed-settings.json`
- macOS: `/Library/Application Support/ClaudeCode/managed-settings.json`
- Windows: `C:\ProgramData\ClaudeCode\managed-settings.json`

It is the only channel that is genuinely enforced. The following configuration must be written
to that file (via Coder workspace templates, see [Distribution Channels](#distribution-channels)):

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

Precedence (highest to lowest): managed settings → project settings (`.claude/settings.json`)
→ user settings (`~/.claude/settings.json`). Only the managed file is enforced; anything below
it is advisory and overridable.

### Token Issuance

Each developer receives a **per-developer ingest token** issued by the governance registry.
The token:

- Binds identity to the token's subject (Keycloak `sub`), not the payload's `user.email`
- Enables server-side identity resolution via `identity_maps`
- Triggers mismatch alerts if the payload's `user.email` disagrees with the token's subject

Token issuance and identity binding are implemented in Story #35.

### Token Placement: Threat Model

`OTEL_EXPORTER_OTLP_HEADERS` carries the per-developer token in the process environment, which
shapes where this configuration is safe:

- The managed-settings file is root-owned on a shared host, but the token is **not** a secret
  once Claude Code runs: it appears in `OTEL_EXPORTER_OTLP_HEADERS` in the process environment,
  is readable via `/proc/<pid>/environ`, and is inherited by every subprocess Claude Code
  spawns. Anyone who can run a process as the same user can read it.
- This is acceptable where **one machine equals one developer** — Coder workspaces satisfy
  this, which is why the enforced channel is the workspace template.
- On a **shared host** with multiple developers, a per-developer token in a machine-global file
  is a credential readable by every user on the box. Do not deploy per-developer tokens there;
  fall back to a per-machine token with reduced blast radius, or accept that attribution is per-
  machine rather than per-developer.

The token is an ingest credential: it authorizes writing telemetry, nothing else. Treat it as
sensitive anyway — a leaked token lets an attacker fabricate attributed usage.

### Distribution Channels

1. **Coder workspace template** (enforced): Write the managed settings to the managed-settings
   file path above via the workspace template at `charts/apps/values.yaml` or equivalent.
   Because it lands in the managed file, developers cannot override it. This is the only
   channel that satisfies the enforcement claim in [Purpose](#purpose).

2. **Dotfiles** (advisory, NOT enforced): Distributing the same block via `~/.claude/settings.json`
   is user settings, which a developer can override. It is useful for personal machines but does
   **not** deliver the coverage guarantee — a developer following this channel ends up with
   precisely the non-enforced configuration the story exists to avoid. Do not treat dotfiles as a
   substitute for the managed file.

## Verification

### Manual Test: Override Blocked

To confirm a developer cannot override the managed telemetry setting:

1. Apply the managed settings template to a developer's workspace
2. Attempt to set `CLAUDE_CODE_ENABLE_TELEMETRY=0` in the developer's shell
3. Start Claude Code and verify telemetry is still emitted
4. Check `otel.ai.camer.digital` for incoming telemetry with the developer's token subject

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
3. Verify the governance API logs a mismatch warning

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