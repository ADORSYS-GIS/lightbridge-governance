# Codex user and session dashboard

The generator (`scripts/generate_codex_dashboard.py`) owns the committed JSON;
Helm substitutes the Loki datasource UID and Grafana Operator provisions it.
This reads existing forwarded OTLP logs, without a new persistence or collector path
(ADR-0014). It replaces the earlier Codex usage/host/permission dashboard.

## Source and measurement contract

- [Codex telemetry reference](https://learn.chatgpt.com/docs/config-file/config-advanced#observability-and-telemetry).
- Live metadata inspected on 2026-09-12 in Loki for this development conversation,
  matching both `codex-app-server` and `codex_cli_rs` jobs. Field existence was
  checked without retrieving prompt/tool content. The evidence establishes this
  client's capability, not every client version or authentication mode.
- Email and conversation ID are JSON attributes, not indexed stream labels.
  The User email and Session textboxes perform literal substring searches using
  Grafana's regex escaping; empty means all. A session link retains the time range
  and user search. These are display filters, not an authorization boundary.
- All snapshots use the selected range and instant queries. Trends use rolling
  intervals with a five-minute minimum. Refresh is 15 minutes; default range 24h.
  No fixed 24h/7d query windows or range-stat amplification from #318 remain.
- Active sessions are distinct nonempty conversation IDs with a prompt, tool
  result, or completed model response in the period. Resuming does not create
  another distinct session. Launch counts are not used.
- Meaningful activity separates `codex.user_prompt`, `codex.tool_result`, and
  `codex.sse_event` with `event.kind=response.completed`. These are observed
  records; retries/replays are not independently deduplicated. Prompts can include
  system-generated follow-ups and tool results can include orchestration tools.
- Input/output/cached token counts come only from completed responses. Cached
  input is a subset of input; the cache ratio divides it by input. Model-token
  share adds only input and output. Do not add reasoning or tool token fields
  on top. Missing components remain unavailable rather than becoming zero.
- Turn waiting uses `codex.turn_ttft.duration_ms`; request waiting uses
  `response.completed.ttft_ms`. Both are milliseconds. Median/p95 are computed
  over samples, not averaged from per-stream quantiles or summed across time.
  Request waiting covers completed responses only. Neither is total session wait.
- Session first/last activity uses minimum/maximum Loki entry timestamps of
  meaningful events inside the selected range. These are observed bounds, not
  true session lifecycle timestamps or active human time. The outer tabular join
  retains sessions missing token/timing measurements; cells remain blank.
- Identity coverage is the fraction of meaningful records with a nonempty
  `user.email`, fleet-wide over the selected period. It deliberately ignores
  user/session filters. Empty-email traffic remains in the all-users view.
  A populated source email is not proof of employee identity; no hostname fallback.
- No dollar cost, proposed/accepted/retained line count, repository attribution,
  active-time measurement or human edit-acceptance rate is invented. In particular,
  `tool_decision` measures permissions and may describe automated approvals.

## Layout

Eight summary numbers, one meaningful-activity trend, two model-share donuts,
one session table, and separate turn/request waiting trends. Session drilldown
filters those same panels rather than opening a duplicate detail dashboard.

## Verification

```sh
python3 scripts/generate_codex_dashboard.py --check
python3 -m unittest discover -s scripts -p 'test_*.py'
helm lint charts/lightbridge-governance
helm template dashboard-check charts/lightbridge-governance --set grafanaDashboard.enabled=true
```

Tests cover selected-range/filter propagation, instant snapshots, meaningful-event
selection, non-overlapping token categories, timing populations, session join/link
contracts, deterministic JSON, and non-overlapping panel positions. Replacing
instant targets with range targets was mutation-tested: the query contract fails.

All 24 query targets executed successfully against the development conversation
in a fixed two-hour window on 2026-09-12, returning aggregate values only. Session
summary values agreed with the corresponding headline values. This is not a
benchmark for every fleet-wide range. Helm rendered all six dashboard JSON
resources with resolved datasource tokens; the OIDC chart assertion also passed.
The Docker-dependent collector checks require a running Docker daemon.

Live Grafana preview validation also caught and corrected two presentation bugs:
Loki instant model rows must be converted to named fields (excluding the timestamp)
for separate donut slices, and dashboard links use `keepTime`, not `includeTime`.
The latter fix is shared across all six generated dashboards. Regression mutations
restoring the wrong time-link property or including timestamps in model labels fail.
User plus session searches and the session-row link were exercised in Grafana;
filtering produced one active session and retained the selected period. A PNG
export completed at 1280×1443 with the user/session filters preserved.
