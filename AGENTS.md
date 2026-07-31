# AGENTS.md

The AI governance platform: a tenant/application/integration registry plus two connectors.

- `governance-core` — registry, credential issuance, normalized model, money.
- `governance-copilot` — **pull** connector for GitHub Copilot daily reports (RFC-0001).
- `governance-foundry` — **push** connector for Microsoft Foundry OTLP (RFC-0002).
- `lightbridge-governance` — the API server (bin).
- `governance-ctl` — the collector CLI (bin). Runs as the `copilot-sync` CronJob.

One image, both binaries. Charts live here and publish to OCI, like `lightbridge-authz`.

## Read these first

1. [`docs/adr/README.md`](docs/adr/README.md) — the decisions and *why*. Read the relevant
   ones before changing anything they cover.
2. [`docs/architecture.md`](docs/architecture.md) — the system map.
3. [`docs/rfc/README.md`](docs/rfc/README.md) — what each connector is specified to do.

## Commands

```bash
just all-checks     # fmt + clippy -D warnings + check + test, in CI's order
just test
just deny           # supply-chain audit, same checks as the SAST job
just up             # local Postgres
just migrate
```

There is no `npm` and no Python here. `cargo` and `cratestack` are the toolchain.

## Conventions that are not negotiable

- **cratestack is the only persistence layer** (ADR-0009). `crates/governance-core/schema/governance.cstack`
  is the source of truth for tables, migrations, CRUD and routes. Do **not** hand-write SQL
  or a migration — if the generated CRUD cannot express it, add a schema `procedure`.
- **REST transport, CBOR payloads.** JSON is retained for exactly one consumer:
  `/internal/v1/resolve`, because Authorino's `metadata.http` speaks JSON and cannot be
  taught CBOR. Do not reach for JSON on the product API.
- **Money is integer micro-USD** (ADR-0008). No float touches a monetary value, ever.
  cratestack's `Int` is `i64`, so never model money as `Decimal` or `Float` in the schema.
- **`thiserror` in libraries, `anyhow` in binaries.** No `unwrap()` outside tests.
- **Never log a token, a signed URL, or a request/response body.** Wrap secrets in a
  newtype whose `Debug`/`Display` print `<redacted>` so this is structural, not a habit.
- **`tenant_id` on every table and every query.** Single-tenant per deployment (ADR-0001),
  but the column is how that stays true.
- **Writes are idempotent.** `ON CONFLICT DO UPDATE` on deterministic keys — reprocessing a
  day must not change row counts.
- **Conventional Commits**, enforced by CI. The type drives release-please.

## Traps this platform has already paid for

- ⚠️ Build the image with **`cargo-auditable`**. Without it Trivy scans only the base OS and
  reports a clean result that means "it never looked at your Rust dependencies".
- ⚠️ `jsonwebtoken` v10 requires an explicit crypto backend feature. With none selected,
  signing **panics at runtime**. The `rust_crypto` pin in the workspace manifest is load-bearing.
- ⚠️ Never mark a `secretKeyRef` env var `optional: true`. Env vars bind once at pod start
  and never refresh, so an optional ref lets a pod that beats ESO capture an **empty**
  credential and fail auth forever.
- ⚠️ The cratestack family must move in **lockstep**. lightbridge-authz broke `main` by
  bumping some members and not others — a newer `cratestack-core` added a
  `TransportStyle::Grpc` variant the older `cratestack-macros` did not cover.
- ⚠️ `/internal/v1/resolve` is in Authorino's ext_authz hot path. Keep it fast and cached.
  **Never add a database lookup to the Authorino step itself** — that pattern was disabled
  in production on 2026-07-02 because the ext_authz timeout is shorter than the lookup.

## AI governance

Every PR uses the governance template: an AI Usage Declaration, a real source-of-truth link,
and verification evidence. AI output is untrusted input — review it as such, and never
submit work you cannot explain. <https://adorsys-gis.github.io/ai-governance/>
