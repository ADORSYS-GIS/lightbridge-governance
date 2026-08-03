# lightbridge-governance

The API server: read surface over the registry, `/internal/v1/resolve` for Authorino, and
`/metrics`/`/livez`/`/readyz` for the ServiceMonitor and the Deployment's own probes. Also
owns the connector operational metrics (ADR-0007) — a CronJob pod can't be scraped, so the
collector records run outcomes in `ingest_manifest` and this always-running process derives
`governance_connector_*` from them (not yet implemented — see the note on `/metrics` below).

## Routes

| Route | Payload | Auth | Owner |
|---|---|---|---|
| Everything under `schema.cstack`'s generated router | CBOR | `x-auth-id` header, forwarded by the gateway ([`router.rs`](src/router.rs)) | `governance-core` |
| `POST /internal/v1/resolve` | **JSON** — the one sanctioned exception (ADR-0009); Authorino's `metadata.http` step can't be taught CBOR | Shared secret (`X-Internal-Token`) | [`resolve.rs`](src/resolve.rs) |
| `GET /metrics` | Prometheus text | none | [`metrics.rs`](src/metrics.rs) |
| `GET /livez`, `GET /readyz` | plain text | none | `main.rs` |

## `/metrics` is currently empty

The registry (`prometheus::Registry`) has no counters registered yet. Deriving
`governance_connector_*` from `ingest_manifest` is ADR-0007's own decision, not implemented
— this exists so the endpoint is real (the `charts/lightbridge-governance` ServiceMonitor
needs something to scrape) rather than a 404. Don't assume connector metrics are live just
because the route exists.

## `/internal/v1/resolve` is fail-closed by design

Every rejection cause — a wrong shared secret, a malformed body, an unknown credential, a
revoked one, or a database error — returns the identical `401` with an empty body. The
*reason* is only ever visible in the `tracing` logs at the point of rejection; this is
deliberate (ADR-0006), not an oversight to fix. See `resolve.rs`'s module doc for the full
rationale, including why a database error must never resolve to "allow".

## Running locally

```bash
just up && just migrate
DATABASE_URL=postgres://postgres:postgres@localhost:5432/lightbridge_governance \
INTERNAL_RESOLVE_TOKEN=dev-token \
cargo run --bin lightbridge-governance
```

All four CLI args (`--listen-addr`/`LISTEN_ADDR`, `--database-url`/`DATABASE_URL`,
`--internal-resolve-token`/`INTERNAL_RESOLVE_TOKEN`, `--resolve-timeout-ms`/`RESOLVE_TIMEOUT_MS`)
are `env`-bindable — see `main.rs`'s `Args` struct for defaults.

## Deploying

`charts/lightbridge-governance` renders this as a `Deployment` alongside the `copilot-sync`
`CronJob` (same image, different binary — `governance-ctl`, via a `command` override). See
that chart's own README for the values contract.
