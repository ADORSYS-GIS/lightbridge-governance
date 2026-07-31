# ADR-0005: One workspace, registry first, connectors as crates

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

The two source specs arrived separately and each defines its own entity model. The Foundry
spec's Increment 1 (Tenant -> Application -> Environment -> Integration -> Agent, plus
credential issuance and revocation) is not Foundry-specific: the Copilot connector needs the
same `tenant_id`, the same application record and the same identity map. Both specs
independently say the normalized model should be provider-agnostic.

Building either connector first and retrofitting a registry underneath it means rewriting
every table's primary key.

## Decision

**One repository, one Cargo workspace**, laid out like `lightbridge-authz`:

```text
crates/governance-core       registry, credentials, normalized model, money
crates/governance-copilot    the pull connector   (RFC-0001)
crates/governance-foundry    the push connector   (RFC-0002)
app/lightbridge-governance   the API server       (bin)
app/governance-ctl           the collector CLI    (bin)
```

One image, both binaries. **The registry is built before either connector.**

Charts live **in this repository** and publish to OCI on merge, matching
`lightbridge-authz`; ai-helm consumes them as upstream charts.

## Consequences

**Positive**
- `tenant_id` and the application record exist before any connector writes a row.
- A third connector is a crate, not a repository.
- One CI pipeline, one release-please changelog, one image to sign and scan.

**Negative**
- The connectors share a release cadence. Acceptable while they share a registry; if that
  ever hurts, splitting a crate out is a smaller change than merging two repositories.

**Neutral / follow-ups**
- Ship **Copilot first**: it is pull-based, needs no public endpoint and changes nothing in
  the request path, so it proves the registry and the Postgres/Grafana pattern at low risk.
  Foundry goes second.

## Alternatives considered

- **A repository per connector with a shared core crate** -- rejected: a private registry
  dependency and a cross-repo version bump for every core change, to decouple two things
  released by the same people on the same day.
- **Charts in ai-helm** -- rejected for consistency: `lightbridge-authz` already publishes
  its own charts to OCI and ai-helm consumes them. Two conventions in one family is worse
  than either convention.

## Related

- ADR-0001 (single tenant -- what `tenant_id` is and is not for)
- RFC-0001, RFC-0002
