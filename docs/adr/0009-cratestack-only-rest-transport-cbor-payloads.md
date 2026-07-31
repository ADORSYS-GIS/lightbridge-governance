# ADR-0009: cratestack is the only persistence layer; REST transport, CBOR payloads

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

The initial scaffold used hand-written `sqlx` with hand-written SQL migrations, which is
what both source specs assume. That is a second way of doing something this family already
has one way of doing: `lightbridge-authz` adopted `cratestack-pg` in its ADR-0003 -- a
schema-first codegen framework (schema file -> migrations + CRUD repositories + typed
client + routes), built by one of this org's own maintainers.

`lightbridge-authz` adopted it **partially**: CRUD through cratestack, hand-written `sqlx`
retained for the security-critical non-CRUD paths (authorization CTEs, aggregation). That
split made sense there, because that service's hard parts are exactly those queries.

It does not carry over here. This service's aggregation happens in **Grafana**, not in Rust
(ADR-0003) -- the dashboards query Postgres directly. What is left on the Rust side is
registry CRUD and bulk idempotent ingest, which is the boilerplate cratestack exists to
remove.

## Decision

**cratestack only.** `crates/governance-core/schema/governance.cstack` is the source of
truth for the tables, the migrations, the CRUD layer and the routes.
`cratestack::include_server_schema!` expands it. There is **no hand-written SQL and no
hand-written migration** in this workspace.

Anything the generated CRUD cannot express goes in a schema **`procedure`** -- not in a
second persistence path. Verified before committing to this: cratestack supports both
`UpsertModelInput` (so idempotent ingest is expressible) and `procedure` blocks (the
escape hatch `lightbridge-authz` uses for `createAccount`).

**Transport is `rest`** -- resource-shaped routes and standard verbs, not the `rpc` style
`lightbridge-authz` uses. It is also cratestack's default `TransportStyle`.

**Payloads are CBOR** (`cratestack-codec-cbor`, `application/cbor`).

## Consequences

**Positive**
- One persistence path. A reader does not have to ask which of two mechanisms owns a table.
- Migrations are derived from the schema, so the schema and the database cannot drift --
  the class of bug where a migration and a row struct disagree simply does not exist.
- CBOR is compact and typed on the wire, and the codec is a first-class family member
  rather than something hand-rolled around `ciborium`.

**Negative**
- ⚠️ We are pre-1.0 on a framework with documented breaking changes across 0.x, and
  `lightbridge-authz`'s ADR-0003 lists real bugs it hit and worked around. The mitigation is
  that it is maintained in-house, so a bug is a conversation rather than an upstream issue.
- ⚠️ **The family must move in lockstep.** `lightbridge-authz` broke `main` by bumping some
  members and not others: a newer `cratestack-core` added a `TransportStyle::Grpc` variant
  that the older `cratestack-macros` did not cover. Bump all five together or none.
- CBOR is not human-readable, so debugging by eye needs a decode step. `curl | cbor2json`
  rather than `curl | jq`.

**Neutral / follow-ups**
- ⚠️ **JSON is retained for exactly one consumer.** Authorino's `metadata.http` step posts
  and parses `application/json` and cannot be taught CBOR, so `/internal/v1/resolve`
  (ADR-0006) must speak JSON. `cratestack-codec-json` is therefore a dependency, and its use
  is confined to that endpoint. Do not reach for it on the product API.
- cratestack has no OpenAPI generation. The typed client it emits is the contract instead.
- `Int` maps to Rust `i64`, so micro-USD amounts (ADR-0008) are safe. Never model money as
  `Decimal` or `Float` in the schema.

## Alternatives considered

- **Hand-written `sqlx`** (the scaffold's original shape) -- rejected: it is the boilerplate
  cratestack removes, and it would make this the only service in the family not using the
  family's tool.
- **cratestack for CRUD + `sqlx` for complex reads**, mirroring `lightbridge-authz` ADR-0003
  -- rejected here specifically because the complex reads are Grafana's, not ours (ADR-0003).
  If that ever stops being true, this ADR is the one to supersede.
- **JSON payloads** -- rejected for the product API: CBOR is smaller and typed, and the codec
  already exists. Retained only for the Authorino interop above.
- **`rpc` transport** -- rejected: the surface is resource-shaped (applications, integrations,
  manifests), so REST is the honest description of it.

## Related

- ADR-0002 (Postgres is the system of record), ADR-0003 (Grafana does the aggregation)
- ADR-0006 (the resolve endpoint that must stay JSON), ADR-0008 (micro-USD)
- `lightbridge-authz` ADR-0003 (the partial adoption this supersedes in scope, not in force)
