# Architecture

The governance platform ingests AI usage from several providers, normalizes it into one
model, and reports on it. Two connectors today: **GitHub Copilot** (pull) and **Microsoft
Foundry** (push).

## Map

```text
                 ┌──────────────────────── governance namespace ────────────────────────┐
GitHub API       │                                                                      │
  (daily         │   CronJob/copilot-sync ──► S3 raw archive                             │
   reports) ────►│     (governance-ctl)   └─► Postgres  ◄──┐                             │
                 │                                          │                            │
Foundry agent    │   OpenTelemetryCollector ──► Tempo/Loki/Mimir  (operator-only)        │
  (OTLP) ───────►│     /foundry-gateway     └─► Deployment/lightbridge-governance ───────┼─► Postgres
                 │                                (API + /metrics + /internal/v1/resolve)│
                 └──────────────────────────────────────────────────────────────────────┘
                                    ▲                                  │
core-gateway ── Authorino ──────────┘                                  ▼
 (TLS, token auth, rate limit)                            Grafana ◄── governance datasource
                                                            (the product — ADR-0003)
```

## Components

| Component | What it is | Decision |
|---|---|---|
| `governance-core` | Registry, credentials, normalized model, `MicroUsd`. Owns `schema/governance.cstack` | ADR-0005, ADR-0008, ADR-0009 |
| `governance-copilot` | Pull connector for GitHub's daily reports | RFC-0001 |
| `governance-foundry` | Push connector: resolve handler + GenAI normalizer | RFC-0002 |
| `lightbridge-governance` | API server. Also derives `governance_connector_*` | ADR-0007 |
| `governance-ctl` | Collector CLI: `sync`, `sync-day`, `replay`, `verify`, `status` | RFC-0001 |
| Postgres (`governance` role) | **System of record**, via cratestack only | ADR-0002, ADR-0009 |
| S3 (`lightbridge-governance/raw/`) | Immutable raw archive, for replay | ADR-0002 |
| Grafana `governance` datasource | The reporting surface | ADR-0003 |
| Mimir | `governance_connector_*` only — health and alerts | ADR-0003, ADR-0007 |

## Things that are deliberately absent

- **No Parquet query layer.** Postgres is the store (ADR-0002).
- **No cache service.** In-process `moka` (ADR-0007).
- **No bespoke OTLP auth service.** core-gateway + Authorino already do it (ADR-0006).
- **No backfill Job.** `sync` self-heals from the high-water mark (RFC-0001).
- **No multi-tenancy in the LGTM stack.** The database is the boundary (ADR-0004).
- **No web console.** Four of five views are Grafana; the fifth is one page (RFC-0002).
- **No hand-written SQL or migrations.** The `.cstack` schema generates them (ADR-0009).

## Wire format

REST routes, **CBOR** payloads (`application/cbor`). The one exception is
`/internal/v1/resolve`: Authorino's `metadata.http` step posts and parses JSON and cannot be
taught CBOR, so that endpoint speaks JSON and only that endpoint does (ADR-0009).

## Where the deployed state lives

Charts are **in this repository** and publish to OCI on merge, as `lightbridge-authz` does.
ai-helm consumes them; per-environment values live in the private `ai-helm-values` repo.

⚠️ **Values-repo-first.** The values file must exist on `ai-helm-values@main` before the
ai-helm change merges, or `ignoreMissingValueFiles` silently falls back to chart defaults.
