# Architecture Decision Records

Why things are the way they are. An ADR captures a decision **with its consequences and
the alternatives it beat** -- not a design document, and not a task list.

## Index

| # | Title | Status | Date |
|---|---|---|---|
| [0001](./0001-single-tenant-deployable-not-saas.md) | Ship a single-tenant deployable, not a multi-tenant SaaS | Accepted | 2026-07-31 |
| [0002](./0002-postgres-is-the-system-of-record-not-parquet-on-s3.md) | Postgres is the system of record; S3 is the raw archive | Accepted | 2026-07-31 |
| [0003](./0003-grafana-reads-postgres-directly.md) | Grafana reads the governance database directly | Accepted (one clause amended by 0011) | 2026-07-31 |
| [0004](./0004-observability-stack-stays-single-tenant.md) | Leave the LGTM stack single-tenant; the database is the isolation boundary | Accepted | 2026-07-31 |
| [0005](./0005-one-workspace-registry-first-connectors-as-crates.md) | One workspace, registry first, connectors as crates | Accepted | 2026-07-31 |
| [0006](./0006-foundry-auth-reuses-core-gateway-and-authorino.md) | Foundry OTLP auth reuses core-gateway + Authorino | Accepted | 2026-07-31 |
| [0007](./0007-api-owns-connector-metrics-no-cache-service.md) | The API derives connector metrics from the manifest table; no cache service | Accepted | 2026-07-31 |
| [0008](./0008-money-is-integer-micro-usd.md) | Money is integer micro-USD, everywhere | Accepted | 2026-07-31 |
| [0009](./0009-cratestack-only-rest-transport-cbor-payloads.md) | cratestack is the only persistence layer; REST transport, CBOR payloads | Accepted | 2026-07-31 |
| [0010](./0010-bidirectional-scanning-request-and-response-paths.md) | Both request and response are scanned before they cross the trust boundary; incremental streaming for response path | Proposed | 2026-08-04 |
| [0010](./0010-governance-auth-keycloak-oauth2-credential-helper.md) ⚠️ | `governance-auth`, a Keycloak OAuth2 credential helper for Claude Code / Codex | Proposed | 2026-08-08 |
| [0011](./0011-bridge-copilot-run-metrics-push-to-pull.md) | Bridge copilot-sync's run-detail metrics from push to pull with a dedicated collector | Proposed | 2026-08-07 |
| [0012](./0012-governance-auth-packaging-and-distribution.md) | `governance-auth` on-disk layout, packaging and distribution | Proposed | 2026-08-14 |
| [0013](./0013-telemetry-ingest-invariants-and-the-declaration-gate.md) | Bind every telemetry source to six ingest invariants, declared before implementation | Proposed | 2026-08-27 |
| [0014](./0014-usage-telemetry-consolidates-into-the-authz-usage-store.md) | Usage telemetry consolidates into the authz usage store — this repo keeps the collectors, not the tables | Accepted | 2026-08-31 |
| [0015](./0015-pin-the-loopback-callback-to-a-registered-port-block.md) | Pin the loopback callback to a registered port block until RFC 8252 §7.3 lands upstream | Accepted | 2026-08-31 |

⚠️ **Number collision**: `0010` is used by two ADRs. Both are listed above so
neither is invisible, but the number needs resolving — renumbering changes
every inbound link, so it is deliberately left as a maintainer decision
(ADR-0012, open question 7) rather than done unilaterally here.

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
