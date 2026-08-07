# governance-copilot

The GitHub Copilot connector — a **pull** connector ([RFC-0001](../../docs/rfc/0001-github-copilot-connector.md)).

## Status: implemented

Auth (`src/auth.rs`), report fetch (`src/client.rs`, `src/report.rs`), parsing (`src/parse.rs`)
and Postgres persistence (`src/store.rs`, `src/sync.rs`) are all in place. The `governance-ctl`
binary (`app/governance-ctl/`) is what actually runs this as the `copilot-sync` CronJob --
`sync` (backfill), `sync-day`, `replay`, `verify`, `status`; see that crate's `src/sync.rs` for
the window logic and `docs/runbooks/copilot-sync-failed.md` for the operator runbook.

## What it pulls

Polls GitHub's daily aggregated Copilot reports (`REPORTS` in `lib.rs`), follows their
short-lived signed download URLs, archives the raw NDJSON to S3, and upserts the normalized
rows into Postgres.

⚠️ Access needs **two** org permissions -- Copilot metrics (read) and Copilot seat management
(read) -- plus `Metadata: Read`, **and** the org's "Copilot metrics API access policy" toggle,
which is a setting, not a permission. An App with every permission ticked still gets `403`
until that toggle is flipped. **`Members: Read` is NOT required** -- spike-0007's A/B
(`docs/spikes/0007-github-app-token-on-copilot-reports.md`) removed it from a live App and the
report endpoints kept returning `200`; this contradicts the vendor docs and an earlier draft of
this README, but the A/B is the thing that was actually run against production GitHub.

## Reliability

- **Bounded retry with backoff** (`client.rs`'s `send_with_retry`, used by every call in
  `auth.rs`/`report.rs`): transient failures (`5xx`, `429`, connection errors, timeouts) are
  retried a bounded number of times, honoring GitHub's `Retry-After` header when present.
  Deterministic failures (`401`/`403`/`404`, a malformed body) are never retried -- retrying
  them cannot succeed and only burns rate limit.
- **Trailing-window backfill, not a one-way ratchet.** `governance-ctl sync` always re-fetches
  the trailing lookback (default 3 days, RFC-0001) regardless of where the Postgres
  high-water mark sits, and separately closes any gap after it, bounded by a max backfill
  window (default 28 days) so a cold start cannot walk back forever. This is what makes a
  late-published report (GitHub returns `204` today, real data tomorrow) actually get
  re-attempted, rather than being permanently orphaned once a later day's manifest row moves
  the high-water mark past it. See `app/governance-ctl/src/sync.rs`'s `backfill_window`.
- **A totally failed `sync` run exits non-zero.** If every day in a non-empty backfill window
  failed, the CronJob's `backoffLimit`/alerting need to see that as a failure, not a silent
  exit 0. A partial failure (some days ok) stays exit 0: it is logged loudly and the failed
  day is picked up by the next run's trailing window.

## Design decisions already made (see RFC-0001 and the ADRs it produced)

- **No separate one-shot backfill Job.** `governance-ctl sync` reads the database high-water
  mark and backfills on its own — one mechanism covers bootstrap, outage recovery, and a
  late-published report.
- **Postgres is the store, not Parquet-on-S3** (ADR-0002). S3 keeps the raw archive only,
  for replay.
- **Reprocessing is an upsert**, not a duplicate — `ON CONFLICT DO UPDATE` on a deterministic
  key (`src/store.rs`).
