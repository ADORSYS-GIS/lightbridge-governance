# Verify (or refute) VS Code Copilot's lines-of-code / edit-outcome metrics

**Status: not yet executed.** This is a much lighter check than
[verify-codex-metrics-temporality.md](./verify-codex-metrics-temporality.md)
-- no daemon rebuild, no instrumentation. It only needs a real VS Code +
Copilot Chat session doing one specific thing, then a Mimir query.

## Why this exists

`scripts/generate_vscode_copilot_dashboard.py` queries
`copilot_chat_lines_of_code_count_total` and
`copilot_chat_chat_edit_outcome_count_total`. Its own docstring claims "these
copilot_chat_* counters were confirmed against production." Checked live on
2026-09-15: **both are completely absent from Mimir**, unconstrained by any
label, over the full 7-day window.

This is a different shape of problem from Claude Code (#335) or Codex
(`verify-codex-metrics-temporality.md`), and the evidence already rules out
the same root cause:

- `job="copilot-chat"` has 28 *other* distinct metric names in that same
  7-day window, all real and populated: `copilot_chat_session_count_total`,
  `copilot_chat_tool_call_count_total`, `copilot_chat_agent_turn_count_bucket`
  (a genuine Histogram -- these cannot silently misrepresent under the wrong
  temporality the way a Sum can), `gen_ai_client_token_usage_*`, and more.
  A systemic delta-vs-cumulative bug would affect every Sum from this client
  uniformly, not two specific ones while 26 others work.
- The two missing metric names match the [official VS Code monitoring
  docs](https://code.visualstudio.com/docs/agents/guides/monitoring-agents)
  exactly (`copilot_chat.lines_of_code.count` /
  `copilot_chat.chat_edit.outcome.count`, correctly converted dot-to-underscore
  with the `_total` suffix Prometheus adds for a monotonic Sum) -- this is not
  a wrong-name guess.
- The dedicated `lightbridge-governance-copilot-otel` collector's own deployed
  config (`kubectl get opentelemetrycollector lightbridge-governance-copilot-otel
  -n governance -o jsonpath='{.spec.config}'`) has exactly one processor,
  `resource` (renames `service.name`, strips a few k8s/process attributes) --
  no filter, no metricstransform. Nothing in this org's own pipeline drops
  these two metric names specifically.
- Fleet `target_info` for `job="copilot-chat"` shows three real installed
  versions across real engineers: `0.61.0`, `0.64.1`, `0.65.0`. The
  `microsoft/vscode-copilot-chat` repo is archived (as of 2026-05-20) and its
  public `CHANGELOG.md` stops at `0.41` (2026-03-25) -- well before any
  version in this fleet -- so it cannot confirm or rule out when these two
  specific metrics were added, or whether these versions emit them at all.

Per the docs, both metrics specifically require Copilot Chat's *file-editing*
workflow (`lines_of_code.count`: "lines added or removed by accepted agent
edits"; `chat_edit.outcome.count`: "file-level chat editing session
outcomes") -- not just asking a question or calling a read-only tool. The
leading hypothesis is that nobody in the sampled window used that specific
workflow, not that it's broken -- but that's unconfirmed, which is what this
check settles.

## The check

1. In VS Code, with the `lightbridge-governance` extension active and
   `governance-auth doctor` reporting all checks passed (confirm this first
   -- an out-of-date `governance-auth` binary can reject its own config file
   outright, as happened on 2026-09-15: `no_claude`/`no_codex`/`no_vscode`/
   `codex_telemetry_only` were added to the config schema in v2.8.0 (PR #329);
   a v2.7.0 binary refuses to start with a config file a newer `configure`
   already wrote. Fix: `governance-auth self update`.)
2. Open Copilot Chat, use its agentic edit mode to make a real change to a
   file (not just ask a question -- actually have it propose an edit).
3. **Accept** the edit (or deliberately reject one, if you want to check the
   `copilot_chat_edit_outcome` label's other value too).
4. Wait 2-3 minutes for the periodic metrics export.
5. Check Mimir directly:
   ```sh
   kubectl -n observability port-forward svc/mimir-nginx 3210:80 &
   END=$(date +%s); START=$((END-600))  # last 10 minutes is enough now
   curl -s --get "http://127.0.0.1:3210/prometheus/api/v1/series" \
     -H "X-Scope-OrgID: anonymous" \
     --data-urlencode 'match[]={__name__=~"copilot_chat_lines_of_code_count_total|copilot_chat_chat_edit_outcome_count_total"}' \
     --data-urlencode "start=$START" --data-urlencode "end=$END"
   ```

## Reading the result

- **If either series now appears**: confirmed usage gap, not a bug -- the
  dashboard's assumption was correct, it just had no data yet in the window
  originally checked. Update this file (and the dashboard generator's
  docstring, which currently states this as unconditionally "confirmed") to
  say so, with the date and what triggered it.
- **If neither appears even after this deliberate trigger**: real gap,
  version-specific or otherwise. Worth a proper live investigation at that
  point -- check whether a *newer* Copilot Chat version (beyond 0.65.0)
  changes anything, and whether VS Code's own telemetry consent/content-
  exclusion settings gate this specific pair of metrics (unconfirmed either
  way from the public docs checked so far).
