# governance-foundry

The Microsoft Foundry connector — a **push** connector ([RFC-0002](../../docs/rfc/0002-microsoft-foundry-otlp-ingestion.md)).

## What this owns

- The normalizer trait and per-provider implementations that turn OTLP GenAI
  spans into `ExecutionInput`/`ModelCallInput`/`ToolCallInput` records:
  [`src/normalizer/{claude_code,codex,foundry,otlp}.rs`](src/normalizer/).
- The dispatch logic that routes a span to the right normalizer by the
  registered integration's `provider` string
  ([`src/normalizer.rs`](src/normalizer.rs), `NORMALIZERS` table) — data-driven,
  not a match over providers, so adding a fourth provider is a new normalizer
  plus one map entry; nothing in the auth, quota or ingest paths changes.
- The trusted resource-attribute list ([`src/lib.rs`](src/lib.rs)) Authorino
  stamps and the collector treats as authoritative.

This crate does **not** own `/internal/v1/resolve`, despite what an earlier
version of this doc (and the module doc of an earlier `lib.rs`) said. That
handler was built in [#11](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/11)
and lives in
[`app/lightbridge-governance/src/resolve.rs`](../../app/lightbridge-governance/src/resolve.rs),
using `governance_core::credential::resolve`.

## Fixtures and the normalizer replay harness

`fixtures/` and `tests/normalizer_fixtures.rs` implement a normalizer replay
harness pinning current output against committed snapshots — but read
[`fixtures/README.md`](fixtures/README.md) and
[`docs/integrations/foundry-golden-fixtures.md`](../../docs/integrations/foundry-golden-fixtures.md)
before treating it as RFC-0002's "golden-dataset fixture": the fixtures are
hand-authored from the documented attribute contract, not captured from a real
OTLP export, so they cannot verify the one thing the pre-go-live review
actually flagged — whether the assumed attribute names (`model.name`,
`tokens.input`, `tokens.output`, `session.id`, `tool.name`, `duration.ms`)
match what the deployed collector emits. `fixtures/captured/` is where a real
export goes once one exists; the same harness picks it up with no code change.

## Design decisions already made

- **`resolve` sits in Authorino's `ext_authz` hot path.** Whatever calls it must stay fast
  and cached — never add a database lookup directly to that step (see the house rule in the
  root `AGENTS.md`; disabled in production once already for exactly this reason).
- **Never trust `tenant_id` from the telemetry body.** It comes from the authenticated
  integration credential and is stamped by Authorino, never read from what the agent sends.
- **A revoked credential propagates within one cache TTL, not instantly.** Two layers, both
  60s by default: the cache TTL Authorino applies to the resolve response itself (the
  documented revocation SLA, ADR-0006 and
  `docs/runbooks/revoke-an-integration-token.md`), and an in-process `moka` cache in front
  of `governance_core::credential::resolve`
  ([`app/lightbridge-governance/src/resolve.rs`](../../app/lightbridge-governance/src/resolve.rs)).
  Neither lives in this crate — `moka` is a workspace dependency, but this crate does not
  import it, so don't assume it's wired in here just because the workspace declares it.
