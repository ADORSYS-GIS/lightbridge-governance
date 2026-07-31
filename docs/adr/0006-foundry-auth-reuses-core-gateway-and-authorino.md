# ADR-0006: Foundry OTLP auth reuses core-gateway + Authorino; build no auth service

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

The Foundry spec's Increment 2 asks for: public HTTPS :443 -> bearer token ->
authentication service -> token lookup -> trusted integration context -> collector, with
the producer forbidden from setting `X-Scope-OrgID` or `governance.*` itself.

That is a description of infrastructure this platform already runs. `security-policies.yaml`
in ai-helm-values contains, in production today, an Authorino AuthConfig whose
`metadata.http` step calls a first-party Rust service's `/v1/resolve` over a shared secret
and stamps the result into request headers via `response.success.headers`.

## Decision

Add a **third host-indexed AuthConfig** (`otel.ai.camer.digital`) alongside the existing
external and internal ones. Its `metadata.http` step calls this service's
`/internal/v1/resolve` with an `X-Internal-Token` shared secret; the response is stamped as
`X-Scope-OrgID` and the `governance.*` attributes. Rate limiting and per-token quotas are a
`BackendTrafficPolicy` (ai-helm ADR-0021), not code.

**Do not build a bespoke OTLP authentication service.**

## Consequences

**Positive**
- TLS termination, ACME, a stable public endpoint, 401 on bad tokens, authoritative
  server-side revocation, per-token rate limits and quotas with live redis counters, and the
  existing gateway dashboards -- all free.
- `/internal/v1/resolve` is ClusterIP-only and trusts its posted body only with the
  internal token, exactly as `lightbridge-repo-auth` already does.

**Negative**
- `resolve` is in the ext_authz hot path of every customer request. It is cached in-process
  (moka, 60s), so **revocation propagates within one TTL, not instantly** -- the spec's
  "revoked tokens stop working" means "within 60 seconds", and the docs must say so.

**Neutral / follow-ups**
- ⚠️ A `sharedSecretRef` pointing at a Secret that does not exist makes the AuthConfig fail
  readiness, which **404s the entire gateway**. That is the OPA-removal outage. Create the
  `ssegning-aws` property and confirm `SecretSynced=True` BEFORE the AuthConfig references it.
- ⚠️ Authorino attaches per **listener** via the SecurityPolicy's `sectionNames`. A new
  `otel-https` listener must be added there or it is silently unauthenticated.
- ⚠️ Never add a *budget* or other database lookup to the Authorino step. The Keycloak
  introspection metadata step was disabled on 2026-07-02 (#533) because the ext_authz timeout
  is shorter than the lookup latency, which turns a slow dependency into fail-open.

## Alternatives considered

- **A dedicated authentication proxy in front of the collector** -- rejected: reimplements
  TLS, ACME, revocation, rate limiting and quotas that the gateway already provides.
- **Collector-native OTLP auth extension** -- rejected: moves tenancy decisions into the
  collector, where the producer's own attributes are already in scope.

## Related

- RFC-0002
- Runbook: `docs/runbooks/revoke-an-integration-token.md`
- ai-helm ADR-0011 (header contract), ADR-0021 (dual-plane authz + rate limiting)
