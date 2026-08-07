# Architecture Decision Records

Why things are the way they are. An ADR captures a decision **with its consequences and
the alternatives it beat** -- not a design document, and not a task list.

## Index

| # | Title | Status | Date |
|---|---|---|---|
| [0001](./0001-single-tenant-deployable-not-saas.md) | Ship a single-tenant deployable, not a multi-tenant SaaS | Accepted | 2026-07-31 |
| [0002](./0002-postgres-is-the-system-of-record-not-parquet-on-s3.md) | Postgres is the system of record; S3 is the raw archive | Accepted | 2026-07-31 |
| [0003](./0003-grafana-reads-postgres-directly.md) | Grafana reads the governance database directly | Accepted | 2026-07-31 |
| [0004](./0004-observability-stack-stays-single-tenant.md) | Leave the LGTM stack single-tenant; the database is the isolation boundary | Accepted | 2026-07-31 |
| [0005](./0005-one-workspace-registry-first-connectors-as-crates.md) | One workspace, registry first, connectors as crates | Accepted | 2026-07-31 |
| [0006](./0006-foundry-auth-reuses-core-gateway-and-authorino.md) | Foundry OTLP auth reuses core-gateway + Authorino | Accepted | 2026-07-31 |
| [0007](./0007-api-owns-connector-metrics-no-cache-service.md) | The API derives connector metrics from the manifest table; no cache service | Accepted | 2026-07-31 |
| [0008](./0008-money-is-integer-micro-usd.md) | Money is integer micro-USD, everywhere | Accepted | 2026-07-31 |
| [0009](./0009-cratestack-only-rest-transport-cbor-payloads.md) | cratestack is the only persistence layer; REST transport, CBOR payloads | Accepted | 2026-07-31 |
| [0010](./0010-bidirectional-scanning-request-and-response-paths.md) | Both request and response are scanned before they cross the trust boundary; incremental streaming for response path | Proposed | 2026-08-04 |

## Writing one

1. Copy `template.md` to `NNNN-short-imperative-title.md`.
2. Fill in Context, Decision, Consequences. One to two pages.
3. List the alternatives **and why they lost** -- that section is the one future readers
   actually need.
4. Open the PR with `Status: Proposed`; move to `Accepted` when the implementation lands.
5. Add a row to the table above.

## ADRs are immutable once Accepted

Do not edit the decision body of an accepted ADR. To change a decision, write a new one
that supersedes it: set the old status to `Superseded by ADR-NNNN`, add a one-paragraph
note at the top saying what changed, and leave the original body alone. The record of what
we believed and why is the point.

## Relationship to RFCs

An **RFC** proposes and specifies something in detail, and can be revised. An **ADR**
records a decision and freezes. An RFC usually produces one or more ADRs; the ADR links
back to it. See [`../rfc/README.md`](../rfc/README.md).
