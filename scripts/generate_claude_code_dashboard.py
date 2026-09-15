#!/usr/bin/env python3
"""Generate the Claude Code user/session dashboard from verified Loki event metadata.

DEV-TIME TOOL ONLY, same convention as the other four dashboard generators in
this directory (see `generate_dashboards.py`'s own docstring for the full
rationale). Standard library only. The **generated JSON is committed** at
`charts/lightbridge-governance/dashboards/claude-code-telemetry.json`; that
file, not this script, is what `helm template`/the Grafana Operator actually
reads. Run with `--check` to verify the committed JSON without modifying it.

Source semantics and live validation: docs/integrations/claude-code-dashboard.md.

## History: why this looked different before 2026-09-14

The first version of this generator (PR #317, then #331) built a fleet-wide,
`[24h]`/`[7d]`-hardcoded dashboard -- useful for the initial field-discovery
work (13 confirmed `event.name` values, the `tool_decision` auto-approval
finding, hooks/MCP/plugins/retention panels with no analogue in Grafana's own
official integration dashboard 25052 -- see git history for that investigation
in full) but not shaped like Codex's dashboard once Codex was reworked into a
user/session view (`generate_codex_dashboard.py`,
docs/integrations/dashboard-direction-and-handoff.md's "Claude Code:
next-agent work" section). This revision reshapes Claude Code the same way:
textbox `user`/`session` filters, `$__range`/`$__interval` instead of fixed
windows, a session table with drilldown links, and one explicitly fleet-wide
coverage indicator instead of an implicit assumption that every panel means
"everyone, always". The 13-event-type inventory, the hooks/MCP/plugins/
retention panels, and the tool-decision auto-approval split are preserved
(Claude Code's genuine differentiators over 25052) -- reshaped, not dropped.

## Reshape validated live 2026-09-14, against THIS development conversation

Via `kubectl -n observability port-forward svc/loki-gateway 3100:80`, querying
for this exact session while it was running (not a synthetic fixture):

  - `attributes_session_id` in Loki matched this conversation's own session
    directory name byte-for-byte; `attributes_user_email` was
    `hello@vymalo.com`, `attributes_app_entrypoint` was `claude-desktop` --
    this session runs inside the Claude desktop app, and that is exactly what
    telemetry reported.
  - `attributes_model` was `claude-sonnet-5` -- a model value the original
    2026-09-09 investigation never saw (confirming the model inventory drifts
    and panels must not hardcode one).
  - `attributes_cost_usd` and `attributes_cost_usd_micros` were confirmed
    consistent on the same `api_request` lines (e.g. `0.0322104` alongside
    `32210`) -- both fields real, not one derived client-side by this
    dashboard.
  - `attributes_input_tokens`/`output_tokens`/`cache_read_tokens`/
    `cache_creation_tokens`/`duration_ms` were all populated together on the
    same lines. Unlike Codex's `cached_token_count` (a documented subset of
    input), Anthropic's usage accounting reports cache read/creation as
    independent counters, not a subset of `input_tokens` -- this dashboard
    therefore shows cache read as its own total, not an invented "cache
    share" ratio that would assume a subset relationship never confirmed here.
  - A single `attributes_session_id` carried `hook_execution_start`/
    `.complete`, `tool_result`, `tool_decision`, `api_request`,
    `assistant_response`, `mcp_server_connection`, `plugin_loaded`,
    `retention_sweep` and `user_prompt` together -- confirming hooks/MCP/
    plugins/retention are attributable to the same session/user scoping as
    everything else, not exclusively fleet-wide as the pre-reshape version
    treated them.
  - `attributes_terminal_type` was still only observed as `non-interactive`
    in this check, same open caveat as before -- not newly confirmed.

## Lines of code, active time, commits: fixed 2026-09-14, now shown (Mimir)

The 2026-09-09 investigation found `claude_code.lines_of_code.count` /
`claude_code.active_time.total` / `claude_code.commit.count` /
`claude_code.pull_request.count` absent from Mimir and left them unshown,
reasoning there was "no comparably honest substitute" for a literal line
count. Root cause, found while tracing that gap live: Claude Code's own
documented default for `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE`
is `delta`, and neither this org's `ai-cli-otel` collector nor Alloy's
config runs a `deltatocumulative` processor -- Prometheus/Mimir's data
model has no delta concept, so every `claude_code.*` Sum metric was a
well-formed export that could never become a queryable series. Verified at
every hop with live self-metrics (collector accept==send, Alloy receive/
convert, all zero errors) before concluding it wasn't a pipeline drop.

Fixed in `governance-auth` (lightbridge-governance#335, merged 2026-09-14):
`claude_code_env()` now sets that preference to `cumulative`. Verified live
immediately after: one real CLI session produced
`claude_code_lines_of_code_count_total` (`type=added`/`removed`),
`claude_code_active_time_seconds_total`, `claude_code_commit_count_total`,
`claude_code_pull_request_count_total`, `claude_code_session_count_total`
and `claude_code_cost_usage_USD_total`/`claude_code_token_usage_tokens_total`
in Mimir, all correctly attributed (`session_id`, `user_email`, `model`).

The panels below use the first four -- cost/tokens/sessions already have a
richer, independently-verified Loki equivalent above, so showing the Mimir
duplicate would be redundant, not additive. **Coverage is partial by
design, not a bug**: only a machine running a `governance-auth` build that
includes #335 AND that has re-run `configure` (via `self update` or a fresh
`login`) emits `cumulative` temporality. An unpatched machine's Claude Code
still emits `delta` and still won't appear here -- see the panels' own
"NO DATA" mapping. Do not read a blank panel here as "nobody edited code";
check the machine's `governance-auth` version and re-run `configure` first.

## What is still NOT confirmed

  - The Loki datasource UID -- same `__DS_LOKI__` / `grafanaDashboard.
    datasources.lokiUid: loki` gap the other four generators already
    document. The new Mimir panels carry the identical gap for
    `__DS_PROMETHEUS__` / `grafanaDashboard.datasources.prometheusUid`,
    already substituted the same way for `generate_vscode_copilot_dashboard.py`.
  - Proposed/accepted/retained code changes and human edit-acceptance rate.
    `tool_decision` measures a permission grant, not human review of the
    diff; see the auto-approval split below. `lines_of_code.count` counts
    lines Claude Code wrote via Edit/Write/NotebookEdit, not lines a human
    reviewed, kept, or later reverted -- added is not accepted.

## The auto-approval finding, still live and now range-scoped

`tool_decision`'s `attributes_source` distinguishes a sandbox/permission-mode
default (`config`) from an actual person (`user_temporary`/`user`). The
2026-09-09 investigation found 702/706 decisions in a 24h fleet window were
`accept`/`config`, and 12,585/12,586 code-editing decisions (`Edit`/`Write`/
`NotebookEdit`) over 7d were `accept`/`config` with zero `source=user`
observed in that window. The "Code edits · source=config share" panel below
carries the same split, now over the dashboard's selected range instead of a
fixed 7d -- a near-100% figure here still means "policy default", not
"engineers love what the agent writes".

Usage:
    python3 scripts/generate_claude_code_dashboard.py           # regenerate the file
    python3 scripts/generate_claude_code_dashboard.py --check   # verify it's current
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from dashboard_common import dashboard_shell

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts" / "lightbridge-governance" / "dashboards" / "claude-code-telemetry.json"

LOKI_TYPE = "loki"
LOKI_UID = "__DS_LOKI__"
LOKI_DS = {"type": LOKI_TYPE, "uid": LOKI_UID}
PROM_DS = {"type": "prometheus", "uid": "__DS_PROMETHEUS__"}
UID = "governance-claude-code-telemetry"

# Same user/session textbox filters as the Loki queries below, translated to
# PromQL label matchers -- the OTel-to-Prometheus bridge carries `user.email`/
# `session.id` resource attributes through as `user_email`/`session_id`
# labels verbatim (confirmed live, see the module docstring's 2026-09-14
# section). Kept as a plain string, not an f-string: PromQL's own `{}` label
# syntax and Python's f-string brace-escaping don't mix cleanly.
#
# Backtick-quoted (raw), NOT double-quoted -- caught in review (#336):
# Grafana's `:regex` format escapes regex metacharacters, so a `.` in an
# email becomes `\.` in the substituted value. Inside a PromQL
# DOUBLE-quoted string that backslash is interpreted as an escape sequence
# (same string-literal grammar `logs()` below already routes around with
# backticks for LogQL); a backtick string is raw, no escape processing, so
# the same `\.` a `.`-bearing user_email search always produces survives
# intact. Every panel using this filter broke the moment the User email
# textbox held anything with a dot -- which is every real email -- until
# this fix; the PR's own live verification never exercised that path
# (fleet-wide queries have nothing to escape).
PROM_FILTER = 'user_email=~`.*${user:regex}.*`, session_id=~`.*${session:regex}.*`'

# Every job label value this daemon's Claude Code traffic has been confirmed
# under across this epic's history -- matched together since they all carry
# the identical event shape (`generate_ai_cli_dashboard.py`'s own
# CLAUDE_CODE_JOB constant covered the first two; `ai-cli/claude-code`
# without the `-desktop` suffix is a third, confirmed-live value).
CLAUDE_JOB = '{job=~"claude-code-desktop|ai-cli/claude-code|ai-cli/claude-code-desktop"}'

NO_DATA_MAPPING = {
    "type": "special",
    "options": {"match": "null+nan", "result": {"text": "NO DATA", "color": "red", "index": 0}},
}


def logs(event: str = "", *, scoped: bool = True) -> str:
    """Base query: the Claude Code job, JSON-parsed, optionally restricted to
    one `event.name` and to the selected user/session textbox filters.

    Textbox variables are escaped as regex literals by Grafana; an empty
    search matches everyone, including an absent email. Session drilldown
    links populate `session` the same way. Same mechanism as
    `generate_codex_dashboard.py`'s own `logs()`.
    """
    q = CLAUDE_JOB
    if event:
        q += " |= " + json.dumps(event)
    q += ' | json | __error__=""'
    if event:
        q += " | attributes_event_name=" + json.dumps(event)
    if scoped:
        q += " | attributes_user_email=~`.*${user:regex}.*` | attributes_session_id=~`.*${session:regex}.*`"
    return q


def meaningful(*, scoped: bool = True) -> str:
    """Prompts, priced model calls and tool results -- excludes hook/MCP/
    plugin/retention housekeeping and the rare `api_error`/`compaction`/
    `subagent_completed` diagnostics from the *activity trend and session
    identification*. Those events still get their own scoped panels below;
    this is specifically what counts as "activity" for session detection."""
    return logs(scoped=scoped) + ' | attributes_event_name=~"user_prompt|api_request|tool_result"'


def count(q: str, window: str = "$__range", group: str = "") -> str:
    by = f" by ({group})" if group else ""
    return f"sum{by}(count_over_time({q}[{window}]))"


def total(field: str, *, group: str = "", event: str = "api_request") -> str:
    by = f" by ({group})" if group else ""
    q = logs(event) + (' | attributes_session_id!=""' if group == "attributes_session_id" else "")
    return f'sum{by}(sum_over_time({q} | attributes_{field}!="" | unwrap attributes_{field} | __error__=""[$__range]))'


def latency(percentile: float, window: str = "$__range", group: str = "") -> str:
    q = logs("api_request")
    if group:
        q += ' | attributes_session_id!=""'
    by = f"({group})" if group else "()"
    return (
        f"quantile_over_time({percentile}, {q} | attributes_duration_ms!=\"\" "
        f'| unwrap attributes_duration_ms | __error__=""[{window}]) by {by}'
    )


def metric_increase(name: str, *, extra: str = "", group: str = "", window: str = "$__range") -> str:
    """`sum[ by (group)](increase(name{user/session filter[, extra]}[window]))`
    -- the PromQL analogue of this file's own `total()`/`count()` for the
    native OTLP metrics in Mimir. `increase()` over a Sum/counter is the
    correct idiom for "how much did this grow in the selected period",
    matching `total()`'s own `sum_over_time` role for Loki-side unwrapped
    fields. `extra` adds one more label matcher (e.g. `type="added"`) for a
    single stat panel; `group` groups a trend panel by a label instead."""
    by = f" by ({group})" if group else ""
    filters = PROM_FILTER + (f", {extra}" if extra else "")
    return "sum" + by + "(increase(" + name + "{" + filters + "}[" + window + "]))"


def prom_stat_panel(
    ids: Ids, *, title: str, description: str, expr: str, unit: str, grid: dict[str, int],
    decimals: int = 0,
) -> dict[str, Any]:
    """Same shape and `NO_DATA_MAPPING` convention as `loki_stat_panel` --
    see that function's own docstring for why an instant query with no
    sparkline is used, not a range query.

    `decimals` defaults to 0: `increase()` over a counter yields a
    fractional result (Prometheus extrapolates to the range boundary, it
    does not do an exact integer reconciliation), so an un-rounded "Commits"
    or "Pull requests" stat can render `1.14` -- caught in review (#336).
    Every current caller is a count or a duration in whole seconds, so 0 is
    the right default everywhere this is used today, not just a fallback."""
    field_config = _base_field_config(unit=unit, mappings=[NO_DATA_MAPPING])
    field_config["defaults"]["decimals"] = decimals
    return {
        "id": ids.take(), "type": "stat", "title": title, "description": description,
        "datasource": PROM_DS, "gridPos": grid,
        "fieldConfig": field_config,
        "options": {
            "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
            "orientation": "auto", "textMode": "auto", "colorMode": "none", "graphMode": "none",
            "justifyMode": "auto",
        },
        "targets": [{"datasource": PROM_DS, "expr": expr, "instant": True, "range": False,
                     "legendFormat": "__auto", "refId": "A"}],
    }


def prom_timeseries_panel(
    ids: Ids, *, title: str, description: str, expr: str, legend: str, unit: str, grid: dict[str, int],
) -> dict[str, Any]:
    return {
        "id": ids.take(), "type": "timeseries", "title": title, "description": description,
        "datasource": PROM_DS, "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit),
        "options": {
            "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "sum"]},
            "tooltip": {"mode": "multi"},
        },
        "targets": [{"datasource": PROM_DS, "expr": expr, "instant": False, "range": True,
                     "legendFormat": legend, "refId": "A"}],
    }


class Ids:
    """Deterministic, sequential Grafana panel ids -- see
    generate_dashboards.py's own `Ids` for the full rationale. Duplicated
    rather than imported, same reasoning as the other four generators."""

    def __init__(self) -> None:
        self._next = 1

    def take(self) -> int:
        value = self._next
        self._next += 1
        return value


def _base_field_config(
    *, unit: str, mappings: list[dict[str, Any]] | None = None, stacking: bool = False,
) -> dict[str, Any]:
    defaults: dict[str, Any] = {"unit": unit}
    if mappings:
        defaults["mappings"] = mappings
    defaults["thresholds"] = {"mode": "absolute", "steps": [{"color": "green", "value": None}]}
    if stacking:
        defaults["custom"] = {"stacking": {"mode": "normal"}}
    return {"defaults": defaults, "overrides": []}


def loki_timeseries_panel(
    ids: Ids, *, title: str, description: str, expr: str, legend: str, unit: str,
    grid: dict[str, int], mappings: list[dict[str, Any]] | None = None, stacking: bool = False,
) -> dict[str, Any]:
    return {
        "id": ids.take(), "type": "timeseries", "title": title, "description": description,
        "datasource": LOKI_DS, "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit, mappings=mappings, stacking=stacking),
        "options": {
            "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "sum"]},
            "tooltip": {"mode": "multi"},
        },
        "targets": [{"datasource": LOKI_DS, "expr": expr, "queryType": "range", "legendFormat": legend, "refId": "A"}],
    }


def loki_stat_panel(
    ids: Ids, *, title: str, description: str, expr: str, unit: str, grid: dict[str, int],
    mappings: list[dict[str, Any]] | None = None, reduce_calc: str = "lastNotNull",
    thresholds_steps: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Return a single snapshot, matching the #318 Loki OOM fix in the other
    four generators. Each expression embeds the selected window (`[$__range]`
    or a fixed diagnostic one); a range query would needlessly recompute that
    whole window at every dashboard step. With no sparkline (`graphMode:
    none`), an instant query supplies the same final value without that
    amplification. Keep `lastNotNull` as the reducer: summing already-
    aggregated samples would multiply the value if a range query were ever
    reintroduced."""
    field_config = _base_field_config(unit=unit, mappings=mappings)
    if thresholds_steps:
        field_config["defaults"]["thresholds"] = {"mode": "absolute", "steps": thresholds_steps}
    return {
        "id": ids.take(), "type": "stat", "title": title, "description": description,
        "datasource": LOKI_DS, "gridPos": grid, "fieldConfig": field_config,
        "options": {
            "reduceOptions": {"calcs": [reduce_calc], "fields": "", "values": False},
            "orientation": "auto", "textMode": "auto", "colorMode": "none", "graphMode": "none",
            "justifyMode": "auto",
        },
        "targets": [{"datasource": LOKI_DS, "expr": expr, "queryType": "instant", "instant": True,
                     "legendFormat": "__auto", "refId": "A"}],
    }


def loki_table_panel(
    ids: Ids, *, title: str, description: str, expr: str, grid: dict[str, int], unit: str = "short",
    rename: dict[str, str] | None = None,
) -> dict[str, Any]:
    """An instant metric query rendered as a table -- a snapshot ranking or
    breakdown rather than a trend over time. Same shape as the other four
    generators' own `loki_table_panel`."""
    exclude = {"Time": True}
    return {
        "id": ids.take(), "type": "table", "title": title, "description": description,
        "datasource": LOKI_DS, "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": unit}, "overrides": []},
        "options": {"showHeader": True, "cellHeight": "sm"},
        "targets": [{"datasource": LOKI_DS, "expr": expr, "queryType": "instant", "instant": True,
                     "format": "table", "refId": "A"}],
        "transformations": [{"id": "organize", "options": {"excludeByName": exclude, "renameByName": rename or {}}}],
    }


def loki_logs_panel(ids: Ids, *, title: str, description: str, expr: str, grid: dict[str, int]) -> dict[str, Any]:
    """A native Grafana Loki "Logs" panel -- raw matching log lines, not an
    aggregation. Used once, for "recent errors", where seeing the actual
    lines is the point rather than a count."""
    return {
        "id": ids.take(), "type": "logs", "title": title, "description": description,
        "datasource": LOKI_DS, "gridPos": grid,
        "options": {"showTime": True, "showLabels": False, "wrapLogMessage": True,
                    "sortOrder": "Descending", "enableLogDetails": True},
        "targets": [{"datasource": LOKI_DS, "expr": expr, "queryType": "range", "refId": "A"}],
    }


def target(expr: str, ref: str = "A", legend: str = "__auto", *, instant: bool = True) -> dict[str, Any]:
    result = {"datasource": LOKI_DS, "expr": expr, "refId": ref, "legendFormat": legend,
              "queryType": "instant" if instant else "range"}
    if instant:
        result["instant"] = True
    return result


def piechart_panel(ids: Ids, *, title: str, description: str, expr: str, legend: str, grid: dict[str, int]) -> dict[str, Any]:
    """Model-share donut -- one instant query, rows turned into named slices
    via `rowsToFields` (Grafana can't otherwise turn "one row per model" into
    separate pie slices without a field per model). Same transform Codex's
    generator uses for the identical problem."""
    return {
        "id": ids.take(), "type": "piechart", "title": title, "description": description,
        "datasource": LOKI_DS, "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": "short", "color": {"mode": "palette-classic"}}, "overrides": []},
        "options": {"pieType": "donut", "displayLabels": [], "tooltip": {"mode": "single"},
                    "legend": {"displayMode": "table", "placement": "bottom", "values": ["value", "percent"]},
                    "reduceOptions": {"values": False, "calcs": ["lastNotNull"], "fields": ""}},
        "transformations": [
            {"id": "filterFieldsByName", "options": {"include": {"names": ["attributes_model", "Value #A"]}}},
            {"id": "rowsToFields", "options": {"mappings": [
                {"fieldName": "attributes_model", "handlerKey": "field.name"},
                {"fieldName": "Value #A", "handlerKey": "field.value"}]}},
        ],
        "targets": [target(expr, legend=legend)],
    }


def build_dashboard() -> dict[str, Any]:
    ids = Ids()
    panels: list[dict[str, Any]] = []

    panels.append({
        "id": ids.take(), "type": "text", "title": "Your Claude Code activity",
        "gridPos": {"x": 0, "y": 0, "w": 24, "h": 3},
        "options": {"mode": "markdown", "content": (
            "Search by email or click a session. Clear searches for everyone. Session times are "
            "observed bounds, not lifecycle timestamps; identity coverage is fleet-wide by design. "
            "Cost is Claude Code's own reported figure -- a real number, not a governance estimate -- "
            "but it is not necessarily the same as an invoice; no admin billing connector is wired "
            "here. Tool-decision figures are dominated by a sandbox/permission-mode default, not a "
            "human clicking approve; see the source split below. Code acceptance/retention (did a "
            "human keep the diff) is not measured -- lines-of-code below counts lines Claude Code "
            "wrote, not lines a person reviewed and kept. Lines of code, active time, commits and "
            "pull requests come from Mimir, not Loki, and only appear for a machine whose "
            "governance-auth has the 2026-09-14 metrics-temporality fix AND has re-run configure -- "
            "see this generator's own docstring; a blank panel there is a coverage gap, not zero."
        )},
    })

    # ---------------------------------------------------------------
    # Section 1 -- summary stats, one glance, every one scoped to the
    # selected user/session/range except the explicitly fleet-wide coverage
    # indicator. Mirrors Codex's 8-stat layout and grid.
    # ---------------------------------------------------------------
    sessions_expr = "count(" + count(meaningful() + ' | attributes_session_id!=""', group="attributes_session_id") + ")"
    fleet = logs(scoped=False) + ' | attributes_event_name=~"user_prompt|api_request|tool_result"'
    attributed = count(fleet + ' | attributes_user_email!=""')
    coverage = f"100 * ({attributed} or vector(0)) / {count(fleet)}"
    stats = [
        ("Active sessions", sessions_expr, "short",
         "Distinct session IDs with a prompt, priced model call or tool result in the selected "
         "period; not session launches."),
        ("Prompts", count(logs("user_prompt")), "short",
         "Observed user_prompt events; may include system-generated follow-ups, not only human-typed messages."),
        ("Total cost", total("cost_usd"), "currencyUSD",
         "Sum of Claude Code's own reported attributes_cost_usd on api_request lines. A real "
         "source-reported figure, not a governance estimate -- see Codex's dashboard for the "
         "estimate/actual distinction that does NOT apply here."),
        ("Input tokens", total("input_tokens"), "short", "Reported input tokens on api_request lines."),
        ("Output tokens", total("output_tokens"), "short", "Reported output tokens on api_request lines."),
        ("Cache read tokens", total("cache_read_tokens"), "short",
         "Reported cache-read tokens on api_request lines. Anthropic reports this as an independent "
         "counter, not a confirmed subset of input tokens -- shown as its own total, not a ratio."),
        ("Request wait · p95", latency(0.95), "ms",
         "95th percentile of attributes_duration_ms on api_request lines. A different population "
         "from human waiting time; a request can be one of several per visible turn."),
        ("Identity coverage · fleet", coverage, "percent",
         "Share of meaningful events fleet-wide with a nonempty user email. Ignores user/session "
         "searches by design. One measured event does not establish complete session coverage."),
    ]
    for n, (title, expr, unit, description) in enumerate(stats):
        p = loki_stat_panel(ids, title=title, description=description, expr=expr, unit=unit,
            grid={"x": n % 4 * 6, "y": 3 + n // 4 * 4, "w": 6, "h": 4}, mappings=[NO_DATA_MAPPING])
        p["fieldConfig"]["defaults"]["decimals"] = 1 if unit in ("ms", "percent") else (2 if unit == "currencyUSD" else 0)
        panels.append(p)

    # ---------------------------------------------------------------
    # Section 2 -- activity trend and model share.
    # ---------------------------------------------------------------
    activity = loki_timeseries_panel(ids, title="Meaningful activity", description=(
        "Events per rolling interval, not events per second. Prompts, priced model calls "
        "(api_request) and tool results; hook/MCP/plugin/retention housekeeping and the rare "
        "api_error/compaction/subagent_completed diagnostics are excluded from this trend, though "
        "each still has its own scoped panel below."), expr="", legend="", unit="short",
        grid={"x": 0, "y": 11, "w": 12, "h": 8})
    activity["targets"] = [target(count(q, "$__interval"), ref, name, instant=False) for ref, name, q in [
        ("A", "Prompts", logs("user_prompt")),
        ("B", "Model calls", logs("api_request")),
        ("C", "Tool calls", logs("tool_result"))]]
    activity["interval"] = "5m"
    activity["options"]["legend"]["calcs"] = []
    panels.append(activity)

    panels.append(piechart_panel(ids, title="Models · responses",
        description="Share of api_request calls by attributes_model.",
        expr=count(logs("api_request"), group="attributes_model"), legend="{{attributes_model}}",
        grid={"x": 12, "y": 11, "w": 6, "h": 8}))
    panels.append(piechart_panel(ids, title="Models · tokens",
        description="Share of input plus output tokens by model. Cache read/creation tokens are "
        "not added -- see the top-row caveat on why they are shown as independent totals.",
        expr=total("input_tokens", group="attributes_model") + " + " + total("output_tokens", group="attributes_model"),
        legend="{{attributes_model}}", grid={"x": 18, "y": 11, "w": 6, "h": 8}))

    # ---------------------------------------------------------------
    # Section 3 -- sessions table. LogQL has no cross-stream join; each
    # column is an independent instant query joined client-side by
    # attributes_session_id (Grafana's joinByField transform), same
    # mechanism as Codex's own session table.
    # ---------------------------------------------------------------
    observed = meaningful() + ' | attributes_session_id!="" | label_format observed_ms=`{{ unixEpochMillis (__timestamp__) }}`'

    def stamp(op: str) -> str:
        return f'{op}_over_time({observed} | unwrap observed_ms | __error__=""[$__range]) by (attributes_session_id)'

    session_queries = [
        ("A", "Observed start", stamp("min"), "dateTimeAsIso"),
        ("B", "Last activity", stamp("max"), "dateTimeAsIso"),
        ("C", "Prompts", count(logs("user_prompt") + ' | attributes_session_id!=""', group="attributes_session_id"), "short"),
        ("D", "Model calls", count(logs("api_request") + ' | attributes_session_id!=""', group="attributes_session_id"), "short"),
        ("E", "Tool calls", count(logs("tool_result") + ' | attributes_session_id!=""', group="attributes_session_id"), "short"),
        ("F", "Cost", total("cost_usd", group="attributes_session_id"), "currencyUSD"),
    ]
    table = loki_table_panel(ids, title="Sessions", description=(
        "One row per session. Observed start and last activity are bounded by the selected period, "
        "not lifecycle timestamps -- not human active time. Blank cells mean no matching signal, "
        "never zero. Click a session to filter this dashboard."),
        expr="", grid={"x": 0, "y": 19, "w": 24, "h": 9})
    table["targets"] = [dict(target(expr, ref), format="table") for ref, _, expr, _ in session_queries]
    table["transformations"] = [
        {"id": "joinByField", "options": {"byField": "attributes_session_id", "mode": "outerTabular"}},
        {"id": "organize", "options": {
            "excludeByName": {name: True for name in ["Time"] + [f"Time {n}" for n in range(1, 7)]},
            "renameByName": {"attributes_session_id": "Session",
                              **{f"Value #{ref}": name for ref, name, _, _ in session_queries}}}},
        {"id": "sortBy", "options": {"sort": [{"field": "Last activity", "desc": True}]}},
    ]
    table["fieldConfig"]["defaults"].update(noValue="—", custom={"width": 100})
    table["fieldConfig"]["overrides"] = [
        {"matcher": {"id": "byName", "options": name}, "properties": [
            {"id": "unit", "value": unit},
            {"id": "custom.width", "value": 170 if unit == "dateTimeAsIso" else 100}]}
        for _, name, _, unit in session_queries] + [{"matcher": {"id": "byName", "options": "Session"}, "properties": [
            {"id": "custom.width", "value": 300},
            {"id": "links", "value": [{"title": "Open session", "url":
                "/d/" + UID + "?${__url_time_range}&var-user=${user:percentencode}&var-session=${__value.raw:percentencode}",
                "targetBlank": False}]}]}]
    panels.append(table)

    # ---------------------------------------------------------------
    # Section 4 -- tools, approvals, invocation source. See the module
    # docstring's auto-approval section: `source` is the load-bearing split.
    # ---------------------------------------------------------------
    decisions = loki_timeseries_panel(ids, title="Tool decisions", description=(
        "By decision AND source -- source is load-bearing: `config` is a sandbox/permission-mode "
        "default, NOT an engineer's decision. See this generator's own docstring for the confirmed "
        "24h/7d fleet figures that motivate the split."),
        expr=f"sum by (attributes_decision, attributes_source) (count_over_time({logs('tool_decision')}[$__interval]))",
        legend="{{attributes_decision}} / {{attributes_source}}", unit="ops", grid={"x": 0, "y": 28, "w": 12, "h": 8})
    panels.append(decisions)

    success = loki_timeseries_panel(ids, title="Tool success rate", description=(
        "By attributes_success on tool_result -- a signal Codex's own telemetry does not carry at all."),
        expr=f"sum by (attributes_success) (count_over_time({logs('tool_result')}[$__interval]))",
        legend="success={{attributes_success}}", unit="ops", grid={"x": 12, "y": 28, "w": 12, "h": 8})
    panels.append(success)

    edit_tools = logs("tool_decision") + ' | attributes_tool_name=~"Edit|Write|NotebookEdit"'
    edit_share = (
        f"sum(count_over_time({edit_tools} | attributes_source=\"config\" [$__range])) "
        f"/ sum(count_over_time({edit_tools}[$__range]))"
    )
    edit_stat = loki_stat_panel(ids, title="Code edits · source=config share", description=(
        "(count with source=config) / (count total), both restricted to tool_decision on "
        "code-editing tools (Edit/Write/NotebookEdit) over the selected range. The closest analogue "
        "to Grafana's own 25052 dashboard's code_edit_tool.decision-based accept rate, split by "
        "source so a near-100% figure reads as \"policy default\", not \"engineers love what the "
        "agent writes\"."), expr=edit_share, unit="percentunit",
        grid={"x": 0, "y": 36, "w": 8, "h": 6}, mappings=[NO_DATA_MAPPING])
    panels.append(edit_stat)

    invocation = loki_table_panel(ids, title="Invocation source", description=(
        "By attributes_query_source on assistant_response -- independent of app.entrypoint. "
        "Confirmed live values include sdk, prompt_suggestion, agent:builtin:general-purpose, "
        "web_fetch_apply, web_search_tool, compact. A high `sdk` share means most matching traffic "
        "is Agent-SDK-driven, not a human typing at an interactive prompt."),
        expr=f"sum by (attributes_query_source) (count_over_time({logs('assistant_response')}[$__range]))",
        grid={"x": 8, "y": 36, "w": 16, "h": 6},
        rename={"attributes_query_source": "Invocation source", "Value": "Count"})
    panels.append(invocation)

    # ---------------------------------------------------------------
    # Section 5 -- waiting time and errors.
    # ---------------------------------------------------------------
    waiting = loki_timeseries_panel(ids, title="Request waiting time", description=(
        "Median/p95 attributes_duration_ms on api_request. A different population from human "
        "waiting time or turn-level latency; never sum percentiles."), expr="", legend="", unit="ms",
        grid={"x": 0, "y": 42, "w": 24, "h": 7})
    waiting["targets"] = [target(latency(q, "$__interval"), ref, name, instant=False)
                          for ref, name, q in [("A", "Median", 0.5), ("B", "p95", 0.95)]]
    waiting["interval"] = "5m"
    waiting["options"]["legend"]["calcs"] = []
    panels.append(waiting)

    errors = loki_timeseries_panel(ids, title="API errors", description="count_over_time on api_error.",
        expr=count(logs("api_error"), "$__interval"), legend="api_error", unit="ops",
        grid={"x": 0, "y": 49, "w": 12, "h": 7})
    panels.append(errors)

    error_logs_expr = (
        logs(scoped=True) + ' | attributes_event_name=~"api_error|tool_result"'
        ' | attributes_success="false" or attributes_event_name="api_error"'
    )
    panels.append(loki_logs_panel(ids, title="Error details", description=(
        'A native Loki "Logs" panel -- the raw api_error/tool_result(success=false) lines '
        "themselves, not an aggregate. Prompt content is never captured here (redacted at the source)."),
        expr=error_logs_expr, grid={"x": 12, "y": 49, "w": 12, "h": 7}))

    # ---------------------------------------------------------------
    # Section 6 -- hooks, MCP servers, plugins, retention. No analogue in
    # 25052; the differentiator this dashboard has always had, now scoped
    # to the selected user/session/range like everything else above.
    # ---------------------------------------------------------------
    hook_complete = logs("hook_execution_complete")
    hooks_ts = loki_timeseries_panel(ids, title="Hook outcomes", description=(
        "success/blocking/cancelled/non-blocking-error counts from hook_execution_complete."),
        expr="", legend="", unit="none", grid={"x": 0, "y": 56, "w": 12, "h": 8}, stacking=True)
    hooks_ts["targets"] = [target(f"sum(sum_over_time({hook_complete} | attributes_num_{field}!=\"\" "
                                   f'| unwrap attributes_num_{field} | __error__=""[$__interval]))', ref, name, instant=False)
                           for ref, name, field in [
                               ("A", "success", "success"), ("B", "blocking", "blocking"),
                               ("C", "cancelled", "cancelled"), ("D", "non-blocking error", "non_blocking_error")]]
    panels.append(hooks_ts)
    panels.append(loki_table_panel(ids, title="Top hooks", description=(
        "topk(10, ...) by attributes_hook_name over the selected range."),
        expr=f"topk(10, sum by (attributes_hook_name) (count_over_time({hook_complete}[$__range])))",
        grid={"x": 12, "y": 56, "w": 12, "h": 8},
        rename={"attributes_hook_name": "Hook", "Value": "Invocations"}))

    mcp = logs("mcp_server_connection")
    panels.append(loki_table_panel(ids, title="MCP connections · status", description=(
        "By attributes_status on mcp_server_connection. Confirmed live values: connected, disconnected."),
        expr=f"sum by (attributes_status) (count_over_time({mcp}[$__range]))",
        grid={"x": 0, "y": 64, "w": 8, "h": 8}, rename={"attributes_status": "Status", "Value": "Count"}))
    panels.append(loki_table_panel(ids, title="MCP connections · transport", description=(
        "By attributes_transport_type on mcp_server_connection. Confirmed live values: stdio, http."),
        expr=f"sum by (attributes_transport_type) (count_over_time({mcp}[$__range]))",
        grid={"x": 8, "y": 64, "w": 8, "h": 8}, rename={"attributes_transport_type": "Transport", "Value": "Count"}))
    panels.append(loki_table_panel(ids, title="Plugins loaded", description=(
        "By attributes_plugin_name and attributes_marketplace_name on plugin_loaded."),
        expr=f"sum by (attributes_plugin_name, attributes_marketplace_name) (count_over_time({logs('plugin_loaded')}[$__range]))",
        grid={"x": 16, "y": 64, "w": 8, "h": 8},
        rename={"attributes_plugin_name": "Plugin", "attributes_marketplace_name": "Marketplace", "Value": "Count"}))

    retention_stats = [
        ("Retention sweeps", count(logs("retention_sweep")), "short"),
        ("Transcripts deleted", total("transcripts_deleted", event="retention_sweep"), "short"),
        ("Session files deleted", total("session_files_deleted", event="retention_sweep"), "short"),
        ("History entries pruned", total("history_entries_pruned", event="retention_sweep"), "short"),
    ]
    for n, (title, expr, unit) in enumerate(retention_stats):
        panels.append(loki_stat_panel(ids, title=title,
            description=f"From retention_sweep, over the selected range.", expr=expr, unit=unit,
            grid={"x": n * 6, "y": 72, "w": 6, "h": 6}, mappings=[NO_DATA_MAPPING]))

    # ---------------------------------------------------------------
    # Section 7 -- Lines of code, active time, commits, pull requests.
    # Native OTLP METRICS (Mimir/Prometheus), not Loki -- the one thing
    # 25052 (Grafana's own official dashboard) has that no earlier version
    # of this dashboard could show. See the module docstring's own
    # "Lines of code, active time, commits" section for the root cause
    # (delta vs cumulative temporality) and the fix (lightbridge-
    # governance#335). NO_DATA here is a coverage gap (an un-updated
    # machine), never a false zero -- these panels never invent one.
    # ---------------------------------------------------------------
    coverage_note = (
        " Only a machine running a governance-auth build with the "
        "2026-09-14 metrics-temporality fix, that has re-run configure, "
        "emits this -- NO DATA means an un-updated machine, not zero "
        "activity."
    )
    panels.append(prom_stat_panel(ids, title="Lines added", unit="short",
        description="sum(increase(claude_code_lines_of_code_count_total{type=\"added\", "
        "user/session filter}[$__range])). Lines Claude Code wrote via Edit/Write/NotebookEdit -- "
        "not lines a human reviewed and kept." + coverage_note,
        expr=metric_increase("claude_code_lines_of_code_count_total", extra='type="added"'),
        grid={"x": 0, "y": 78, "w": 6, "h": 6}))
    panels.append(prom_stat_panel(ids, title="Lines removed", unit="short",
        description="Same query, type=\"removed\"." + coverage_note,
        expr=metric_increase("claude_code_lines_of_code_count_total", extra='type="removed"'),
        grid={"x": 6, "y": 78, "w": 6, "h": 6}))
    panels.append(prom_stat_panel(ids, title="Active time", unit="s",
        description="sum(increase(claude_code_active_time_seconds_total{...}[$__range])), both "
        "type=user (keyboard) and type=cli (tool execution/AI responses) combined -- this stat "
        "does not split by type; query by the `type` label directly (e.g. in Explore) if the "
        "split matters." + coverage_note,
        expr=metric_increase("claude_code_active_time_seconds_total"),
        grid={"x": 12, "y": 78, "w": 6, "h": 6}))
    panels.append(prom_stat_panel(ids, title="Commits", unit="short",
        description="sum(increase(claude_code_commit_count_total{...}[$__range])) -- commits made "
        "through Claude Code specifically, not every commit in a touched repository." + coverage_note,
        expr=metric_increase("claude_code_commit_count_total"),
        grid={"x": 18, "y": 78, "w": 6, "h": 6}))

    panels.append(prom_stat_panel(ids, title="Pull requests", unit="short",
        description="sum(increase(claude_code_pull_request_count_total{...}[$__range])) -- created "
        "via shell command or an MCP tool." + coverage_note,
        expr=metric_increase("claude_code_pull_request_count_total"),
        grid={"x": 0, "y": 84, "w": 6, "h": 8}))
    lines_trend = prom_timeseries_panel(ids, title="Lines of code over time", unit="short", legend="",
        description="Added vs removed, per $__interval -- the trend 25052 has that this dashboard "
        "could not show before the 2026-09-14 temporality fix." + coverage_note,
        expr="", grid={"x": 6, "y": 84, "w": 18, "h": 8})
    lines_trend["targets"] = [
        {"datasource": PROM_DS, "expr": metric_increase("claude_code_lines_of_code_count_total",
            extra='type="added"', window="$__interval"), "instant": False, "range": True,
            "legendFormat": "added", "refId": "A"},
        {"datasource": PROM_DS, "expr": metric_increase("claude_code_lines_of_code_count_total",
            extra='type="removed"', window="$__interval"), "instant": False, "range": True,
            "legendFormat": "removed", "refId": "B"},
    ]
    panels.append(lines_trend)

    d = dashboard_shell(UID, "Claude Code telemetry", (
        "User and session activity, model usage, tool/approval behaviour, waiting time, and "
        "hooks/MCP/plugin/retention hygiene for Claude Code, sourced from forwarded OTLP logs in "
        "Loki, plus lines of code/active time/commits/pull requests from native OTLP metrics in "
        "Mimir (fixed 2026-09-14, lightbridge-governance#335 -- coverage depends on the machine's "
        "governance-auth version, see the generator's own docstring). Reshaped 2026-09-14 to match "
        "the Codex user/session dashboard's filter and range contract. Generated by "
        "scripts/generate_claude_code_dashboard.py -- do not hand-edit; regenerate instead. See "
        "docs/integrations/claude-code-dashboard.md."), panels)
    d["tags"].append("claude-code")
    d["time"] = {"from": "now-24h", "to": "now"}
    d["templating"]["list"] = [{"name": name, "label": label, "type": "textbox", "query": "",
        "current": {"text": "", "value": ""}, "options": [{"text": "", "value": "", "selected": True}],
        "hide": 0, "skipUrlSync": False, "description": description} for name, label, description in [
            ("user", "User email", "Literal email search; blank includes everyone, including unattributed traffic."),
            ("session", "Session", "Literal session ID search; blank includes all sessions. Click a session row to select it.")]]
    d["links"].append({"type": "link", "title": "Clear user & session", "url": "/d/" + UID + "?var-user=&var-session=",
                        "keepTime": True, "includeVars": False, "targetBlank": False})
    return d


def render(dashboard: dict[str, Any]) -> str:
    return json.dumps(dashboard, indent=2, sort_keys=False) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="regenerate in-memory and exit non-zero if the committed file is out of date, "
        "without writing anything",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=OUTPUT_PATH,
        help=f"output path (default: {OUTPUT_PATH.relative_to(REPO_ROOT)})",
    )
    args = parser.parse_args(argv)

    generated = render(build_dashboard())
    json.loads(generated)  # fail loudly on malformed output, not silently

    if args.check:
        if not args.output.exists():
            print(f"{args.output}: does not exist -- run without --check to generate it", file=sys.stderr)
            return 1
        current = args.output.read_text()
        if current != generated:
            print(f"{args.output}: out of date -- run `python3 {Path(__file__).name}` to regenerate", file=sys.stderr)
            return 1
        print(f"{args.output}: up to date")
        return 0

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(generated)
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
