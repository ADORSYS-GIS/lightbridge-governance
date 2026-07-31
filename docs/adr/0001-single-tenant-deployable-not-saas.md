# ADR-0001: Ship a single-tenant deployable, not a multi-tenant SaaS

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

Both source specs are written in SaaS language -- "a customer installs the app",
"tenant-isolated prefixes", "cross-tenant authorization checks". That framing implies
self-service tenant onboarding, a public GitHub App with an installation webhook,
per-tenant credential issuance and a tenancy boundary enforced at every layer.

That is not what we are building. This platform governs **our** org and the repos we
manage. If we sell it, the customer runs their own installation of the same artifacts.

## Decision

Build a **single-tenant deployable**. One installation serves one organization.

`tenant_id` stays on every table and every S3 prefix -- not to serve many customers from
one install, but so our deployment and a customer's run the identical schema, and so the
column exists if the decision is ever revisited. Every query filters on it.

Explicitly out of scope: self-service tenant onboarding, a public GitHub App, the
`installation`/`installation_repositories` webhook flow, and cross-tenant authorization.

## Consequences

**Positive**
- The GitHub App can be private to the org and installed by hand.
- No onboarding UI, no webhook receiver, no tenant provisioning flow in the MVP.
- Credential issuance is an operator action (`governance-ctl`) plus a row, not a product surface.

**Negative**
- Selling to a customer means helping them deploy, not adding them to our instance.
  That is a deliberate trade -- it is also what keeps their telemetry out of our cluster.

**Neutral / follow-ups**
- Retrofitting real multi-tenancy later would mean a tenancy review of every query, not
  a schema migration. The column is cheap insurance; it is not the hard part.

## Alternatives considered

- **Multi-tenant SaaS from day one** -- rejected: it prices in an onboarding flow, a
  public App, and a tenancy boundary we would have to defend, for a customer count of one.
- **Drop `tenant_id` entirely** -- rejected: adding it later means rewriting every
  primary key, which is exactly the retrofit ADR-0005 exists to avoid.

## Related

- RFC: `docs/rfc/0001-github-copilot-connector.md`, `docs/rfc/0002-microsoft-foundry-otlp-ingestion.md`
- ADR-0004 (why the observability stack stays single-tenant too)
