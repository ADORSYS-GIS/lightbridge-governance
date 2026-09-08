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

`cargo` and `cratestack` are the toolchain for everything that ships in the image.

Two exceptions, both fenced, both deliberate — and neither is reachable from the Rust gate:

- **npm, in `ide/vscode` only** — the VS Code extension (`just ext-check`, `just ext-build`).
  `just all-checks` does not run it and `ci.yml` does not know it exists; it has its own
  workflow (`vscode-extension.yml`), path-filtered so a Rust-only PR never starts a node job.
- **Python, for `scripts/generate_dashboards.py`** — a dev-time generator whose *output* (the
  dashboard JSON) is committed and is what the chart ships. Nothing at deploy or CI time runs it.

The rule is not "no other toolchain". It is: **nothing outside `cargo` may become a dependency
of building, testing or deploying the server image.** Check that before adding a third.

## Conventions that are not negotiable

- **No second persistence path** (ADR-0009). `crates/governance-core/schema/governance.cstack`
  is the source of truth for tables, migrations, CRUD and routes. Never add a direct `sqlx`
  dependency to any `Cargo.toml` — reach sqlx only through `cratestack::sqlx`. Every manifest
  in this repo has zero direct sqlx edges today; that is the property to preserve, not "no raw
  SQL."
- **Raw SQL exists and is allowed** where generated CRUD can't express the operation — an
  advisory lock and trigger DDL in `migrate.rs`, a credential-resolve JOIN in `credential.rs`,
  transactional upserts in `governance-core/src/ingest.rs` and
  `governance-copilot/src/store.rs`, a batch
  `= ANY($3)` lookup in `identity.rs`. It belongs inside a schema `procedure` or a dedicated
  store module, never as a parallel path to the database. See webank-context's
  [ADR-0038](https://github.com/ADORSYS-GIS/webank-context/blob/master/decisions/0038-cratestack-is-the-only-database-api.md)
  for the estate-wide version of this rule — this repo already satisfies its strictest clause
  (no direct sqlx dependency), but ADR-0038's capability findings were verified against
  cratestack 0.7.8 and we're on 0.5.1, so re-verify them here before relying on them.
- **Every id we mint is a CUID2** — 24 chars, lowercase `a-z0-9`, starts with a letter
  ([ADR-0039](https://github.com/ADORSYS-GIS/webank-context/blob/master/decisions/0039-cuid2-is-the-house-id-format.md)).
  This repo already does this — `cuid::cuid2()`, never `Uuid::new_v4()` — so the rule is: keep it
  that way, and never add a new call site that reaches for the `uuid` crate instead.
  Bans **minting**, not **storing**: an id we don't own (Keycloak's `sub`, an external token)
  stays exactly as it arrives and keeps being accepted, unvalidated.
  - Never validate an id's shape — no regex, no parse, no length check, no hyphen branching.
    Ids are opaque strings.
  - Never sort or paginate by id — CUID2 has no ordering. Use `createdAt`.
  - Store as TEXT (cratestack's `id String @id` already does this) — no native `uuid` column,
    no `DEFAULT gen_random_uuid()`.
  - Mint through one chokepoint, not a `cuid::cuid2()` call at every insert site.
- **REST transport, CBOR payloads.** JSON is retained for exactly one consumer:
  `/internal/v1/resolve`, because Authorino's `metadata.http` speaks JSON and cannot be
  taught CBOR. Do not reach for JSON on the product API.
- **Money is integer micro-USD** (ADR-0008). No float touches a monetary value, ever.
  cratestack's `Int` is `i64`, so never model money as `Decimal` or `Float` in the schema.
- **`thiserror` in libraries, `anyhow` in binaries.** No `unwrap()` outside tests — this is
  now mechanically enforced, see below.
- **Never log a token, a signed URL, or a request/response body.** Wrap secrets in a
  newtype whose `Debug`/`Display` print `<redacted>` so this is structural, not a habit.
- **`tenant_id` on every table and every query.** Single-tenant per deployment (ADR-0001),
  but the column is how that stays true.
- **Writes are idempotent.** `ON CONFLICT DO UPDATE` on deterministic keys — reprocessing a
  day must not change row counts.
- **Conventional Commits**, enforced by CI. The type drives release-please.

## Rust rules

### Don't hand-review what the linter already catches

`rustfmt.toml`, `clippy.toml` and `[workspace.lints]` in the root `Cargo.toml` enforce the
mechanical layer. Flagging these by hand in review is noise: `unwrap`/`expect`/`panic`/`todo`
in shipping code, `redundant_clone`, `needless_borrow`, `map_unwrap_or`, `clone_on_copy`,
`large_enum_variant`, `needless_collect`, `indexing_slicing`, `dbg!`, import grouping, and
supply-chain policy (`deny.toml`).

Two things about that config worth knowing before you edit it:

- **There is no soft tier here.** CI runs `clippy … -- -D warnings`, so `warn` and `deny`
  both fail the build. Every level was measured at zero before being set. If you add a lint,
  measure it first — and `cargo clean -p <crate>` before you believe the number, because a
  cached clippy run reports clean without having looked.
- **The fmt check runs on nightly on purpose.** `imports_granularity` and `group_imports` are
  nightly-only; on stable, rustfmt warns and exits 0, so a stable `--check` enforces nothing.
  Verified by planting a misplaced `std` import: stable passed it, nightly caught it. Use
  `cargo +nightly fmt` locally.

### Spend review attention here instead

What no lint can judge, roughly in order of damage done:

1. **Failure modes — does the unavailable branch become the permissive branch?** The highest-yield
   question in anything security-adjacent, and this service is: `/internal/v1/resolve` decides
   whether a caller's telemetry is accepted. When a dependency is unreachable the answer is
   *withhold*, never *allow*. `unwrap_or(false)` on a check is how an outage becomes an
   authorization bypass. A missing or unparseable attribute is **not** a default — it's "unknown",
   and unknown routes to the strictest branch. Write a test per dependency asserting refusal.
2. **Ownership shape.** Does a function take `T` where `&T` would do, forcing every caller to
   clone? Is there a clone in a loop or a per-request path? Start at `&T` and move outward only
   when forced; a `.clone()` should be explicable ("crosses a thread boundary"), not "the borrow
   checker complained". `Arc::clone` for a `'static` boundary is a refcount bump, not a defect.
3. **Error types.** Can a caller distinguish the cases they must handle? Keep variants at *our*
   abstraction level — leaking a dependency's error type into a public enum makes every dependency
   bump a breaking change.
4. **Do the tests test anything?** Would they fail if the logic were wrong?
5. **Money and units.** Any float near a monetary value is a defect (ADR-0008).

State the **mechanism** in review comments, not the verdict. "This clones per request" is
actionable; "non-idiomatic" is not.

### Testing rules that have actually caught things

- **Prove the test catches the bug.** A test written after the fix, that passes, has demonstrated
  nothing — it may assert something already true. Break the code, watch it fail *for the reason you
  predicted*, restore it. Say that you did.
- **Green does not mean tested.** A test that returns early when an env var is absent reports as
  *passed*; if CI never sets the variable the job is green having run nothing. Assert the tests
  actually ran. Investigate flakes — never paper over them with retries.
- **Idempotency is the property to test here**, not coverage: reprocessing the same day must not
  change row counts.

### Mocks must be unreachable from a production path

Not "unlikely" — unreachable. Test doubles live in `tests/support/`, never in `src/` behind a
doc comment saying "do not use in production". A service that refuses to boot beats one that
boots with a mock credential.

### Suppressions

`#[expect(lint, reason = "…")]`, never `#[allow(…)]` — `expect` fails once the suppression stops
being needed, so it cleans itself up. When you decline something non-obvious (a dependency you
chose not to upgrade, an accepted advisory), **write the reason in the manifest, not just the PR
body** — the person hitting the confusion in six months is reading `Cargo.toml`, and the PR
description is unreachable from there.

### Where the fuller rules live

Two corpora, deliberately kept separate rather than merged:

- **"Is there a rule for X?"** → the `rust-skills` catalogue (265 single-topic rules, each naming
  the clippy lint that catches it). Broader than anything written here.
- **"Which do I pick, and what does it cost?"** → the `rust-coding` skill's decision procedures
  (borrow/clone/`Cow`, which smart pointer, static vs dynamic dispatch, when type-state earns its
  complexity).

Where they conflict, the house rules above win — they exist because something broke. The
catalogue's own exemplary test uses float money, and it prints `#[allow]` in four rules and
`#[expect]` in none of 265; do not follow either here.

## Traps this platform has already paid for

- ⚠️ Build the image with **`cargo-auditable`**. Without it Trivy scans only the base OS and
  reports a clean result that means "it never looked at your Rust dependencies".
- ⚠️ `jsonwebtoken` v11 requires an explicit crypto backend feature (`aws_lc_rs` or `rust_crypto`).
  With none selected, signing **panics at runtime**. The backend pin in the workspace manifest is
  load-bearing. It is `aws_lc_rs`, not `rust_crypto`: the latter pulls the `rsa` crate, which has
  RUSTSEC-2023-0071 (Marvin attack, no fix released) and fails the SAST advisories gate.
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
