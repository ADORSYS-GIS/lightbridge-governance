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

- **S3 raw:** `s3://ssegning-k8s-state/lightbridge-governance/raw/tenant=<t>/org=<o>/day=<d>/<report>.ndjson`
  plus a manifest object. Deterministic keys, overwrite-safe.
- **Postgres:** `copilot_org_daily`, `copilot_user_daily`, `copilot_repo_daily`,
  `copilot_user_teams`, `copilot_seat_snapshot`, upserted `ON CONFLICT DO UPDATE`.

### Identity

`user-teams-1-day` gives GitHub-login -> team. `identity_map` exists only to reach *internal*
identity and cost centre, joined to Keycloak `user_entity` by verified email (the ai-helm
ADR-0063 datasource). **Never match on display name.**

## Verification

- Two consecutive runs over the same day change no row counts (`governance-ctl verify`).
- 28 days of history present in Postgres *and* replayable from S3 with the network off
  (`governance-ctl replay`).
- Dashboard totals reconcile against a hand-checked source report for one spot-checked day.
- Killing the credential produces a firing alert within the configured window, not silence.

## Risks and unknowns

**⚠️ The access model has three permissions and a policy toggle, not one permission.**
The report endpoints return 400/403 unless the App holds Copilot metrics, Copilot seat
management **and Members** (all read) -- and then still 403 until an org owner enables the
organization's **"Copilot metrics API access policy"**, which is a setting, not a
permission. A correctly installed App with every box ticked is indistinguishable from a
misconfigured one until that is on.

**⚠️ App installation tokens are not documented as supported on these endpoints.** GitHub's
docs describe the fine-grained permission and the PAT scope but do not state that
installation tokens work. Community reports say they do at org scope with the three
permissions above; enterprise scope definitively rejects them. **Spike this before writing
the connector** -- a throwaway App and one curl. Fallback: a fine-grained PAT in
`ssegning-aws`, which is structurally cheap because the credential layer resolves to a
bearer token either way.

## Open questions

1. Does the installation-token spike pass? (Blocks the choice of credential, nothing else.)
2. Which host serves the signed download URLs? It determines the `toFQDNs` egress allowlist
   and can only be answered by looking at a real response.
3. Do we ingest `organization-28-day/latest` and `users-28-day/latest` at all, given the
   1-day reports plus backfill already produce the same window?

## Decisions produced

- [ADR-0002](../adr/0002-postgres-is-the-system-of-record-not-parquet-on-s3.md)
- [ADR-0003](../adr/0003-grafana-reads-postgres-directly.md)
- [ADR-0007](../adr/0007-api-owns-connector-metrics-no-cache-service.md)
