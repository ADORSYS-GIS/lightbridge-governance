# lightbridge-governance

Governance for AI usage across providers: who is using what, what it costs, which seats are
idle, and whether anything sensitive is leaving the building.

Two connectors today:

| Connector | Shape | Source |
|---|---|---|
| **GitHub Copilot** | pull — polls GitHub's daily aggregated reports | [RFC-0001](docs/rfc/0001-github-copilot-connector.md) |
| **Microsoft Foundry** | push — authenticated OTLP from hosted agents | [RFC-0002](docs/rfc/0002-microsoft-foundry-otlp-ingestion.md) |

Both normalize into one provider-agnostic model in Postgres. **The dashboards are the
product** — Grafana reads that database directly ([ADR-0003](docs/adr/0003-grafana-reads-postgres-directly.md)).

## Layout

```text
crates/governance-core       registry, credentials, normalized model, money
crates/governance-copilot    the pull connector
crates/governance-foundry    the push connector
app/lightbridge-governance   API server        (bin)
app/governance-ctl           collector CLI     (bin)
migrations/                  sqlx migrations
charts/                      Helm chart, published to OCI on merge
docs/{adr,rfc,runbooks}/     why, what, and what-to-do-at-3am
```

## Getting started

```bash
just up          # local Postgres
just migrate
just all-checks  # fmt + clippy -D warnings + check + test
```

## Where things are decided

Start at [`docs/adr/README.md`](docs/adr/README.md). The load-bearing ones:

- [ADR-0001](docs/adr/0001-single-tenant-deployable-not-saas.md) — single-tenant deployable, not SaaS. A customer runs their own install.
- [ADR-0002](docs/adr/0002-postgres-is-the-system-of-record-not-parquet-on-s3.md) — Postgres is the system of record; S3 is the raw archive.
- [ADR-0004](docs/adr/0004-observability-stack-stays-single-tenant.md) — the LGTM stack stays single-tenant; the database is the isolation boundary.
- [ADR-0006](docs/adr/0006-foundry-auth-reuses-core-gateway-and-authorino.md) — reuse core-gateway + Authorino; build no auth service.

## Status

Scaffold. The workspace builds, the decisions are recorded, and the connectors are
specified but not implemented. RFC-0001 has one blocking unknown — whether GitHub App
installation tokens work against the Copilot report endpoints — which is a spike, not a
design question.

## Licence

Apache-2.0.
