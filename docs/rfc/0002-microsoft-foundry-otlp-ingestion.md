# RFC-0002: Microsoft Foundry OTLP ingestion

- Status: Draft
- Date: 2026-07-31
- Author: @stephane-segning
- Source of truth: [`sources/microsoft-foundry-governance-mvp-plan.md`](./sources/microsoft-foundry-governance-mvp-plan.md)
  (the original planning spec, copied in so it survives outside the maintainer's own machine)

## Summary

A **push** connector. A Foundry hosted agent is configured with our OTLP endpoint and an
integration token; Authorino authenticates the token, resolves it to trusted tenant context
and stamps it as headers; an OpenTelemetryCollector redacts and fans the three signals out
to Tempo/Loki/Mimir and to our own ingestion API, which normalizes them into execution,
model-call and tool-call records.

## Motivation

Hosted agents are a black box otherwise. Nobody can answer what an agent costs, how often
it fails, which models and tools it uses, or whether it is putting sensitive content into a
prompt -- and unlike Copilot there is no daily report to poll.

## Design

### Ingestion path

```text
Foundry agent
  -> https://otel.ai.camer.digital        (core-gateway, public LB, ACME HTTP-01)
  -> Authorino AuthConfig #3              (host-indexed, alongside external + internal)
       authentication: bearer integration token
       metadata.http -> /internal/v1/resolve  (X-Internal-Token shared secret)
       response.success.headers -> X-Scope-OrgID, governance.tenant.id,
                                   governance.application.id, governance.integration.id
  -> BackendTrafficPolicy                 (per-token rate limit + quota, redis-backed)
  -> OpenTelemetryCollector/foundry-gateway   (OTLP/HTTP :4318)
       memory_limiter -> redaction -> transform -> batch
  -> Tempo | Loki | Mimir | this API
```

> **Amendment (2026-09-04) — the "this API" destination was removed (#243).** The
> `/internal/v1/ingest` endpoint this diagram's "this API" referred to was deleted in #243 —
> nothing had ever called it. When RFC-0002 ships, it must design its **own** authenticated
> endpoint (e.g. `kubernetesTokenReview`) rather than re-adding the shared-secret route this
> diagram predates; #243's technical context explains why that is less work than retrofitting
> the removed one. The `governance_core::ingest::ingest_telemetry` persistence entry point and
> the `governance-foundry` normalizer pipeline still exist and are the intended write path for
> that future endpoint, but are currently reachable only from tests.

Increment 2 of the source spec is therefore **already built** (ADR-0006). We add a host, an
AuthConfig and a collector -- not an authentication service.

### Collector

`memory_limiter` **must be first** in every pipeline: it sheds load before an OOM, and an
OOM loses data outright. `mode:` is **required** on the CR (the v1beta1 webhook rejects an
empty mode with a confusing message) and is **immutable** -- changing it means deleting the
CR once so ArgoCD recreates it. Both are recorded in ai-helm ADR-0034 and
`charts/core-gateway/templates/otel.yaml`.

Three replicas, PDB, anti-affinity, strict body-size limits.

### Privacy (release blocker, not an enhancement)

Three capture modes -- `metadata_only` (default), `redacted`, `full`. The mode is resolved
from the **integration record** and arrives at the collector as a trusted header, so
redaction happens **before** data reaches Tempo/Loki, not after.

### Normalization

Execution / model-call / tool-call records, each keeping `trace_id`, `span_id`,
`raw_backend`, `raw_schema_version` so an operator can jump to the trace. Money in integer
micro-USD (ADR-0008). Do not couple the product to raw OpenTelemetry GenAI attribute names
-- those conventions are still moving.

### Tenancy

Single tenant (ADR-0001, ADR-0004). Telemetry reaches the LGTM stack for **operator** use;
users see the governance database. `X-Scope-OrgID` is stamped anyway so a customer install
that does enable multi-tenancy needs no code change.

## Verification

- Unauthenticated OTLP returns 401; a revoked token stops working within one cache TTL.
- One agent execution produces a searchable trace, correlated logs, token/duration metrics
  and exactly one normalized execution record.
- A prompt containing a seeded secret reaches Tempo **redacted**, verified by reading the
  stored span rather than by trusting the processor's own counter.
- The golden-dataset fixture replays through the real collector config in CI on every
  change to collector config, normalization, pricing or policy logic.

## Risks and unknowns

- ⚠️ A `sharedSecretRef` to a missing Secret fails AuthConfig readiness and **404s the whole
  gateway**. Secret first, `SecretSynced=True` confirmed, then the AuthConfig.
- ⚠️ The new listener must be added to the SecurityPolicy's `sectionNames` or it is
  silently unauthenticated.
- ⚠️ Changing a Foundry agent's OTLP env vars **requires publishing a new agent version**.
  So endpoint and token must be long-lived, with server-side revocation -- not short-lived
  tokens with rotation.
- ⚠️ "Full content: 7 days" is not achievable under the current global Loki
  `retention_period: 90d`. Per-stream retention matches on a **label**, so content-bearing
  streams need a distinguishing stream label designed in from the start (ADR-0004).
- Use Loki's **native OTLP endpoint** with structured metadata. Do not copy the existing
  Envoy-access-log path (`otelcol.exporter.loki`), which stores the line as
  `{"attributes":...,"resources":...}` -- that shape cost a lot of debugging (ai-helm ADR-0046).

## Open questions

1. Does the Foundry OTLP client handle a 401 with a JSON body, or does it need a specific
   challenge shape? Thirty minutes against a real hosted agent settles it.
2. Does the installed Mimir version support native OTLP ingestion, or do we fall back to
   `prometheusremotewrite`?
3. Where does the integration-setup page live -- a server-rendered page on this API, or a
   screen in `lightbridge-ss`?

## Decisions produced

- [ADR-0004](../adr/0004-observability-stack-stays-single-tenant.md)
- [ADR-0006](../adr/0006-foundry-auth-reuses-core-gateway-and-authorino.md)
- [ADR-0008](../adr/0008-money-is-integer-micro-usd.md)
