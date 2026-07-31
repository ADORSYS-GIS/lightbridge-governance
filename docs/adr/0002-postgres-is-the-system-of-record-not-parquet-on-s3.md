# ADR-0002: Postgres is the system of record; S3 is the raw archive

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

The Copilot source spec specifies "no new database": keep detailed data as partitioned
Parquet on S3, have the API query the relevant partitions, and cache responses in
Memcached. That is a reasonable design for a greenfield platform. It is the wrong design
*here*, because the premise -- that Postgres would be a new dependency -- is false.

The platform already runs a CloudNativePG cluster (`lightbridge-main-db`) carrying six
per-app roles (`repoauth`, `codeintel`, `coder`, `lakefs`, `mlflow`, `grafana_ro`), each
added the same way: a role, a database, an ExternalSecret. Barman backups to S3 are
already configured for it.

## Decision

Add a `governance` role and database to the existing `lightbridge-main-db` cluster and
make **Postgres the system of record**. S3 keeps the **raw archive** only: every source
object exactly as fetched, so any day can be replayed without calling the provider again.

## Consequences

**Positive**
- Deletes an entire subsystem: no Parquet writer, no Arrow/object_store/DataFusion in the
  dependency tree, no hand-rolled partition-pruning query layer, and no separate
  `normalize` and `publish-metrics` commands.
- Idempotent reprocessing becomes `INSERT ... ON CONFLICT DO UPDATE`, which is what the
  spec's "deterministic keys and overwrite-safe processing" was describing.
- Server-side filtering and pagination are ordinary SQL.
- Unlocks ADR-0003 -- Grafana reads it directly.
- Sizing is trivial: ~500 seats x 365 days is ~200k rows/year on the largest table.

**Negative**
- The governance data shares a Postgres cluster with other workloads. Mitigated by the
  per-role isolation already in use, and the volume is negligible next to the existing
  tenants of that cluster.

**Neutral / follow-ups**
- If query volume ever outgrows it, the S3 raw archive means an analytical store can be
  built from replay without a migration.

## Alternatives considered

- **Parquet on S3 + DataFusion** -- rejected: a query engine we would own and operate, to
  avoid a dependency we already have.
- **A dedicated Postgres for governance** -- rejected: a new cluster to back up and patch
  for ~200k rows a year.

## Related

- ADR-0003 (Grafana reads this database directly)
- ADR-0008 (money is integer micro-USD)
- ai-helm `charts/lightbridge-db` -- where the role and database are declared
