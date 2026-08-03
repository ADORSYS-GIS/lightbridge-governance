# governance-core

The shared domain: the tenant/application/environment/integration registry, credential
issuance, and the money type every connector and the API depend on.

## Why this crate exists first

Both connectors need to answer "whose data is this?" before they can store anything.
Built first, that costs one schema. Built after either connector has real rows, it costs a
migration and a re-ingest of everything (see [#9](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/9),
now closed).

## What's in here

| Module | Owns |
|---|---|
| `schema` | Generated from [`schema/governance.cstack`](schema/governance.cstack) — models, migrations, CRUD repositories, REST routes. **Not hand-edited.** Change the `.cstack` file and rebuild (ADR-0009). |
| [`credential`](src/credential.rs) | Issuing, revoking and resolving integration credentials: 256-bit CSPRNG secrets, SHA-256 hashed (not argon2/bcrypt — see the module doc for why a high-entropy secret doesn't need a slow hash), fail-closed resolution for `/internal/v1/resolve`. |
| [`migrate`](src/migrate.rs) | Applies the migrations `schema.cstack` derives, plus the `touch_updated_at` triggers cratestack can't carry through `apply_pending` (dollar-quoted plpgsql — see the module's own comment). |
| [`registry`](src/registry.rs) | The hand-written enums the schema references: `Provider`, `ContentCapture`. |
| [`money`](src/money.rs) | `MicroUsd` — money as `i64` micro-USD, never a float (ADR-0008). |
| [`error`](src/error.rs) | This crate's `Error`/`Result`. |

## Persistence is cratestack, only

There is no hand-written SQL or migration file in this crate. Anything the generated CRUD
can't express goes in a schema `procedure`, not a second persistence path (ADR-0009). Two
upstream cratestack 0.5.1 gaps are hand-patched directly into the generated migration SQL,
each with a comment citing the upstream issue:

- `cratestack/cratestack#260` — the Postgres emitter doesn't emit `FOREIGN KEY` for
  declared `@relation` fields.
- `cratestack/cratestack#262` — it doesn't emit `CREATE UNIQUE INDEX` for a model-level
  `@@unique([...])`.

See the migration files under `migrations/postgres/` for exactly what was hand-added and
why.

## Testing

`cargo test -p governance-core` runs the unit tests unconditionally. The integration suite
(`tests/migrate_and_create.rs`) needs a real Postgres and skips with a printed message if
`DATABASE_URL` isn't set:

```bash
just up && just migrate
DATABASE_URL=postgres://postgres:postgres@localhost:5432/lightbridge_governance cargo test -p governance-core
```

Idempotency — reprocessing the same data must not change row counts — is the property that
matters here, not line coverage; `migrate_is_idempotent` and the credential revoke/replay
tests exist for exactly that reason.
