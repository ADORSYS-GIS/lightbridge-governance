# ADR-0007: The API derives connector metrics from the manifest table; no cache service

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

Two smaller decisions that share a root cause -- the specs assume infrastructure this
platform does not have.

**Metrics.** Both specs describe the collector emitting its own operational metrics. But
metrics on this platform are **pull-based**: Alloy discovers `ServiceMonitor`/`PodMonitor`
CRDs cluster-wide and remote-writes to Mimir. OTLP to Alloy carries traces, not first-party
metrics. A CronJob pod that exits cannot be scraped.

**Cache.** Both specs specify Memcached. There is no memcached in this cluster. The only
cache already deployed is redis-ha, which is TLS-only with an internal CA and a password --
real wiring for a single-replica API serving data that refreshes every six hours.

## Decision

**Metrics:** the collector records every run outcome in `ingest_manifest`. The
always-running **API** derives the `governance_connector_*` gauges and counters from that
table and exposes them on `/metrics`, scraped by one `ServiceMonitor`.

**Cache:** no cache service. In-process `moka` in the API, for the token->tenant resolution
(ADR-0006) and for common query responses.

## Consequences

**Positive**
- Metrics are always scrapeable, including while the CronJob is not running -- which is
  exactly when `last_success_timestamp` and `report_age_seconds` matter most.
- Those two are *derived* from stored state rather than remembered by a process, so they
  survive a restart and cannot drift from the data.
- No pushgateway, and no stale-metric semantics to reason about.
- No new component, no NetworkPolicy for it, no ADR to justify it.

**Negative**
- The API must be up for connector health to be visible. It is also the thing that would
  page us, so an API outage is not a silent failure.
- In-process cache means cache state is per-replica. At one replica that is a non-issue;
  if the API ever scales out, revisit -- redis-ha is already wired for LibreChat and the
  rate-limit service and would be the answer.

**Neutral / follow-ups**
- Keep the metric labels to `provider`, `tenant_id`, `organization_id`, `report_type`,
  `status`. Per-user or per-repository labels belong in Postgres (ADR-0003), not here.

## Alternatives considered

- **Prometheus pushgateway** -- rejected: a new component with stale-metric semantics, to
  publish numbers we can derive from a table we already write.
- **Memcached** -- rejected: a new chart, NetworkPolicy and ADR for a single-replica cache.
- **redis-ha** -- rejected for now: TLS + internal CA + password wiring for no MVP benefit.

## Related

- ADR-0002 (`ingest_manifest` lives in Postgres), ADR-0003 (Mimir keeps only these metrics)
- Runbook: `docs/runbooks/copilot-sync-failed.md`
