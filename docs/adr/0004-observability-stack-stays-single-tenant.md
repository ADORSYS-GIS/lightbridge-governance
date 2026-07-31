# ADR-0004: Leave the LGTM stack single-tenant; the governance database is the isolation boundary

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

The Foundry spec's isolation model rests entirely on `X-Scope-OrgID`: the gateway stamps a
trusted tenant ID, Tempo/Loki/Mimir isolate on it, and the acceptance test is "tenant A
cannot query tenant B".

**On this platform that header currently does nothing.** Verified in `ai-helm-values`:

```yaml
# environments/prod/values/mimir.yaml:16
multitenancy_enabled: false
# environments/prod/values/loki.yaml:21
auth_enabled: false
# environments/prod/values/tempo.yaml:7
replicas: 1                # single-binary
```

`mimir.yaml` even carries the comment "a multi-tenant fleet we don't run". So the spec's
Increment 3 and Increment 10 are not configuration tweaks on top of what is running -- they
are a migration of the observability stack that currently monitors our own production.

## Decision

**Do not enable multi-tenancy.** Combined with ADR-0001 (single-tenant deployable), there
is nothing to isolate: one installation, one tenant.

Customer telemetry flows to Tempo/Loki/Mimir for **operator** use. Everything a *user* sees
comes from the governance API and the governance database, where isolation is a single
`WHERE tenant_id = $1` in one place that can actually be tested.

A customer running their own installation makes this decision for themselves.

## Consequences

**Positive**
- Our production monitoring stack is untouched: no cutover, no data discontinuity in the
  boards we would be watching *during* that cutover.
- Customer telemetry volume never lands in the ingesters we need during an incident caused
  by that volume.
- The isolation claim we make is one we can demonstrate with a test.

**Negative**
- Grafana access to raw traces/logs is **operator-only**. A user-facing "open this
  execution's trace" deep link is not available.

**Neutral / follow-ups**
- ⚠️ Do NOT market non-enumerability as isolation. A 128-bit trace ID is unguessable, which
  is not the same as authorized, and saying otherwise is how this becomes a finding.
- If user-facing trace access is ever required, the answer is a **second** LGTM instance for
  customer telemetry -- not multi-tenanting the one that watches production.
- ⚠️ "Full prompt content: 7 days" is not achievable with the current global
  `retention_period: 90d`. Per-stream retention needs `limits_config.retention_stream`
  matched on a LABEL, so content-bearing streams must carry a distinguishing stream label.
  Design that in from the start; retrofitting means the 7-day promise silently is not kept.

## Alternatives considered

- **Enable multi-tenancy on the existing stack** -- rejected: every existing writer would
  have to start sending the header, existing blocks do not relabel themselves, and it is the
  wrong blast radius.
- **A second multi-tenant LGTM instance** -- deferred, not rejected. Correct if user-facing
  Grafana ever becomes a requirement; a whole second stack to operate until then.

## Related

- ADR-0001 (single-tenant deployable)
- ADR-0003 (the database, not the metrics stack, is the reporting surface)
