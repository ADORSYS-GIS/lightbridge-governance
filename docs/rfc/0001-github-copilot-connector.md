# RFC-0001: GitHub Copilot connector

- Status: Draft
- Date: 2026-07-31
- Author: @stephane-segning
- Source of truth: [`sources/github-copilot-governance-mvp.md`](./sources/github-copilot-governance-mvp.md)
  (the original planning spec, copied in so it survives outside the maintainer's own machine), and
  <https://docs.github.com/en/rest/copilot/copilot-metrics?apiVersion=2026-03-10>

## Summary

A **pull** connector. Every six hours it fetches GitHub's daily aggregated Copilot reports
for the organization, follows their short-lived signed download URLs, archives the raw
NDJSON to S3 and upserts normalized rows into Postgres. Grafana reports on those rows
(ADR-0003); Mimir carries only connector health (ADR-0007).

## Motivation

Copilot seats are bought per user and the only usage signal GitHub gives back is a daily
report behind an API. Without ingesting it, questions like "who has a seat and has never
used it", "what does adoption look like by team", and "what are we paying per active user"
have no answer at all.

## Design

### Endpoints

Org scope, `X-GitHub-Api-Version: 2026-03-10`, `Accept: application/vnd.github+json`:

```http
GET /orgs/{org}/copilot/metrics/reports/organization-1-day?day=YYYY-MM-DD
GET /orgs/{org}/copilot/metrics/reports/users-1-day?day=YYYY-MM-DD
GET /orgs/{org}/copilot/metrics/reports/repos-1-day?day=YYYY-MM-DD
GET /orgs/{org}/copilot/metrics/reports/user-teams-1-day?day=YYYY-MM-DD
GET /orgs/{org}/copilot/billing/seats?per_page=100&page=N
```

Reports are NDJSON behind signed URLs that expire quickly -- download in the same job run.
Data exists from 2025-10-10 and stays available for roughly one year.

`user-teams-1-day` **exists at organization scope**. The source spec states team attribution
is enterprise-only and that we therefore need a manual GitHub-login -> team mapping table;
that is out of date. We ingest the report instead. The caveat that *is* real: GitHub omits
teams with fewer than five seated Copilot users.

### Scheduling

`0 */6 * * *`, `concurrencyPolicy: Forbid`, `backoffLimit: 4`,
`activeDeadlineSeconds: 1800`. Each run re-fetches D-1, D-2 and D-3 so a late-published
report is picked up with no operator action.

**There is no separate backfill Job.** A one-shot k8s Job is immutable, so re-running it
means deleting the object out of band, which ArgoCD selfHeal fights. `sync` reads the
high-water mark from `ingest_manifest` and backfills up to 28 days when it is behind. Late
recovery and first-run backfill are then the same code path.

### Storage

- **S3 raw:** `s3://ssegning-k8s-state/copilot-governance/raw/org=<o>/day=<d>/<report>.ndjson`
  (the bucket and `copilot-governance/` prefix per the ai-helm plan). Deterministic keys,
  overwrite-safe; the archive is for replay only, not a query layer.
- **Postgres (legacy, being retired):** `copilot_org_dailys`, `copilot_user_dailys`,
  `copilot_repo_dailys`, `copilot_user_teams`, `copilot_seat_snapshots`, upserted
  `ON CONFLICT DO UPDATE`. Coverage is recorded in `ingest_manifests` (one row per day +
  report), which drives the high-water-mark backfill and `governance-ctl verify`.
- **OTLP (current sink, ADR-0014):** day-grain facts and seat snapshots are emitted as OTLP
  log records through the authenticated edge collector per the
  [encoding contract](#otlp-day-grain-encoding-contract) below. The direct-Postgres write path
  is removed at the cutover (lightbridge-authz#588); the S3 raw archive and `replay` remain
  the backfill mechanism until then.

### Identity

`user-teams-1-day` gives GitHub-login -> team. `identity_map` exists only to reach *internal*
identity and cost centre, joined to Keycloak `user_entity` by verified email (the ai-helm
ADR-0063 datasource). **Never match on display name.**

### OTLP day-grain encoding contract

Under [ADR-0014](../adr/0014-usage-telemetry-consolidates-into-the-authz-usage-store.md)
Decision 2, `governance-ctl` stops writing its own Postgres and emits day-grain facts and seat
snapshots as **OTLP log records** through the authenticated edge OTEL collector. The usage-side
day-grain normalizer in `lightbridge-authz` reads these records into the generalized
`usage_day_facts` / `usage_seat_snapshots` tables. **This section is a contract with that
normalizer, not an implementation detail** -- the attribute names, types and units below are
pinned and must not change without a coordinated change on both sides.

#### Transport

- OTLP **log records** (not metrics, not traces) over the authenticated edge collector.
- **One log record per (report, subject)** -- i.e. one record per normalized row.
- The record **body** is a short human-readable summary; all machine-readable data lives in
  typed **attributes**.
- Money is **integer micro-USD** (`net_cost_micro_usd`, ADR-0008). No float ever appears.

#### Common attributes (every record)

| attribute | type | value |
|---|---|---|
| `source` | string | `github_copilot` (the trusted-source stamp) |
| `tenant_id` | string | deployment tenant (ADR-0001) |
| `org` | string | GitHub org scope |
| `report` | string | `organization-1-day` / `users-1-day` / `repos-1-day` / `user-teams-1-day` / `billing-seats` |
| `day` | string | `YYYY-MM-DD` (`report_day`, or `snapshot_day` for seats) |
| `subject_kind` | string | `org` / `user` / `repo` / `user_team` / `seat` |
| `subject_id` | string | the natural key of the subject |

#### Per-report attributes

**`organization-1-day`** — `subject_kind=org`, `subject_id=organization_id`

| attribute | type | unit |
|---|---|---|
| `active_users` | int | count |
| `engaged_users` | int | count |
| `total_interactions` | int | count |
| `total_completions` | int | count |
| `ai_credits` | int | count |
| `net_cost_micro_usd` | int | micro-USD |

**`users-1-day`** — `subject_kind=user`, `subject_id=provider_user_id`

| attribute | type | unit |
|---|---|---|
| `user_login` | string | |
| `total_interactions` | int | count |
| `total_completions` | int | count |
| `ai_credits` | int | count |
| `net_cost_micro_usd` | int | micro-USD |

**`repos-1-day`** — `subject_kind=repo`, `subject_id=repository_id`

| attribute | type | unit |
|---|---|---|
| `coding_agent_activity` | int | count |
| `code_review_activity` | int | count |
| `pull_request_activity` | int | count |

**`user-teams-1-day`** — `subject_kind=user_team`, `subject_id=user_id`

| attribute | type | unit |
|---|---|---|
| `team_id` | string | |
| `team_slug` | string | |

**`billing-seats`** — `subject_kind=seat`, `subject_id=provider_user_id`

| attribute | type | unit |
|---|---|---|
| `user_login` | string | |
| `seat_assigned_at` | string (RFC 3339) | optional; absent = unknown |
| `last_activity_at` | string (RFC 3339) | optional; absent = never used |
| `last_activity_editor` | string | optional |
| `seat_state` | string | `active` / `pending_cancellation` |

#### Worked examples

`organization-1-day` for org `g1` on `2026-08-01`:

```
body: "org g1 2026-08-01: 10 active, 4 engaged, 150 interactions"
attributes:
  source="github_copilot"  tenant_id="t1"  org="g1"
  report="organization-1-day"  day="2026-08-01"
  subject_kind="org"  subject_id="g1"
  active_users=10  engaged_users=4  total_interactions=150
  total_completions=120  ai_credits=0  net_cost_micro_usd=0
```

`users-1-day` for user `1001` (`octocat`) on `2026-08-01`:

```
body: "user 1001 2026-08-01: 42 interactions, 20 completions"
attributes:
  source="github_copilot"  tenant_id="t1"  org="g1"
  report="users-1-day"  day="2026-08-01"
  subject_kind="user"  subject_id="1001"
  user_login="octocat"  total_interactions=42  total_completions=20
  ai_credits=2  net_cost_micro_usd=25000
```

`repos-1-day` for repo `844522530` on `2026-08-01`:

```
body: "repo 844522530 2026-08-01: 3 coding, 1 review, 2 pr"
attributes:
  source="github_copilot"  tenant_id="t1"  org="g1"
  report="repos-1-day"  day="2026-08-01"
  subject_kind="repo"  subject_id="844522530"
  coding_agent_activity=3  code_review_activity=1  pull_request_activity=2
```

`user-teams-1-day` for user `1001` in team `9001` on `2026-08-01`:

```
body: "user 1001 -> team 9001 (eng-platform) 2026-08-01"
attributes:
  source="github_copilot"  tenant_id="t1"  org="g1"
  report="user-teams-1-day"  day="2026-08-01"
  subject_kind="user_team"  subject_id="1001"
  team_id="9001"  team_slug="eng-platform"
```

`billing-seats` for seat `1001` (`octocat`) snapshotted `2026-08-07`:

```
body: "seat 1001 (octocat) active 2026-08-07"
attributes:
  source="github_copilot"  tenant_id="t1"  org="g1"
  report="billing-seats"  day="2026-08-07"
  subject_kind="seat"  subject_id="1001"
  user_login="octocat"  seat_assigned_at="2026-01-01T00:00:00Z"
  last_activity_at="2026-08-01T09:30:00Z"
  last_activity_editor="vscode/1.90.0/copilot/1.200.0"  seat_state="active"
```

#### Idempotency

The usage-side natural-key upsert is keyed on `(source, day, subject_kind, subject_id)`
(ADR-0014 §3). Re-emitting a day changes no counts in the usage store.



## Verification

- Two consecutive runs over the same day change no row counts (`governance-ctl verify`).
- 28 days of history present in Postgres *and* replayable from S3 with the network off
  (`governance-ctl replay`).
- Dashboard totals reconcile against a hand-checked source report for one spot-checked day.
- Killing the credential produces a firing alert within the configured window, not silence.

## Risks and unknowns

**✅ Resolved by spike-0007: two permissions and a policy toggle.** The report endpoints
return 200 for an App holding Copilot metrics and Copilot seat management (both read) plus
`Metadata: Read` -- **`Members: read` is NOT required**, despite this RFC's earlier draft
and the vendor docs (the A/B comparison in spike-0007 removed `Members: read` and the
endpoints kept returning 200). The org's **"Copilot metrics API access policy"** must still
be enabled: it is a setting, not a permission, and until it is on, a correctly installed
App is indistinguishable from a misconfigured one (both 403).

**✅ Resolved by spike-0007: App installation tokens work on these endpoints.** The live run
minted an installation token from a GitHub App and received 200 from
`/orgs/{org}/copilot/metrics/reports/...` plus the signed-download host
`copilot-reports.github.com` (verbatim -- that host is what the Cilium `toFQDNs` egress
allowlist pins, see #12). A classic `read:org` PAT received 403 "requires admin, or
relevant organization role access", so the App-token path is the one in production.

## Open questions

1. ~~Does the installation-token spike pass?~~ **Answered by spike-0007: yes** -- see Risks above.
2. ~~Which host serves the signed download URLs?~~ **Answered by spike-0007:
   `copilot-reports.github.com`**, pinned in the Cilium egress allowlist.
3. Do we ingest `organization-28-day/latest` and `users-28-day/latest` at all, given the
   1-day reports plus backfill already produce the same window?

## Decisions produced

- [ADR-0002](../adr/0002-postgres-is-the-system-of-record-not-parquet-on-s3.md)
- [ADR-0003](../adr/0003-grafana-reads-postgres-directly.md)
- [ADR-0007](../adr/0007-api-owns-connector-metrics-no-cache-service.md)
