# ADR-0017: Replace shared-secret auth on `/internal/v1/resolve` with Kubernetes TokenReview

- Status: Proposed
- Date: 2026-09-04
- Decision owners: @stephane-segning

## Context

`/internal/v1/resolve` authenticates callers via a shared `X-Internal-Token` secret compared
with constant-time equality (`resolve.rs:167`). This is an interim measure from [#11](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/11) that
provides no per-caller identity: every `metadata.http` step in every Authorino AuthConfig
sends the same secret, so revoking one consumer's access would break all consumers.

The endpoint is **not in production** — the deployed AuthConfig in `ai-helm-values` points at
`lightbridge-repo-auth`, not this service ([#244](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/244) corrects a prior claim to the
contrary). ADR-0013 invariant 2 ("identity is bound at credential issuance") requires that
each caller be independently identifiable and revocable. [#169](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/169) scoped itself to
`/internal/v1/ingest` and explicitly left this endpoint out; [#243](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/243) deletes the ingest
endpoint entirely, making this the last `X-Internal-Token` in the codebase.

The reason to do this **now** rather than as security debt: once [#13](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/13) (AuthConfig #3) ships
and Authorino starts calling this endpoint, replacing its authentication becomes a
coordinated cutover across two repos. Today it has **no caller**, so the change is free.

`kubernetesTokenReview` is already in production use on this cluster — one of three live
AuthConfigs authenticates with it — so the infrastructure is proven and the governance pod's
ServiceAccount can be granted the minimal RBAC.

## Decision

**Replace the shared-secret `X-Internal-Token` header check with Kubernetes TokenReview-based
authentication.**

1. Authorino presents a projected ServiceAccount token (JWT) in the `Authorization: Bearer`
   header when calling `/internal/v1/resolve`.
2. The governance pod calls the kube-apiserver's TokenReview API (`POST
   /apis/authentication.k8s.io/v1/tokenreviews`) to validate it.
3. An allowlist of permitted ServiceAccount names (`<namespace>/<name>` format) is configured
   via the `ALLOWED_SERVICE_ACCOUNTS` environment variable.
4. When the kube-apiserver is unreachable or returns `authenticated: false`, the request is
   **refused** (fail-closed).
5. The shared secret (`INTERNAL_RESOLVE_TOKEN` / `X-Internal-Token`) is removed from all code,
   the Helm chart, and the ExternalSecret.

Implementation: raw `reqwest` HTTP POST — no `kube` crate. TokenReview is a single POST with
a simple JSON body/response; the `kube` crate's typed client, watch streams and runtime are
unnecessary overhead.

## Consequences

**Positive**
- Per-caller identity: revoking one consumer (ServiceAccount) does not break another.
- ADR-0013 invariant 2 is satisfied at the transport layer.
- `subtle` (constant-time comparison) is removed from the binary's dependency set — net
  -1 dependency, 0 additions.
- No `kube` / `k8s-openapi` added — the supply chain stays bounded.

**Negative**
- TokenReview adds ~1–5ms latency per request to the ext_authz hot path (in-cluster call to
  kube-apiserver). Bounded and acceptable; TokenReview result caching can be added later if
  measurable.
- RBAC: the governance pod's ServiceAccount needs `create` on `tokenreviews.authentication.k8s.io`,
  which is a new cluster-wide permission. Scoped to `tokenreviews` only — narrower than
  `system:auth-delegator`.

**Neutral / follow-ups**
- Caching TokenReview results could be added later if the overhead becomes measurable — keyed
  by `(audience, token-sub, token-exp)`, TTL shorter than the credential cache.
- Authorino's AuthConfig in `ai-helm-values` must be updated to send the projected SA token
  instead of the shared secret — a coordinated change, not part of this repo.
- The `governance_internal_resolve_token` property in the remote secret store can be cleaned
  up after the AuthConfig change lands.

## Alternatives considered

- **`kube` crate with `k8s-openapi`** — rejected: ~200 transitive crates (`hyper`, `tower`,
  `tonic`/`prost`) for a single HTTP POST. Overkill; `reqwest` is already in the tree.
- **JWT local validation (fetch JWKS, validate in-process)** — rejected: kube-apiserver does
  not expose a standard OIDC JWKS endpoint for ServiceAccount tokens; would require manual
  signing-key caching and rotation.
- **Keep shared secret** — rejected: violates ADR-0013 invariant 2 and provides no per-caller
  revocation.

## Related

- Issue: [#244](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/244) (this ticket)
- Issue: [#169](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/169) (the ingest half, closed — superseded by [#243](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/243))
- Issue: [#13](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/13) (AuthConfig #3 — this must land first)
- ADR: [`0006-foundry-auth-reuses-core-gateway-and-authorino.md`](./0006-foundry-auth-reuses-core-gateway-and-authorino.md)
- ADR: [`0013-telemetry-ingest-invariants-and-the-declaration-gate.md`](./0013-telemetry-ingest-invariants-and-the-declaration-gate.md) (invariant 2)
- Runbook: `docs/runbooks/revoke-an-integration-token.md`
