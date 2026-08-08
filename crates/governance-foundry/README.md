# governance-foundry

The Microsoft Foundry connector — a **push** connector ([RFC-0002](../../docs/rfc/0002-microsoft-foundry-otlp-ingestion.md)).

## Status: scaffold

This crate currently exports only the trusted OTLP resource attributes
([`src/lib.rs`](src/lib.rs)). It's blocked on [#8](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/8)
(the Foundry tenancy spike) via [#13](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/13).

**Note:** the module doc says this crate "owns the `/internal/v1/resolve` handler" — that
was the design at the time it was written. In practice `/internal/v1/resolve` was built in
[#11](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/11) and lives in
[`app/lightbridge-governance/src/resolve.rs`](../../app/lightbridge-governance/src/resolve.rs),
using `governance_core::credential::resolve` — not in this crate. The GenAI-span normalizer
this crate is meant to own hasn't been built yet.

## What this owns once built

- The normalizer turning OTLP GenAI spans into execution / model-call / tool-call records.

## Fixtures and the normalizer replay harness

`fixtures/` and `tests/normalizer_fixtures.rs` implement the "golden-dataset fixture"
RFC-0002's Verification section asks for, with an honesty caveat that matters: see
[`fixtures/README.md`](fixtures/README.md) and
[`docs/integrations/foundry-golden-fixtures.md`](../../docs/integrations/foundry-golden-fixtures.md)
for what it actually checks (normalizer output pinned against committed snapshots) and what
it does not (there is no real captured payload behind it yet, so the attribute-naming
assumption the pre-go-live review flagged is still open).

## Design decisions already made

- **`resolve` sits in Authorino's `ext_authz` hot path.** Whatever calls it must stay fast
  and cached — never add a database lookup directly to that step (see the house rule in the
  root `AGENTS.md`; disabled in production once already for exactly this reason).
- **Never trust `tenant_id` from the telemetry body.** It comes from the authenticated
  integration credential and is stamped by Authorino, never read from what the agent sends.
- **A revoked credential propagates within one cache TTL, not instantly.** The caching is
  Authorino's own (currently 60s, per ADR-0006 and `docs/runbooks/revoke-an-integration-token.md`)
  — not an in-process cache in this crate. `moka` is a workspace dependency but is not
  actually used anywhere in this codebase yet; don't assume it's wired in just because it's
  declared.
