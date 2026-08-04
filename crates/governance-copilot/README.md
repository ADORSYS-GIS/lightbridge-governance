# governance-copilot

The GitHub Copilot connector — a **pull** connector ([RFC-0001](../../docs/rfc/0001-github-copilot-connector.md)).

## Status: scaffold

This crate currently exports only the report endpoints and the API version pin
([`src/lib.rs`](src/lib.rs)) — the fetch/archive/upsert implementation itself is
[#12](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/12), which is blocked on
[#7](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/7) (the App-token spike).
As of this writing, #7 has confirmed GitHub App installation tokens work for these
endpoints, but the org's Copilot usage metrics policy still needs to be enabled and the
empirical run still needs to land before #12 can start.

## What it will pull

Polls GitHub's daily aggregated Copilot reports (`REPORTS` in `lib.rs`), follows their
short-lived signed download URLs, archives the raw NDJSON to S3, and upserts the normalized
rows into Postgres via `governance-core`.

⚠️ Access needs **three** org permissions (Copilot metrics, Copilot seat management,
Members — all read) **plus** the org's "Copilot metrics API access policy" toggle, which is
a setting, not a permission. An App with every box ticked still gets `403` until that
toggle is flipped — this is exactly the blocker #7's spike surfaced.

## Design decisions already made (see RFC-0001 and the ADRs it produced)

- **No separate one-shot backfill Job.** The eventual `governance-ctl sync` reads the
  database high-water mark and backfills up to 28 days on its own — one mechanism covers
  bootstrap, outage recovery, and a late-published report.
- **Postgres is the store, not Parquet-on-S3** (ADR-0002). S3 keeps the raw archive only,
  for replay.
- **Reprocessing must be an upsert**, not a duplicate — `ON CONFLICT DO UPDATE` on a
  deterministic key, once this crate has anything to upsert.
