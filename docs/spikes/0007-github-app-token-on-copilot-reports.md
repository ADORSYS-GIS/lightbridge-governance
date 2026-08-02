# Spike 0007 — GitHub App installation tokens on Copilot report endpoints

- Status: Findings recorded; decision made. Empirical run pending — blocked on org-admin
  rights (App creation, org install, and the policy toggle are UI-only); PR #41 is green.
- Ticket: [#7](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/7) · Epic: [#5](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/5)
- Owner: @stephane-segning · Date: 2026-08-02

## Decision

**Register a GitHub App.** GitHub App installation tokens ARE accepted on all five
org-scope report endpoints under `/orgs/{org}/copilot/metrics/reports/*` — the official
API reference now lists them as a supported token type (this ticket's premise that the
docs are silent is outdated). The App is the better credential anyway: machine identity,
no human-account dependency, revocable, scoped to exactly the permissions we grant. The
fine-grained PAT fallback stays available but is not needed.

## Evidence

| Question | Answer | Source |
|---|---|---|
| Do installation tokens work on the reports endpoints? | **Yes** — "GitHub App installation access tokens" listed for every reports endpoint, permission "Organization Copilot metrics" (read) | [REST API reference, apiVersion 2026-03-10](https://docs.github.com/en/rest/copilot/copilot-usage-metrics?apiVersion=2026-03-10) |
| Same, from a production connector | Org-level metrics works with App tokens; only *enterprise* billing/premium endpoints reject them | [navikt/copilot #111](https://github.com/navikt/copilot/issues/111), [getdx docs](https://docs.getdx.com/connectors/github-copilot-metrics) |
| Is `Members: Read` genuinely required? | Documented as required: "Read access to members, organization copilot metrics, and organization copilot seat management" | [copilot-metrics-viewer DEPLOYMENT.md](https://github.com/github-copilot-resources/copilot-metrics-viewer/blob/main/DEPLOYMENT.md) (still A/B-tested in the run) |
| Org policy toggle state | **OFF, empirically.** Probed twice on 2026-08-02 (morning and evening), identical result: `GET /orgs/adorsys-gis/copilot/metrics/reports/organization-1-day` with a `read:org` PAT returned `403 {"message":"The 'Copilot usage metrics' policy must be enabled to use this API"}`. Blocks App tokens and PATs equally. | Live `curl` (evidence appended to [#5](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/5)) |
| Is there data to fetch? | Yes — org plan is `free` with **64 filled Copilot seats** (`orgs/adorsys-gis` plan object), so reports will contain data once the toggle is enabled. | Live `gh api orgs/adorsys-gis` |
| Where is the toggle? | Org Settings → Copilot → Policies → Features → "Copilot usage metrics" → Enabled (`https://github.com/organizations/adorsys-gis/settings/policies/copilot`). UI-only, no API. Org admins today: ado-vba, francis-pouatcha, stephane-segning, yannicksiewe (verified via `gh api`). | [GitHub changelog 2025-12-16](https://github.blog/changelog/2025-12-16-track-organization-copilot-usage) |
| Signed-download host for egress policy | **`copilot-reports.github.com`** (since 2026-05-20; was `copilot-reports-*.b01.azurefd.net`). Rare fallback: `*.blob.core.windows.net`. Allowlist both, plus `api.github.com` (report endpoints + token minting). | [GitHub changelog 2026-05-20](https://github.blog/changelog/2026-05-20-copilot-usage-metrics-reports-now-use-github-owned-download-urls) |
| Token lifetime | Installation tokens expire after **1 hour** — mint per run, never persist. | Source spec `docs/rfc/sources/github-copilot-governance-mvp.md` §"tokens expire after one hour" |
| Report payloads | NDJSON behind signed URLs despite `.json` names. Org-level data exists since 2025-12-12; data freshness ~2 days. | [GitHub changelog 2025-12-16](https://github.blog/changelog/2025-12-16-track-organization-copilot-usage) |

## Verification evidence (to be completed by the runner)

Run [`spike-0007-run.sh`](./spike-0007-run.sh) as the org admin, then complete:

- [ ] `curl` status lines for each variation (baseline / after policy toggle / without `Members: Read`) — the script appends these to an evidence file
- [ ] A redacted response body proving a real report was returned
- [ ] The signed-download host, verbatim
- [ ] The App-vs-PAT decision, stated in one sentence
- [ ] Confirmation the throwaway App was deleted
- [ ] Decision comment posted on #5; this file's Status flipped to "Empirical run complete"

## Operational inputs for #12 (the collector)

- Egress allowlist: `api.github.com`, `copilot-reports.github.com`, `*.blob.core.windows.net`.
- The org policy toggle must stay **Enabled** — it is UI-only, so a re-enable needs an org
  admin. Include "is the metrics policy enabled?" in the collector's first-run diagnostics.
- Secrets in production come from ESO (`secretKeyRef`, never `optional: true`): App ID,
  private key, installation id. Nothing from this spike's env vars carries over.
- Known behaviour to record, not fix: teams with fewer than 5 seated Copilot users are
  omitted from `user-teams-1-day`.

## Sources

- [REST API endpoints for Copilot usage metrics](https://docs.github.com/en/rest/copilot/copilot-usage-metrics?apiVersion=2026-03-10)
- [REST API endpoints for Copilot user management](https://docs.github.com/en/rest/copilot/copilot-user-management)
- [Track organization Copilot usage (changelog)](https://github.blog/changelog/2025-12-16-track-organization-copilot-usage)
- [GitHub-owned download URLs (changelog)](https://github.blog/changelog/2026-05-20-copilot-usage-metrics-reports-now-use-github-owned-download-urls)
- [copilot-metrics-viewer DEPLOYMENT.md](https://github.com/github-copilot-resources/copilot-metrics-viewer/blob/main/DEPLOYMENT.md)
- [navikt/copilot #111](https://github.com/navikt/copilot/issues/111)
- Source-of-truth spec: [`../rfc/sources/github-copilot-governance-mvp.md`](../rfc/sources/github-copilot-governance-mvp.md)
