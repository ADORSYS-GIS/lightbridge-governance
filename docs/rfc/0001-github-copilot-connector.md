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
pinned and must not change without a coordinated change on both sides. (The RFC as a whole is
still Draft, but this contract section is agreed and load-bearing for the cutover; it is the
cross-repo interface both sides build against.)

#### Transport

- OTLP **log records** (not metrics, not traces) over the authenticated edge collector.
- **One log record per (report, subject)** -- i.e. one record per normalized row.
- The record **body** is a short human-readable summary; all machine-readable data lives in
  typed **attributes**.
- Money is **integer micro-USD** (`net_cost_micro_usd`, ADR-0008). No float ever appears.

#### Common attributes (every record)

| attribute | type | value |
|---|---|---|
| `source` | string | `github-copilot` (the trusted-source stamp) |
| `tenant_id` | string | deployment tenant (ADR-0001) |
| `org` | string | GitHub org scope |
| `report` | string | `organization-1-day` / `users-1-day` / `repos-1-day` / `user-teams-1-day` / `billing-seats` |
| `day` | string | `YYYY-MM-DD` (`report_day`, or `snapshot_day` for seats) |
| `subject_kind` | string | `org` / `user` / `repo` / `user_team` |
| `subject_id` | string | the natural key of the subject (for `billing-seats`, the org the seat belongs to — the seat holder is in `provider_user_id`) |

**`subject_kind` is a closed vocabulary** — `org`, `user`, `repo`, `user_team` — matching the
usage store's `CHECK (subject_kind IN ('org','user','repo','user_team'))` constraint. A record
must never carry a kind outside this set; the usage-side normalizer rejects unknown kinds. A
`billing-seats` record is an **org** subject (see below), never a `seat` kind.

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

**`billing-seats`** — `subject_kind=org`, `subject_id=org` (the entity the seat belongs to); the seat holder is carried in `provider_user_id`

| attribute | type | unit |
|---|---|---|
| `provider_user_id` | string | the seat holder |
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
  source="github-copilot"  tenant_id="t1"  org="g1"
  report="organization-1-day"  day="2026-08-01"
  subject_kind="org"  subject_id="g1"
  active_users=10  engaged_users=4  total_interactions=150
  total_completions=120  ai_credits=0  net_cost_micro_usd=0
```

`users-1-day` for user `1001` (`octocat`) on `2026-08-01`:

```
body: "user 1001 2026-08-01: 42 interactions, 20 completions"
attributes:
  source="github-copilot"  tenant_id="t1"  org="g1"
  report="users-1-day"  day="2026-08-01"
  subject_kind="user"  subject_id="1001"
  user_login="octocat"  total_interactions=42  total_completions=20
  ai_credits=2  net_cost_micro_usd=25000
```

`repos-1-day` for repo `844522530` on `2026-08-01`:

```
body: "repo 844522530 2026-08-01: 3 coding, 1 review, 2 pr"
attributes:
  source="github-copilot"  tenant_id="t1"  org="g1"
  report="repos-1-day"  day="2026-08-01"
  subject_kind="repo"  subject_id="844522530"
  coding_agent_activity=3  code_review_activity=1  pull_request_activity=2
```

`user-teams-1-day` for user `1001` in team `9001` on `2026-08-01`:

```
body: "user 1001 -> team 9001 (eng-platform) 2026-08-01"
attributes:
  source="github-copilot"  tenant_id="t1"  org="g1"
  report="user-teams-1-day"  day="2026-08-01"
  subject_kind="user_team"  subject_id="1001"
  team_id="9001"  team_slug="eng-platform"
```

`billing-seats` for seat `1001` (`octocat`) snapshotted `2026-08-07`:

```
body: "seat 1001 (octocat) active 2026-08-07"
attributes:
  source="github-copilot"  tenant_id="t1"  org="g1"
  report="billing-seats"  day="2026-08-07"
  subject_kind="org"  subject_id="g1"  provider_user_id="1001"
  user_login="octocat"  seat_assigned_at="2026-01-01T00:00:00Z"
  last_activity_at="2026-08-01T09:30:00Z"
  last_activity_editor="vscode/1.90.0/copilot/1.200.0"  seat_state="active"
```

#### Idempotency

The usage-side natural-key upsert is keyed on `(source, day, subject_kind, subject_id)`
(ADR-0014 §3). Re-emitting a day changes no counts in the usage store.

**`billing-seats` is the exception.** Because a seat snapshot is an **org** subject
(`subject_kind=org`, `subject_id=org`), every seat on a snapshot day would share the same
`(source, day, subject_kind, subject_id)` key. The seat-snapshot upsert is therefore keyed on
`provider_user_id` (the seat holder, the table's NOT NULL PK) in addition to `(source, day)`;
re-emitting a snapshot day still changes no counts. See known-issue #5 for the coordinated
change this requires on the usage-side normalizer.



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

## Known issues and follow-ups (from review of the OTLP day-grain emit)

Items raised by the review of the OTLP day-grain emit work (the `Sink`/`emit.rs`/`metrics.rs`
path and the RFC-0001 encoding contract it pins). Tracked here so a reader of this contract doc
sees them without going back to the PR thread. Status reflects the governance-side mitigation
landed in the emit work; the **coordinated `lightbridge-authz` normalizer changes remain the
cross-repo prerequisite for cutover** and are called out per item.

1. **`user-teams-1-day` `subject_id` is not unique per record (P1).** `subject_id` is set to
   `user_id`, but the usage-side natural-key upsert is keyed on
   `(source, day, subject_kind, subject_id)` (see *Idempotency* above). A user who belongs to
   more than one team produces two records on the same day with the same key, which collide
   and merge on the pinned key -- one membership is silently lost. Fixing this changes the
   pinned key and therefore requires a coordinated change on the usage-side normalizer in
   `lightbridge-authz`; it cannot be done safely on the governance side alone.
   **Governance-side mitigation (landed):** the emitter now **skips `user-teams-1-day`
   entirely** -- the authz-side receiver refuses it (lightbridge-authz#751), so emitting it
   would be rejected and break the cutover count assertions. The report's rows are still
   written to Postgres in shadow mode; they are simply not emitted as OTLP until the key is
   fixed. **Do not cut over the day-grain emit for multi-team orgs until the normalizer key is
   fixed.**

2. **No test constructs a `Sink` (P2) — resolved.** The `Sink::emit_rows` path is now
   exercised by unit tests in `emit/mod.rs` using `opentelemetry_sdk::logs::InMemoryLogExporter`
   (accepted/rejected accounting, org-cost aggregation, and the collision guard), so the emit
   path (encode -> OTLP -> stats accounting) is covered without a network.

3. **`user-teams` aside, a day whose emit fails inside a gap-fill is never re-emitted (P3).**
   In `ingest_day`, `sync_day` (rows + manifest `status="ok"`) runs before the emit block, so
   a failed `emit_rows` returns `Err` after the manifest already advanced the high-water mark.
   `backfill_window`'s trailing lookback re-covers the recent days, so a recent failure
   self-heals, but a day in the middle of a cold-start backfill (wider than the lookback)
   falls outside it, and `Replay` is not given a sink, so no command re-emits it. Mitigated
   while Postgres remains authoritative (until the cutover). **Under the freeze (the cutover
   path) this is further mitigated by the F1 fix: no manifest is written during a freeze, so
   the high-water mark never advances and a failed day stays inside the next run's window and
   is re-attempted.** Still open in shadow mode.

4. **loc-gate is red (P2) — resolved.** The `loc-gate` check exceeded several LoC ceilings at
   this head: `emit.rs` (637 > 200), `main.rs` (346 > 328), `metrics.rs` (608 > 499),
   `sync/operators.rs` (222 > 200), and `crates/governance-copilot/src/sync.rs` (280 > 246).
   The over-ceiling files were split into focused submodules, each under the 200-LoC ceiling:
   `emit/` (mod + helpers + tests), `sync/backfill/` (mod + window + outcome + ingest + tests),
   `sync/cutover/` (mod + export + verify + decommission + tests), `sync/operators/` (mod +
   replay), and `crates/governance-copilot/src/sync/` (mod + day + seats). The gate now passes.

5. **`billing-seats` `subject_kind`/`subject_id` were pinned to a vocabulary the usage store
   rejects (P1).** The encoding previously stamped `subject_kind="seat"` and
   `subject_id=provider_user_id`, but `usage_seat_snapshots` enforces
   `CHECK (subject_kind IN ('org','user','repo','user_team'))` and documents `subject_id` as
   "the org/team/entity this seat belongs to" with a distinct NOT NULL `provider_user_id` PK
   column. The encoding is now `subject_kind=org`, `subject_id=org`, with the seat holder in a
   dedicated `provider_user_id` attribute. This is a coordinated change with the usage-side
   normalizer in `lightbridge-authz` (it must map `provider_user_id` onto the seat-snapshot
   PK); it cannot be cut over until that side accepts the corrected encoding.
   **Governance-side mitigation (landed):** the collision guard keys seat records on
   `(day, provider_user_id)` (the table's PK), so a duplicate seat holder in one snapshot is
   refused under the freeze rather than silently merged. **Do not cut over the day-grain emit
   until the normalizer accepts the corrected encoding.**

6. **Freeze must not write to the governance telemetry tables (F1) — resolved.** Under
   `CUTOVER_FREEZE_WRITES` the emitter is the only write path. Two guards enforce this:
   (a) the empty-report branch in `governance-copilot::sync::ingest_one` no longer writes an
   `ingest_manifests` row during a freeze (previously it advanced the high-water mark for empty
   days while non-empty days left none, splitting the watermark); and (b) `export-counts` /
   `verify-counts` / `decommission` refuse to run while the freeze is on, because
   `ingest_manifests` is frozen and verification against it would report a false green. Run
   those operators **before** enabling the freeze.

7. **Count-assertion export shape (aligned with lightbridge-authz#751) — resolved.** The
   `export-counts` output is the authz `verify-counts` CLI's `VerifyManifest` shape:
   `day_facts` (the three day-fact reports), `seat_snapshots` (`billing-seats` per-day), and
   top-level `executions`/`model_calls`/`tool_calls`. `user-teams-1-day` is excluded from the
   export because the authz receiver refuses it (known-issue #1) and it is therefore not present
   in the usage store -- asserting it would always mismatch and block the cutover.

## Decisions produced

- [ADR-0002](../adr/0002-postgres-is-the-system-of-record-not-parquet-on-s3.md)
- [ADR-0003](../adr/0003-grafana-reads-postgres-directly.md)
- [ADR-0007](../adr/0007-api-owns-connector-metrics-no-cache-service.md)
- [ADR-0008](../adr/0008-money-is-integer-micro-usd.md) (money as integer micro-USD in the OTLP contract)
- [ADR-0014](../adr/0014-usage-telemetry-consolidates-into-the-authz-usage-store.md) (the OTLP day-grain emit is the ADR-0014 Decision 2 sink)
