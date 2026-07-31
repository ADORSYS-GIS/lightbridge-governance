# ADR-0003: Grafana reads the governance database directly

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

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
