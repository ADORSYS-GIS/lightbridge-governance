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

It also snapshots the org's Copilot **seats** (`/orgs/{org}/copilot/billing/seats`) --
RFC-0001's headline motivation: "who has a seat and has never used it". This is a
structurally different endpoint, not one more entry in `REPORTS`:

- **Plain paginated JSON, not the two-step envelope/signed-URL flow** the four reports use.
  `src/seats.rs`'s `GithubClient::fetch_seats` follows `Link: rel="next"`, bounded by a page
  cap and a per-page byte cap so a malformed or looping `Link` header cannot hang a run.
- **No `day` parameter, no history.** GitHub always returns the CURRENT seat assignments, so
  seats are fetched exactly **once per `sync` run**, stamped with today's date, and are
  deliberately kept OUTSIDE `REPORTS`/`sync_day`'s per-day backfill loop -- looping them per
  backfilled day would write the SAME current snapshot under several different
  `snapshot_day`s, fabricating a seat history that was never actually observed. **There is no
  backfill for seats, ever**: a day a run failed to snapshot is gone, not recoverable by a
  later run (though it IS replayable from that run's own S3 archive if the bug was in
  parsing, not fetching -- see `sync::sync_seats`'s doc comment).
- **Manifest report type `billing-seats`** (`SEATS_REPORT_TYPE`), not one of the four report
  slugs. `store::high_water_mark` explicitly excludes it from the day-based reports'
  `MAX(report_day)` computation -- a seat snapshot succeeding every run must not make the
  *daily reports* look current while they have actually been failing.
- **`GET .../billing/seats` field mapping** (`SeatRow` in `src/model.rs`, `parse::parse_seats`):
  `assignee.id`/`assignee.login` → `provider_user_id`/`user_login`; `created_at` →
  `seat_assigned_at`; `last_activity_at`/`last_activity_editor` carried through as-is (`NULL`
  means "never used", never a fabricated default -- that is the exact signal RFC-0001 wants).
  `seat_state` is derived, not a GitHub field: GitHub gives no lifecycle status here, only
  `pending_cancellation_date`, so a seat is `"pending_cancellation"` when that date is set and
  `"active"` otherwise (a cancelled seat simply stops appearing in the listing at all).

⚠️ Access needs **two** org permissions -- Copilot metrics (read) and Copilot seat management
(read) -- plus `Metadata: Read`, **and** the org's "Copilot metrics API access policy" toggle,
which is a setting, not a permission. An App with every permission ticked still gets `403`
until that toggle is flipped. **`Members: Read` is NOT required** -- spike-0007's A/B
(`docs/spikes/0007-github-app-token-on-copilot-reports.md`) removed it from a live App and the
report endpoints kept returning `200`; this contradicts the vendor docs and an earlier draft of
this README, but the A/B is the thing that was actually run against production GitHub. The
seats endpoint needs no *additional* permission beyond what was already required for the four
reports -- "Copilot seat management: Read" was already listed above before this connector wrote
a single seat row, so there is no new App-permission change to roll out.

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
- **A seats-only failure also exits non-zero, independently of the day-based reports.**
  `BackfillOutcome::exit_result` (`app/governance-ctl/src/sync.rs`) checks the seat snapshot's
  own `Result` separately from `covered`/`window_days`: every day-report succeeding does not
  mask a broken seat snapshot, and a healthy seat snapshot does not mask every day-report
  failing. RFC-0001's headline use case going silently unfilled is exactly the failure this
  independence exists to surface.

## Design decisions already made (see RFC-0001 and the ADRs it produced)

- **No separate one-shot backfill Job.** `governance-ctl sync` reads the database high-water
  mark and backfills on its own — one mechanism covers bootstrap, outage recovery, and a
  late-published report.
- **Postgres is the store, not Parquet-on-S3** (ADR-0002). S3 keeps the raw archive only,
  for replay.
- **Reprocessing is an upsert**, not a duplicate — `ON CONFLICT DO UPDATE` on a deterministic
  key (`src/store.rs`).
