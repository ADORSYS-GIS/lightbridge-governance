# Replay from the raw archive

**When:** normalized data is wrong or missing, but the source objects in S3 are intact.
Typically after a normalizer bug, a bad schema migration, or a partial ingest.

This is the payoff for ADR-0002's split: **Postgres is the system of record, S3 is the raw
archive.** Any day can be rebuilt without calling the provider again -- which matters
because GitHub's signed URLs have long expired and its reports only go back a year.

## 1. Establish what is actually wrong

```bash
governance-ctl verify
```

Compares stored row counts against `ingest_manifest`. Drift means the normalized tables and
the manifests disagree; **agreement does not prove the data is right**, only that it is
consistent with what was recorded at ingest time. If the normalizer was wrong at ingest
time, both are wrong together -- in that case skip to §3.

## 2. Confirm the raw objects exist

```bash
aws --endpoint-url https://nbg1.your-objectstorage.com s3 ls \
  s3://ssegning-k8s-state/lightbridge-governance/raw/tenant=<t>/org=<o>/ --recursive \
  | head -20
```

If the objects are gone, replay is not possible and the only route is re-fetching from the
provider -- which works only inside its retention window. Say so plainly rather than
half-rebuilding.

## 3. Replay

```bash
governance-ctl replay --from 2026-07-01 --to 2026-07-31
```

Reads the S3 objects, re-runs normalization at the **current** code version and upserts.
Safe to repeat: writes are `ON CONFLICT DO UPDATE` on deterministic keys, and replay makes
no outbound provider calls.

For a normalizer fix, replay the whole affected range rather than the days that look wrong
-- the days that look right may be wrong in a way the dashboard does not surface.

## 4. Verify against something independent

```bash
governance-ctl verify
```

Then reconcile one day by hand against its raw object. A round-trip through our own code
proves the code is self-consistent, not that it is correct; the raw NDJSON is the only
independent witness we keep.

## 5. If the schema changed

Replay runs current code against old raw bytes, so a `schema_version` bump in the manifest
is expected and correct. If a raw object cannot be parsed by current code, that is a
compatibility break: fix the parser to handle both versions rather than rewriting the
archive. **The archive is immutable** -- it is the only thing that lets us prove what the
provider actually sent.
