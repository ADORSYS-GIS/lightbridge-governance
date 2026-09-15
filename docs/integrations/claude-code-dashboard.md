# Claude Code user and session dashboard

The generator (`scripts/generate_claude_code_dashboard.py`) owns the committed
JSON; Helm substitutes two datasource UIDs -- Loki (existing forwarded OTLP
logs, no new persistence or collector path, ADR-0014) and, since the
lines-of-code/active-time/commits/pull-requests panels below, Mimir/
Prometheus -- and Grafana Operator provisions it. Reshaped 2026-09-14 from
the original fleet-wide dashboard (PR #317, #331) to match
[Codex's](codex-dashboard.md) user/session contract, per
[dashboard direction and implementation handoff](dashboard-direction-and-handoff.md).

## Source and measurement contract

- [Claude Code monitoring reference](https://code.claude.com/docs/en/monitoring-usage).
- Live metadata inspected on 2026-09-09 (initial field discovery, 13 confirmed
  `event.name` values across a real 24h fleet window) and re-verified on
  2026-09-14 against **this development conversation's own session**, live,
  while it was running (`kubectl -n observability port-forward svc/loki-gateway
  3100:80`, no synthetic fixture). The 2026-09-14 check confirmed
  `attributes_session_id` matched this conversation's own session id exactly,
  `attributes_user_email` was `hello@vymalo.com`, `attributes_model` was
  `claude-sonnet-5` (a value the 2026-09-09 pass never saw), and
  `attributes_cost_usd`/`attributes_cost_usd_micros` were consistent on the
  same `api_request` lines. A single session id carried hook, MCP, plugin and
  retention events alongside prompts and API calls -- the basis for scoping
  those sections by the same user/session filters as everything else, not
  leaving them fleet-wide as the original version did.
- Email and session ID are JSON attributes, not indexed stream labels. The
  User email and Session textboxes perform literal substring searches using
  Grafana's regex escaping; empty means all. A session link retains the time
  range and user search. These are display filters, not an authorization
  boundary -- same caveat as Codex's dashboard.
- All snapshots use the selected range (`$__range`) and instant queries.
  Trends use `$__interval`. Refresh is 15 minutes (this chart's shared
  default); default range 24h. No fixed 24h/7d query windows remain, matching
  the #318 Loki OOM fix already applied to the other four generators.
- Active sessions are distinct nonempty `attributes_session_id` values with a
  `user_prompt`, `api_request`, or `tool_result` event in the period
  ("meaningful" activity -- hook/MCP/plugin/retention housekeeping and the
  rare `api_error`/`compaction`/`subagent_completed` diagnostics do not count
  toward session detection or the activity trend, though each still has its
  own scoped panel).
- Cost is Claude Code's own reported `attributes_cost_usd` -- a real,
  source-emitted figure, **not** a governance-computed estimate like Codex's
  daemon-side pricing annotation. It is not necessarily identical to an
  invoice; no admin billing connector is implemented or claimed.
- Input/output/cache-read/cache-creation tokens are four independent
  `unwrap` fields. Unlike Codex's `cached_token_count` (documented as a
  subset of input), Anthropic's own usage accounting reports cache read and
  cache creation as independent counters. This dashboard therefore shows
  cache read tokens as their own total, not a "cache share" ratio that would
  assume an unconfirmed subset relationship. Model-token share (the
  "Models · tokens" donut) adds only input and output, for the same reason.
- Request waiting uses `attributes_duration_ms` on `api_request`. Claude Code
  has no separate turn-level time-to-first-token event the way Codex's
  `codex.turn_ttft` does; a request can be one of several per visible
  assistant turn (tool-use loops), so this is a different population from
  human waiting time or a single visible reply.
- Session first/last activity uses minimum/maximum Loki entry timestamps of
  meaningful events inside the selected range -- observed bounds, not true
  session lifecycle timestamps or human active time. The outer tabular join
  retains sessions missing a signal; cells stay blank, never zero.
- Identity coverage is the fraction of meaningful records with a nonempty
  `attributes_user_email`, fleet-wide over the selected period. It
  deliberately ignores the user/session filters. A populated email is not
  proof of employee identity.
- Tool-decision figures split `attributes_decision` by `attributes_source`:
  `config` is a sandbox/permission-mode default, not a person clicking
  approve. The 2026-09-09 investigation found 702/706 decisions in a 24h
  fleet window were `accept`/`config`, and 12,585/12,586 code-editing
  decisions (`Edit`/`Write`/`NotebookEdit`) over 7d were `accept`/`config`
  with zero `source=user` observed. The "Code edits · source=config share"
  panel keeps that split, now over the selected range instead of a fixed 7d.
- Lines of code, active time, commits and pull requests are `claude_code.*`
  OTLP **metrics** (Mimir/Prometheus), not the log events this generator's
  other panels read from Loki. The 2026-09-09 investigation found them
  absent from this org's Mimir and left them unshown, reasoning there was
  no honest substitute for a literal line count. Root cause, found later the
  same day: Claude Code's documented default for
  `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` is `delta`, which has
  no representation in Prometheus/Mimir's cumulative-only data model, and
  nothing downstream converted one to the other -- fixed in `governance-auth`
  (lightbridge-governance#335). These four panels are real Mimir data,
  scoped by the same `user`/`session` filters (translated to PromQL label
  matchers on `user_email`/`session_id`), verified live after the fix.
  Coverage is partial by design: only a machine running a `governance-auth`
  build with #335 that has re-run `configure` emits `cumulative`
  temporality, so a blank panel here means an un-updated machine, never
  zero activity -- each panel's own `NO DATA` mapping says so.
- Hooks (`hook_execution_start`/`.complete`), MCP server connections
  (`mcp_server_connection`), plugin loads (`plugin_loaded`) and data-retention
  hygiene (`retention_sweep`) have no analogue in Grafana's own official
  integration dashboard (25052) -- Claude Code's genuine differentiators,
  preserved from the original version and now scoped like every other panel.

## Layout

A text panel, eight range-scoped summary stats (one -- identity coverage --
explicitly fleet-wide), one meaningful-activity trend, two model-share
donuts, one session table with drilldown links, tool-decision/success/
invocation-source panels, a waiting-time trend, an error trend plus raw error
log lines, a hooks section (outcomes trend + top-hooks table), an MCP/plugins
section, four retention stats, and (section 7, Mimir/Prometheus) five stats
-- lines added, lines removed, active time, commits, pull requests -- plus
a lines-added-vs-removed trend.

## Cross-client handoff

See [dashboard direction and implementation handoff](dashboard-direction-and-handoff.md)
for shared display decisions, VS Code Copilot next steps, and the
local-versus-central processing tradeoffs that also apply here (this
dashboard reads existing forwarded OTLP; no new laptop-side parser was added
for Claude Code, unlike Codex's local metadata adapter).

## Verification

```sh
python3 scripts/generate_claude_code_dashboard.py --check
python3 -m unittest discover -s scripts -p 'test_*.py'
helm lint charts/lightbridge-governance
helm template dashboard-check charts/lightbridge-governance --set grafanaDashboard.enabled=true
```

### 2026-09-14 verification

All 30 dashboard tests pass (11 in `test_claude_code_user_dashboard.py`,
mirroring `test_codex_user_dashboard.py`'s contract style: deterministic
output, every usage query carries the user/session/range filters except the
one fleet-wide coverage indicator, no fixed `[24h]`/`[7d]` windows remain, the
model-share donuts use named-field rows without a timestamp label, the
session join preserves missing signals and the drilldown link, no panel
overlaps another, no invented measurement -- "accepted", "retained", "active
duration" -- appears in any title ("lines of code" was in that list until
the metrics-temporality fix made it real), and the Mimir panels are
correctly sourced, scoped, `NO DATA`-mapped, and decimal-rounded). All six
generator `--check`s and `helm lint`/`helm template` (with the Grafana CR
enabled) pass; the embedded dashboard JSON round-trips through Python's
`json.loads` and carries the `user`/`session` textbox variables.

Every field this dashboard's queries reference was independently confirmed
live against this development conversation's own session while it was
running (see the source contract above) -- not a fixture, not the 2026-09-09
investigation reused unchecked. `just chart-checks`'s OIDC/XFF/OTel-config
assertion scripts could not be run in this environment (the installed `yq` is
the Python/jq-wrapper build, not the Go `mikefarah/yq` build those scripts
expect); this is a pre-existing environment gap unrelated to this dashboard
and was not introduced by this change.

### Live Grafana preview verification (2026-09-14)

Done, with explicit go-ahead. A `GrafanaDashboard` CR was applied under a
distinct preview UID (`governance-claude-code-preview`, same mechanism as
Codex's own `governance-codex-preview`) in the `observability` namespace,
alongside -- never overwriting -- the untouched production UID
(`governance-claude-code-telemetry`). The Grafana Operator reported
`DashboardSynchronized`/`ApplySuccessful` immediately.

Viewed live (after the user authenticated the session; Claude does not
handle Grafana credentials):

- Fleet-wide, unfiltered, last 24h: 6 active sessions, 208 prompts, $2.43K
  total cost, three real models (`claude-opus-5` 64% of responses,
  `claude-sonnet-5` 36%, `claude-haiku-4-5-20251001` 1%), identity coverage
  100%. A previously undocumented `tool_decision` source value,
  `reject/hook`, appeared live (17 occurrences) alongside the already-known
  `accept/config`, `accept/user_temporary`, `reject/config` -- a hook
  blocking a tool call, a real signal this reshape did not previously surface.
- Filtering `User email` to `hello@vymalo.com` (this session's own account,
  applied via URL and confirmed via a fresh page load) correctly narrowed
  every scoped panel to 2 sessions, 8 prompts, $6.83, 100% `claude-sonnet-5`
  -- and left "Identity coverage · fleet" at 100% unchanged, exactly the
  fleet-wide exception the contract above claims.
- Filtering `Session` alone to this conversation's own session ID narrowed
  to exactly 1 active session, with cost and token counts that grew between
  two checks made minutes apart -- confirming the numbers are live, not
  cached, mid-conversation.
- The session table's own row for this conversation matched its live
  values at every check.
- Export PNG (`/render/d/governance-claude-code-preview?...`) returned
  `200`, 1280×3077, full page. Every section -- stats, activity trend,
  model donuts, session table with drilldown links, tool decisions/success,
  code-edit share, invocation source, waiting time, errors (raw log lines,
  metadata only, no prompt content), hooks, MCP connections, plugins,
  retention -- rendered with real data and no panel overlap.

The preview `GrafanaDashboard` CR was deleted immediately after this check;
production is unaffected until this branch is released and deployed through
the normal pipeline.

### Lines of code / active time / commits: live Mimir verification (2026-09-14)

Done, same mechanism, a second temporary preview CR (`governance-claude-code-
preview2`), deleted immediately after. Grafana Operator again reported
`DashboardSynchronized`/`ApplySuccessful`. Queried the five new panels' exact
PromQL expressions directly against Mimir (fleet-wide, last 24h, no
user/session filter):

- Lines added: 187.2. Lines removed: 26.1. Active time: 466s. All real,
  plausible numbers, not zero or an error.
- Commits and pull requests both returned no data -- correct: nobody in this
  fleet had made a commit or opened a pull request through Claude Code
  recently. The panels' `NO DATA` mapping shows this honestly rather than a
  fabricated zero, exactly the "unknown is not zero" contract this
  dashboard's own generator and `dashboard-direction-and-handoff.md`'s
  measurement agreement both require.

### Review findings and fixes (PR #336, review by stephane-segning)

An adversarial review of this PR caught three things this document's own
first draft did not, and the fixes are recorded here rather than only in
the PR diff, since a future reader of this file needs the same warning:

- **P1 (shipped broken, now fixed):** `PROM_FILTER` originally quoted the
  `user`/`session` textbox substitution with PromQL double quotes. Grafana's
  `:regex` format escapes a `.` in an email to `\.`; a PromQL double-quoted
  string interprets that backslash as an escape sequence, which the
  generator's own `logs()` function already routes around for LogQL with
  backtick (raw) strings. `PROM_FILTER` didn't, so every one of the six new
  Mimir panels broke the moment the User email textbox held a real email --
  exactly the drilldown case they were built for. This PR's own live
  verification above only exercised the fleet-wide (unfiltered) case, which
  has nothing to escape and never triggered it. Fixed by quoting
  `PROM_FILTER` with backticks instead; the regression test that used to
  assert the double-quoted (broken) string now asserts the backtick one and
  additionally asserts the double-quoted form is absent, and was confirmed
  to fail for the predicted reason when the bug was reintroduced.
- **P2, this file:** contradicted `dashboard-direction-and-handoff.md` on
  whether the live preview had happened, said "one datasource token" where
  there are now two, omitted section 7 from the Layout list, still claimed
  lines-of-code/active-time/commits were absent with "no substitute proxy"
  after the fix had already landed, and reported 27 tests (10 new) where the
  real counts are 30 (11 new). All fixed above.
- **P3:** `increase()` over a Prometheus counter is a fractional
  extrapolation to the range boundary, not an exact integer reconciliation,
  so an un-rounded "Commits"/"Pull requests" stat could render `1.14`. Fixed
  with an explicit `decimals: 0` on every panel `prom_stat_panel` produces
  (the only unit these five panels use is a count or whole seconds, so 0 is
  correct everywhere it currently applies, not merely a default).

Also flagged, and fixed in the same pass: no CI job ran `python3 -m
unittest discover -s scripts -p 'test_*.py'` or any of the six generators'
`--check`, so the "committed JSON matches the generator" guarantee this
document repeatedly claims was enforced by convention only -- a stale
commit (hand-edited JSON, or a generator changed without regenerating)
would have merged clean. Added as `dashboard-checks` in `.github/workflows/
ci.yml`, enumerating all six generator scripts by name rather than a glob
(`generate_dashboards.py`, the AI CLI overview's own historical name,
doesn't match a `generate_*_dashboard.py` shape and a glob would have
silently skipped it).
