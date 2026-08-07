# governance-ctl

The collector CLI. Runs as the `copilot-sync` CronJob in production, and doubles as an
operator tool from a shell. One image holds both this binary and `lightbridge-governance`
(`.docker/Dockerfile`); the CronJob overrides the image's `ENTRYPOINT` to run this one with
a subcommand instead.

## Subcommands

| Command | Does |
|---|---|
| `sync` | Ingest the trailing lookback window (default the last 3 days), plus any gap after the high-water mark if behind. **The only one the CronJob ever runs.** Exits non-zero if the window was non-empty and every day in it failed (see "Exit codes" below); a partial failure logs loudly and stays exit 0. |
| `sync-day <YYYY-MM-DD>` | Ingest one specific report day. Idempotent — safe to re-run. |
| `replay <from> <to>` | Re-derive normalized rows from the raw S3 archive without calling GitHub again. |
| `verify` | Reconcile stored row counts against the manifests and report drift. |
| `status` | Print connector status: last success, report age, unmapped users. Distinguishes "never synced" from "synced N days ago" — see `sync::SyncStatus`. |
| `migrate` | Apply the schema migrations `governance-core::migrate` derives. No hand-written migration files exist (ADR-0009). |

`DATABASE_URL` and the chosen subcommand are required; see `main.rs`'s `Args`/`Command` for
the full surface. `COPILOT_LOOKBACK_DAYS` (default 3) and `COPILOT_MAX_BACKFILL_DAYS`
(default 28) tune `sync`'s window (`sync::backfill_window`) and are both optional — an
invalid or missing value falls back to the default rather than failing the process at
startup (see AGENTS.md's CrashLoopBackOff note on required-arg-with-no-default).

## Why there's no separate backfill Job

A one-shot Kubernetes `Job` is immutable — re-running it means deleting the object
out-of-band, which fights ArgoCD's selfHeal. `sync` always re-fetches the trailing lookback
window and separately closes any gap after the database high-water mark, bounded by
`COPILOT_MAX_BACKFILL_DAYS` so a cold start cannot walk back forever
(`sync::backfill_window`) — one mechanism covers first-run bootstrap, outage recovery, and a
late-published report, and a day that failed once is retried automatically by the next
scheduled run's trailing window rather than being permanently skipped once the high-water
mark moves past it.

## Exit codes

`sync` exits non-zero when the computed window was non-empty and **every** day in it
failed — a totally broken run (dead credential, GitHub unreachable), which the CronJob's
`backoffLimit`/alerting need to see. A **partial** failure (some days ok, some not) stays
exit 0: it is logged (`"day failed; continuing backfill"`), counted, and the failed day is
picked up by the next run's trailing window with no operator action. See
`sync::BackfillOutcome::exit_result` and `docs/runbooks/copilot-sync-failed.md`.

## Running locally

```bash
just up && just migrate
# or directly:
DATABASE_URL=postgres://postgres:postgres@localhost:5432/lightbridge_governance \
cargo run --bin governance-ctl -- migrate
```
