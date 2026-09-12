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
- The daemon annotates completed native responses with a **standard US API-equivalent
  estimate** using the dated `openai-standard-us-2026-09-12` rate card. The first
  supported model is exact `gpt-6-astra`; missing counters, unknown models and
  nonzero cache writes remain unpriced. Current live cache-write counts are zero;
  their inclusion in Codex input must be verified before pricing nonzero writes.
  [Official Astra rates](https://developers.openai.com/api/docs/models/gpt-6-astra)
  distinguish cached input and whole-request long-context pricing above 272,000
  input tokens. Computation uses checked integer micro-USD. Currency scaling in
  Grafana is presentation only. This baseline excludes fast-mode/residency
  adjustments and tool fees; it does not assert historical invoice pricing.
- Estimated cost sums only priced observations. Its accompanying coverage stat
  includes unannotated native responses, so upgrading the daemon midway through
  a selected period cannot silently make a partial sum look complete. Queries
  use observation sums consistently with the native token totals; exporter replays
  can inflate both. A per-response dedup query was rejected after the seven-day
  fleet test exceeded Loki's 500-series limit. Authoritative billing needs upstream
  deduplication in the usage store, not a label per response in Loki. No estimate is written
  into an actual-spend field. Admin billing is not connected or implemented here;
  a future authoritative reporting source must retain its own attribution grain.
- Session turn time sums durations of terminal turns **observed and ending** in the selected
  period, including interrupted turns and waits. It is neither human active time
  nor an interval union; concurrent turns can overlap. Source session/turn keys
  suppress replay before aggregation. No token counts are added from transcripts.
- Repository attribution uses launch metadata only when the recorded turn cwd
  matches the launch cwd. Other turns remain unattributed. The repository table
  sums measured turn durations, not assumed allocations of token cost. Its
  credential-derived email is read from resource attributes, distinct from the
  native events' source-provided email. These logins can differ (as in the live
  pilot). User filters select native Codex sessions, then correlate metadata by
  session ID; this does not assert that the two emails are the same identity.
  Coverage intersects measured sessions with
  native active sessions, so orphan metadata cannot inflate coverage above 100%.
- Proposed/accepted/retained lines and human edit-acceptance rates remain
  unmeasured. Structured edits missed shell changes in the live pilot, and
  `tool_decision` measures permissions that may be automated. No zero or substitute
  counter is presented for these fields.

## Layout

Eight activity summary numbers, one activity trend, two model-share donuts,
one session table, separate turn/request waiting trends, a cost estimate with two
coverage indicators, and a repository table. Session drilldown filters those same
panels. Session token columns are replaced by turn time and estimated cost, keeping
the table within the existing export width.

## Local collection

The updated `governance-auth serve --otel` annotates native OTLP JSON and protobuf
logs at admission, before the durable spool. A rate update therefore cannot change
an already-spooled observation on retry. Other signals are unaffected.

Every minute, a separate task inspects changed regular `.jsonl` files under
`$CODEX_HOME/sessions` (default `~/.codex/sessions`), after obtaining a valid
governance session. It respects persisted `last_no_codex`. Only the observed engine
version `0.154.0-alpha.6.2` is supported initially; incompatible files produce a
warning and no measurements. Native OTLP continues independently.

Discovery is bounded to 20,000 entries, four directory levels and eight changed
files per pass; each file has a 64 MiB inspection limit. Automatic export includes
terminal turns from the last 24 hours only. Recently resumed files do not backfill
years of history. Symlink entries are skipped. No transcript contents, commands,
prompts, diffs or content hashes enter the metadata payload or checkpoint. The
in-memory cache contains file signatures only. Failed parse signatures are retried
on file change or daemon restart; warnings and dashboard coverage expose gaps.
Successful signatures advance only after every generated batch is durably admitted.
Restart replays are harmless to the turn-duration queries.

For an explicit validation or bounded backfill, including continuation files:

```sh
governance-auth codex --dry-run --transcript /absolute/session.jsonl /absolute/continuation.jsonl
# Omit --dry-run to hand the same metadata to the local daemon's durable spool.
```

Metadata logs use observation time to avoid Loki rejecting late source events.
Original start/end times remain fields, and queries also filter the source end time
to the selected period. An absolute historical period ending before collection
will therefore omit later backfill: this is not a historical session ledger.
Dry runs authenticate and print only a terminal-turn count.

## Cross-client handoff

See [dashboard direction and implementation handoff](dashboard-direction-and-handoff.md)
for shared display decisions, Claude/VS Code Copilot next steps and the local-versus-central
processing tradeoffs.

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


### Measurement extension verification (2026-09-12)

With the live daemon stopped, all 558 `governance-auth` unit/integration tests
passed; the macOS fake browser launcher was corrected to intercept `open`.
After the final observation-time change, all seven metadata tests and strict
all-target clippy passed again. Nightly formatting, 17 dashboard tests, all six
deterministic generator checks, Helm lint/template, resolved JSON validation,
OIDC chart assertions and cargo-deny advisories/bans/licenses/sources passed.
The Docker-dependent collector checks were not rerun (Docker is unavailable).

Regression mutations proved that tests reject double-counting cached input,
inheriting a repository after a working-directory change, and replacing the
native-session intersection with a union. The restored tests passed.

All 28 final queries passed with this conversation's actual native user/session
filters. The 24-hour and seven-day duration queries both returned 2,486,933 ms
for 21 terminal turns, agreeing with local allowlisted metadata. Re-exporting
those turns left the total unchanged. The pilot also verified that different
Codex and governance login emails correlate through the session key without
being treated as the same identity. Fleet cost/coverage queries passed for both
24 hours and seven days after removing per-response label expansion.

The latest unreleased release binary was installed and the launchd daemon
restarted successfully. The final preview PNG rendered at 1280×1823 with the
user/session filters, duration, estimate, coverage and repository panels intact.
The provisioned production dashboard remains unchanged until the branch ships.
