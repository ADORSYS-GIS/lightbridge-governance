# Copilot sync failed

**Symptom:** the `no successful sync in 36h` or `report older than 72h` alert fires, or the
Copilot dashboards have stopped advancing while showing no error.

## Which metric to actually page on

Two unrelated `governance_copilot*`-prefixed families exist in this system. Only one of
them is alert-grade -- wiring a page to the other one *will* eventually page on nothing,
or fail to page when it should.

- **`governance_connector_last_success_timestamp_seconds{provider="github_copilot"}`**
  (`app/lightbridge-governance/src/metrics.rs`) is the one to alert on. It is derived,
  on every `/metrics` scrape, directly from `ingest_manifests` in Postgres (ADR-0007) --
  the same table the CronJob writes as its idempotent, authoritative record of what
  actually landed. It survives API restarts (recomputed from stored state, not
  remembered by a process), is unaffected by the OTel collector's own restarts, label
  drift, or a `CiliumNetworkPolicy` misconfiguration between `copilot-sync` and that
  collector, and is absent (never a fabricated `0`) until a refresh has actually
  observed something. `time() - governance_connector_last_success_timestamp_seconds`
  is the real freshness signal; this is what `no successful sync in 36h` /
  `report older than 72h` must be built on.
- **`governance_copilot_*`** (`app/governance-ctl/src/metrics.rs`, pushed over OTLP to a
  dedicated collector -- see ADR-0011) is best-effort **per-run detail** for dashboards:
  reports fetched by outcome, rows upserted by report type,
  `governance_copilot_last_run_timestamp_seconds`, unmapped users, manifest drift. It is
  useful for "what did the last run actually do", but its entire state lives in the
  collector's in-memory Prometheus cache (`replicas: 1`, no PodDisruptionBudget) -- a
  collector restart (node drain, image bump, OOM, reschedule) blanks every series until
  the next `copilot-sync` run, up to 6h later on the CronJob's schedule. A missing
  `governance_copilot_*` series is therefore not distinguishable from "no run has
  happened yet today" and must never be the sole basis for a page. Use it to see *what*
  a run did, not *whether* the connector is healthy.

## 0. Distinguish "broken" from "did not run"

This is the whole job. The dashboards look identical either way.

```bash
governance-ctl status
```

`last_success_at`, `report_age_seconds` and the per-report row counts come from
`ingest_manifest` (ADR-0007), so they reflect stored state rather than a process's memory.

Then look at whether the CronJob even fired:

```bash
kubectl -n governance get cronjob copilot-sync
kubectl -n governance get jobs --sort-by=.metadata.creationTimestamp | tail -5
```

- **No recent Job** -> scheduling problem, go to §1.
- **Job ran and failed** -> go to §2.
- **Job succeeded but data is stale** -> GitHub has not published the report yet. The
  three-day lookback means this self-heals; confirm by checking whether the day is
  available at all before doing anything else.

## 1. The CronJob did not run

`concurrencyPolicy: Forbid` means a hung previous run blocks every subsequent schedule.

```bash
kubectl -n governance get jobs -o json \
  | jq -r '.items[] | select(.status.active == 1) | .metadata.name'
```

Delete a hung Job and the next tick will schedule. If it hung on a network call, note the
`activeDeadlineSeconds: 1800` should have killed it -- if it did not, that is the bug.

## 2. The Job ran and failed

```bash
kubectl -n governance logs job/<name> --tail=200
```

Logs are structured JSON. The `status` field and `reasonCode` say which stage failed.

A Job's exit code distinguishes two very different situations, so do not treat every failed
Job the same:

- **Non-zero exit** (`backoffLimit`/alerting engaged): every day in that run's window failed --
  a totally broken run (dead credential, GitHub unreachable for the whole run). Go straight to
  the credential/egress checks below.
- **Exit 0, but the logs show `"day failed; continuing backfill"` for one or more days**: a
  partial failure. This is expected to self-heal -- the failed day is within the trailing
  lookback window and gets re-attempted on the next scheduled run with no operator action. Only
  chase this by hand if `governance-ctl verify` still shows drift for that day after the next
  scheduled run has had a chance to retry it.

### 403 from the report endpoints

**The most likely cause is not the credential.** Check, in this order:

1. Does the App still hold **both** required org permissions -- Copilot metrics and Copilot
   seat management, both read -- plus `Metadata: Read`? Losing either of the first two gives
   400/403. **`Members: Read` is NOT required** -- spike-0007's live A/B removed it from the
   App and the report endpoints kept returning `200`
   (`docs/spikes/0007-github-app-token-on-copilot-reports.md`); do not spend time re-adding it.
2. Is the organization's **"Copilot metrics API access policy"** still enabled? It is an
   org *setting*, not a permission, and an org owner can turn it off without touching the App.
3. Only then suspect the credential itself.

### 404 on a specific day

Reports exist from 2025-10-10 and for roughly one year back. A 404 outside that window is
correct behaviour, not a failure -- `verify` should be reporting it as skipped, not failed.

### Timeouts fetching the signed URL

The signed download URLs expire quickly and are served from a different host than
`api.github.com`. If the download times out while the API call succeeded, suspect the
`toFQDNs` egress allowlist rather than GitHub.

```bash
kubectl -n governance exec deploy/lightbridge-governance -- \
  wget -qO- --timeout=5 https://api.github.com/zen || echo "egress blocked"
```

## 3. Force a re-run

Safe at any time -- every write is `ON CONFLICT DO UPDATE` keyed on
`(tenant, provider, scope, day, report_type)`.

```bash
governance-ctl sync                      # lookback + backfill to the high-water mark
governance-ctl sync-day 2026-07-29       # one specific day
```

## 4. Confirm recovery

```bash
governance-ctl verify
```

Reconciles stored row counts against the manifests. **Run this rather than re-reading the
dashboard** -- a dashboard that is merely cached looks exactly like one that is correct.
