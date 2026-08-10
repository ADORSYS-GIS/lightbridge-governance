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
- **`governance_org_*`** (`app/lightbridge-governance/src/metrics.rs`, derived in
  `crates/governance-core/src/org_kpis.rs`) is a small set of **org-level KPI gauges**
  (active/engaged users, cost, seats) for alerting on business questions --
  "monthly spend exceeded X", "active users dropped 30%", "licences going to waste" --
  that a Grafana SQL panel (ADR-0003) cannot page on. Same shape as
  `governance_connector_*`: derived from Postgres (`copilot_org_dailys`/
  `copilot_seat_snapshots`) on every `/metrics` scrape, so it survives an API restart
  the same way, and is absent (never a fabricated reading) until a refresh has actually
  confirmed a value. **Alert-grade**, in contrast to `governance_copilot_*` above -- see
  "Org-level KPI alerts" below.

## Org-level KPI alerts (`governance_org_*`)

A deliberate, bounded exception to ADR-0003's "Mimir keeps only
`governance_connector_*`": these carry no unbounded dimension (at most
`organization_id`, a handful of values per deployment -- ADR-0001 is single-tenant per
deployment), so they can live in Mimir without reopening the cardinality problem
ADR-0003 exists to close. See `crates/governance-core/src/org_kpis.rs`'s module doc
comment for the full reasoning, and why this is derived by the API rather than pushed
through the copilot-sync OTel collector (ADR-0011's family is dashboard-grade, not
alert-grade, because a collector restart blanks it for up to 6h -- these gauges have no
such gap).

All money gauges are **integer micro-USD** (ADR-0008, `..._micro_usd` suffix) -- divide
by `1e6` in PromQL for a human-readable USD number, never at the source.

Every gauge is absent (not a fabricated `0`) until a refresh has actually observed a
value for that organization, and a Postgres outage freezes the last known value rather
than zeroing it -- an alert must not fire "active users dropped to zero" just because
the database was briefly unreachable. `governance_org_kpi_has_data{family="usage"|"seats"}`
tells you whether a tenant has ANY data at all (`0` once confirmed, absent before the
first successful refresh) -- check it before reading a missing per-organization series
as "zero", which is the same absent-vs-zero trap `governance_connector_has_synced` exists
to avoid for connector freshness.

Example alerts, copy-pasteable into an `AlertmanagerConfig`/`PrometheusRule`:

```promql
# Monthly spend exceeded $5,000 (5_000 * 1e6 micro-USD) for any organization.
governance_org_cost_month_to_date_micro_usd > 5000000000

# Active users dropped 30% versus the same time yesterday. No separate
# "percentage dropped" metric exists or is needed -- this is a plain PromQL
# comparison over the gauge itself.
governance_org_active_users
  < (governance_org_active_users offset 1d) * 0.7

# Licence waste: seats assigned but never used at all, as of the latest seat
# snapshot -- the single most valuable alert in this family. Tune the
# absolute threshold to the deployment's seat count; a ratio alert also
# works if seat counts vary a lot across organizations:
#   governance_org_seats_never_used / governance_org_seats_assigned > 0.3
governance_org_seats_never_used > 20

# This tenant's usage data has gone missing entirely (not just stale for one
# organization) -- distinct from "genuinely zero active users", which would
# still render has_data=1 with the per-organization gauge present at 0.
governance_org_kpi_has_data{family="usage"} == 0
```

`time()`-based staleness (e.g. "no usage data refreshed in 48h") is intentionally NOT
listed here: unlike `governance_connector_last_success_timestamp_seconds`, these gauges
are values, not timestamps, so PromQL's `absent()`/`up`-style staleness handling (a
series that stops being scraped naturally disappears after Prometheus's own staleness
window) already covers "the API stopped refreshing this" -- watch
`governance_connector_metrics_scrape_errors_total{reason=~"org_.*"}` (extends the same
counter ADR-0007 defined) via `increase(...[10m]) > 0` for that instead, exactly as
already recommended for `governance_connector_*` above.

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
- **Non-zero exit with `"the once-per-run Copilot seat snapshot failed"`**: the four day-based
  reports may have succeeded -- this is a separate failure domain, checked independently (see
  "Seat snapshot failed" below). Do not assume the credential/policy is fine just because the
  reports landed, or vice versa.

### Seat snapshot failed (`report_type = billing-seats` in the logs/manifest)

Unlike the four day-based reports, `/copilot/billing/seats` has **no `day` parameter and no
history** -- it always returns the org's CURRENT seat assignments. Consequences for on-call:

- **There is no backfill for seats, ever.** If a run's seat snapshot fails, that day's seat
  state is gone -- not deferred, not recoverable by re-running `sync` later (a later run
  captures *that later day's* current seats, not the missed day's). The only way to recover a
  specific failed day's rows is if the raw fetch itself actually succeeded and only parsing
  broke afterward -- in that case the bytes are already archived and `governance-ctl replay`
  recovers them from S3, exactly like the four reports.
- **A seats-only failure fails the whole `sync` run**, even when every day-based report
  succeeded (`BackfillOutcome::exit_result` in `app/governance-ctl/src/sync.rs` checks the two
  independently). Do not read a non-zero exit as "the reports must be broken too" -- check
  `governance-ctl status` and the per-report log lines before assuming both failed.
- **`governance-ctl sync-day <day>`** does *not* touch seats at all -- it is the one-off
  historical-day repair tool for the four reports only, and seats has no "day" to repair.
  Re-running `governance-ctl sync` (the full backfill command) is what re-attempts today's
  seat snapshot.
- The permission/policy checks below apply identically to the seats endpoint -- it uses the
  same App installation token as the four reports, no separate credential.

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
