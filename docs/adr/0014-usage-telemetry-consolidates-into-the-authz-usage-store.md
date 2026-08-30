# ADR-0014: usage telemetry consolidates into the authz usage store — this repo keeps the collectors, not the tables

- Status: Accepted
- Date: 2026-08-31
- Decision owners: Stephane Segning Lambou
- Resolves: [#182](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/182) (this repo's
  half of the store-boundary decision) and, jointly with
  [lightbridge-authz ADR-0027](https://github.com/ADORSYS-GIS/lightbridge-authz/blob/main/docs/adr/0027-one-usage-store-partitioned-by-grain.md)
  (via lightbridge-authz#535), the contradiction between this repo's RFC-0003 §6 and
  lightbridge-authz#491.
- Amends: RFC-0003 §6 (corrected in this change), ADR-0002 and ADR-0003 (amendment notes added in
  this change).

## Context

RFC-0003 §6 declared a split store boundary: gateway request telemetry in `lightbridge-authz`,
IDE and vendor-platform telemetry here — while flagging, honestly, that lightbridge-authz#491
asserts the opposite allocation and that the decision was unmade. Meanwhile this repo built a
grain-partitioned `executions`/`model_calls`/`tool_calls` hierarchy plus vendor-named Copilot day
tables (already condemned by [#167](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/167)),
on a CNPG cluster with no TimescaleDB ([#159](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/159))
— the same foundation work lightbridge-authz#489 needs next door. Epic
[#161](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/161) said it plainly: "if the
boundary resolves the other way, this epic moves repositories."

The repo owner resolved it the other way on 2026-08-31.

## Decision

### 1. The authz usage store is the system of record; this repo's telemetry store is decommissioned

All AI-usage telemetry — including everything this repo collects — lands in the
`lightbridge-authz-usage` database, in one hypertable family per grain (request / execution / day /
seat) with `source` as a dimension column. ADR-0013's invariants are adopted there verbatim;
invariant 1 ("grain partitions storage, vendor never does") governs the target schema, which is
what #167 asked for — delivered in the consolidated store rather than here.

### 2. This repo keeps the collectors, and they become clients of the usage ingest surface

`governance-ctl` (Copilot pull), `governance-auth` (client config distribution), `redact-extproc`,
and the `aiCliOtel` collector chart all stay here. `governance-ctl` stops writing its own Postgres
and POSTs day-grain facts and seat snapshots to the authenticated usage ingest API; its S3 raw
archive and `replay` subcommand are unchanged and become the backfill mechanism. The push
connector's per-provider normalizer design (RFC-0002, generalised per #30) is ported to the usage
ingest surface — including its dispatch-by-source registry and the identity-from-credential rule
(ADR-0013 invariant 2).

### 3. Existing data migrates; nothing is silently dropped

Copilot dailies are replayed from the S3 raw archive through the new day-grain ingest path
(idempotent by `(source, day, subject_kind, subject_id)`); `executions`/`model_calls`/`tool_calls`
rows migrate directly. Row counts are asserted before the old tables are dropped — the same
no-loss bar #167 set for its own migration.

### 4. Epic dispositions

- #159 (Timescale foundation) — moves: the hypertable/compression/retention work happens once, in
  the authz usage store (#163's image question is answered once, for that cluster's usage tenant).
- #161 / #183–#188 (query API) — move repositories as #161 itself predicted; the closed
  per-grain query contract, grain guard, NULL-cost and µUSD rules land on the authz usage query
  surface. The dashboards read that surface.
- #160 / #168–#171 / #84 / #144 (collector auth) — stay here; unchanged in substance, retargeted
  at the usage ingest endpoints.
- #167 — superseded in place: the generalised day-grain schema is built in the consolidated
  store; this repo's vendor-named tables are migrated then dropped rather than generalised in situ.
- #95 / #96–#105 (acceptance telemetry) — stay here as collector/normalizer/schema-definition
  work; their storage target is the consolidated store.

## Rejected: keeping the split of RFC-0003 §6

Separation kept developer PII out of a store with an unauthenticated ingest listener, and kept
each repo's blast radius small. It cost: the headline query (#36, spend per engineer across all
sources) as a cross-service join; every storage invariant built and operated twice on two
Timescale-less clusters; and a boundary two repos already disagreed about in writing. ADR-0027
preserves the PII and auth arguments instead as prerequisites (identity isolation in
`usage_identities`; authenticated ingest before any developer-attributed source goes live).

## Consequences

Positive: one foundation, one query contract, one money discipline; #36 becomes a single-store
query; "adding a source requires no schema change" holds estate-wide.

Negative: this repo gives up its store — schema, migrations, and the query surface it planned —
and takes a dependency on the authz usage service's availability for every dashboard. Migration
and replay work is real and must be verified by row counts, not assumed.

## Related

- lightbridge-authz: ADR-0027, #535, #491, #489, #549; ADR-0022 (query contract prior art)
- this repo: #182, #167, #159, #161, #160, #30, #36, RFC-0003, ADR-0002, ADR-0003, ADR-0008,
  ADR-0013
