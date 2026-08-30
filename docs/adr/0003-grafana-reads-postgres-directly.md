# ADR-0003: Grafana reads the governance database directly

- Status: Accepted -- one scoping clause amended by [ADR-0011](./0011-bridge-copilot-run-metrics-push-to-pull.md)
- Date: 2026-07-31
- Decision owners: @stephane-segning

> **Amendment note (2026-08-31, [ADR-0014](./0014-usage-telemetry-consolidates-into-the-authz-usage-store.md)).**
> For telemetry data, "the governance database" Grafana reads is now the `lightbridge-authz` usage
> store. This ADR's reasoning stands unchanged — dashboards read business/reporting data from
> Postgres rather than from Mimir, and the cardinality argument is unaffected. Only the target
> moved. The body below is left exactly as written.

> **Amendment note (2026-08-07, ADR-0011).** One sentence in the Decision below is no
> longer true as written: "Mimir keeps only the ~10 low-cardinality
> `governance_connector_*` operational metrics ... and nothing else." ADR-0011 adds a
> second family, `governance_copilot_*` (8 series, labels limited to
> `command`/`report`/`status`), carrying per-run detail that is not reconstructible from
> `ingest_manifests` and therefore cannot be derived the way ADR-0007 derives
> `governance_connector_*`.
>
> Everything else in this ADR stands unchanged and is still the decision: business and
> reporting data is read from Postgres by Grafana rather than published to Mimir, the
> cardinality argument that motivates that is unaffected, and **alerting still belongs on
> `governance_connector_*`** -- the new family is dashboard-grade only, because it lives
> in a collector's in-memory cache and is blanked by a restart. The body below is left
> exactly as written, per this directory's rule that an accepted ADR's decision is not
> edited in place.

## Context

Both source specs route their reporting through Prometheus/Mimir, and both then spend
significant effort working around the consequence: "keep GitHub usernames out of
Prometheus labels", "avoid labels such as username, repository, team, model, session_id".
That constraint is real -- per-user labels are a cardinality bomb -- but it exists only
because the data was being pushed through a metrics system in the first place.

The product is the dashboards. The platform already has the precedent for this exact
move: ai-helm ADR-0063 runs a read-only Postgres `GrafanaDatasource` (`uid: keycloak`)
against a CNPG replica so dashboards can resolve opaque user IDs to people.

## Decision

Expose the governance database to Grafana as a **read-only Postgres
`GrafanaDatasource`** (`uid: governance`), pointed at the CNPG `-ro` replica. Business
dashboards -- adoption, seats, licence hygiene, executions, policy violations, cost --
are SQL panels over it.

**Mimir keeps only the ~10 low-cardinality `governance_connector_*` operational metrics.**
Those drive the connector-health dashboard and the alerts, and nothing else.

The Grafana role gets `SELECT` and nothing else, on the reporting tables and nothing else.

## Consequences

**Positive**
- The entire low-cardinality-label constraint disappears for business reporting. Usernames,
  repositories, teams, models and application IDs become *columns*, which is what they are.
- No metric-publishing step, so no second copy of the data to keep consistent.
- Filters, joins and pagination are SQL rather than PromQL contortions.

**Negative**
- Dashboard panels become SQL, which is not portable to a Prometheus-only install. Given
  ADR-0001 (we ship the whole deployable), that is not a real constraint.
- Grafana needs egress to the database. The `observability` namespace is default-deny, so
  this needs a `CiliumNetworkPolicy` -- the same overlay edit ADR-0063 already made.

**Neutral / follow-ups**
- Alerting stays on Mimir via `governance_connector_*`. A "no violations" panel and a
  "no telemetry" panel must be visibly different, or an outage reads as a clean bill of health.

## Alternatives considered

- **Publish everything to Mimir** -- rejected: forces the cardinality workarounds the
  specs spend pages on, and still cannot answer "which repositories did this team use".
- **A bespoke web UI over the API** -- rejected for the reporting surface; four of the
  five views the Foundry spec asks for are tables and time series, which Grafana already is.

## Related

- ADR-0002 (Postgres is the system of record)
- ADR-0007 (why the API, not the CronJob, owns the operational metrics)
- ai-helm ADR-0063 (the `keycloak` read-only datasource precedent)
