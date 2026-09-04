# lightbridge-governance

The API server: read surface over the registry, `/internal/v1/resolve` for Authorino, and
`/metrics`/`/livez`/`/readyz` for the ServiceMonitor and the Deployment's own probes. Also
owns the connector operational metrics (ADR-0007) — a CronJob pod can't be scraped, so the
collector records run outcomes in `ingest_manifests` and this always-running process derives
`governance_connector_*` from them on every `/metrics` scrape (see below).

## Routes

| Route | Payload | Auth | Owner |
|---|---|---|---|
| Everything under `schema.cstack`'s generated router | CBOR | `x-auth-id` header, forwarded by the gateway ([`router.rs`](src/router.rs)) | `governance-core` |
| `POST /internal/v1/resolve` | **JSON** — the one sanctioned exception (ADR-0009); Authorino's `metadata.http` step can't be taught CBOR | Kubernetes TokenReview (ADR-0017) — Authorino presents a projected ServiceAccount token | [`resolve.rs`](src/resolve.rs) / [`authn.rs`](src/authn.rs) |
| `GET /metrics` | Prometheus text | none | [`metrics.rs`](src/metrics.rs) |
| `GET /livez`, `GET /readyz` | plain text | none | `main.rs` |

## `/metrics`

`governance_ingest_*` (the `/internal/v1/ingest` telemetry path) is a set of plain in-process
counters. `governance_connector_*` (ADR-0007) is different: it is derived from
`ingest_manifests` fresh on every scrape, bounded by `CONNECTOR_METRICS_TIMEOUT_MS`, and is
absent (not zero) for a provider that has never synced or before the first successful refresh
-- an unreachable database must never render as a healthy-looking reading. See `metrics.rs`'s
module doc comment for exactly what each metric means, the refresh-on-scrape tradeoff, and
what a DB outage looks like on this endpoint.

## `/internal/v1/resolve` is fail-closed by design

Caller authentication is Kubernetes TokenReview (ADR-0017): Authorino presents a projected
ServiceAccount token in `Authorization: Bearer`, and this process validates it against the
kube-apiserver's TokenReview API, then checks the authenticated ServiceAccount against the
`ALLOWED_SERVICE_ACCOUNTS` allowlist. Every non-happy path — missing token, unreachable
kube-apiserver, `authenticated: false`, ServiceAccount not in the allowlist, a malformed body,
an unknown credential, a revoked one, or a database error — returns the identical `401` with
an empty body. The *reason* is only ever visible in the `tracing` logs at the point of
rejection; this is deliberate (ADR-0006), not an oversight to fix. See `resolve.rs`'s module
doc for the full rationale, including why a database error must never resolve to "allow".

Fail-closed is the invariant, not a preference: when the kube-apiserver is unreachable the
answer is *withhold*, never *allow* (AGENTS.md's first review question). See `authn.rs` for
the `TokenReviewVerifier` and its sabotage-first tests.

## Running locally

```bash
just up && just migrate
DATABASE_URL=postgres://postgres:postgres@localhost:5432/lightbridge_governance \
KUBE_APISERVER_URL=https://kubernetes.default.svc \
TOKEN_REVIEW_AUDIENCE=api \
ALLOWED_SERVICE_ACCOUNTS=authorino/authorino \
INTERNAL_INGEST_TOKEN=dev-token \
TENANT_ID=dev-tenant \
cargo run --bin lightbridge-governance
```

`TENANT_ID` has no default (ADR-0001: single-tenant per deployment, and `governance_connector_*`
scopes its `ingest_manifests` query by it, per the house rule that `tenant_id` belongs in the
WHERE clause of every query even here) — the process will not start without it, matching
`INTERNAL_INGEST_TOKEN`. `ALLOWED_SERVICE_ACCOUNTS` likewise has no default: an empty allowlist
rejects every caller, which is the safe startup failure. Locally there is no in-cluster CA or
SA token, so the `TokenReviewVerifier` falls back to system roots and an empty bearer token —
the endpoint will fail closed against a real apiserver unless you run in-cluster. See
`main.rs`'s `Args` struct for the full list of `env`-bindable CLI args and their defaults,
including `CONNECTOR_METRICS_TIMEOUT_MS` (bounds the `governance_connector_*` query, see
`metrics.rs`).

## Deploying

`charts/lightbridge-governance` renders this as a `Deployment` alongside the `copilot-sync`
`CronJob` (same image, different binary — `governance-ctl`, via a `command` override). See
that chart's own README for the values contract.
