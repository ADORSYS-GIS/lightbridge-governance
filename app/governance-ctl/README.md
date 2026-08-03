# governance-ctl

The collector CLI. Runs as the `copilot-sync` CronJob in production, and doubles as an
operator tool from a shell. One image holds both this binary and `lightbridge-governance`
(`.docker/Dockerfile`); the CronJob overrides the image's `ENTRYPOINT` to run this one with
a subcommand instead.

## Subcommands

| Command | Does |
|---|---|
| `sync` | Ingest the most recent days, backfilling to the high-water mark if behind. **The only one the CronJob ever runs.** |
| `sync-day <YYYY-MM-DD>` | Ingest one specific report day. Idempotent — safe to re-run. |
| `replay <from> <to>` | Re-derive normalized rows from the raw S3 archive without calling GitHub again. |
| `verify` | Reconcile stored row counts against the manifests and report drift. |
| `status` | Print connector status: last success, report age, unmapped users. |
| `migrate` | Apply the schema migrations `governance-core::migrate` derives. No hand-written migration files exist (ADR-0009). |

Only `DATABASE_URL` and the chosen subcommand are required; see `main.rs`'s `Args`/`Command`
for the full surface. `sync`/`sync-day`/`replay`/`verify`/`status` need the Copilot
connector itself ([`governance-copilot`](../../crates/governance-copilot)) to be
implemented first — currently blocked on [#12](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/12).

## Why there's no separate backfill Job

A one-shot Kubernetes `Job` is immutable — re-running it means deleting the object
out-of-band, which fights ArgoCD's selfHeal. `sync` reads the database high-water mark and
backfills up to 28 days on its own whenever it's behind, which covers first-run bootstrap,
outage recovery, and a late-published report with one mechanism instead of three.

## Running locally

```bash
just up && just migrate
# or directly:
DATABASE_URL=postgres://postgres:postgres@localhost:5432/lightbridge_governance \
cargo run --bin governance-ctl -- migrate
```
