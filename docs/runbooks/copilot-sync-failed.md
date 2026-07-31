# Copilot sync failed

**Symptom:** the `no successful sync in 36h` or `report older than 72h` alert fires, or the
Copilot dashboards have stopped advancing while showing no error.

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

### 403 from the report endpoints

**The most likely cause is not the credential.** Check, in this order:

1. Does the App still hold **all three** org permissions -- Copilot metrics, Copilot seat
   management **and Members**, all read? Losing any one gives 400/403.
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
