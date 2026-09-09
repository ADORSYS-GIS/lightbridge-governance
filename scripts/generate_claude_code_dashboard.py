#!/usr/bin/env python3
"""Generate the Claude Code telemetry Grafana dashboard JSON.

DEV-TIME TOOL ONLY, same convention as the other three dashboard generators
in this directory (see `generate_dashboards.py`'s own docstring for the full
rationale). Standard library only. The **generated JSON is committed** at
`charts/lightbridge-governance/dashboards/claude-code-telemetry.json`; that
file, not this script, is what `helm template`/the Grafana Operator actually
reads.

Determinism contract: identical to the other three generators -- no
timestamps, no `random`, no `uuid`, no hash-seed-dependent ordering, panel
ids from a plain counter over a fixed call sequence.

## Why this dashboard exists, given `ai-cli-telemetry.json` already has a
## Claude Code section

`generate_ai_cli_dashboard.py` built a 3-event-type Claude Code section
(`api_request`/`tool_decision`, cost/tokens/tools) when this epic's daemon
work first shipped. Benchmarked directly against Grafana's own official
integration dashboard for this client (grafana.com/grafana/dashboards/25052,
"Claude Code", downloaded and inspected panel-by-panel) at the user's
request, that section is a fraction of what's actually available: Claude
Code emits at least 13 distinct event types, not 2, and this dashboard's own
live investigation (below) found real signal in nine of them the existing
section never touches -- hooks, MCP servers, plugins, data-retention
housekeeping, and a genuine tool-success/failure field 25052 itself doesn't
have. `ai-cli-telemetry.json` is left as-is (still the only place Copilot
Chat's Mimir metrics live); this is a dedicated, deeper replacement for its
Claude Code section, following the same per-client split already applied to
OpenCode and Codex.

## Confirmed live 2026-09-09 (kubectl port-forward svc/loki-gateway, this
## org's real developers' real Claude Code sessions, not documentation)

Same ingestion shape as OpenCode/Codex, despite an early false alarm while
investigating this (worth recording so a future reader doesn't repeat the
detour): Loki's query API groups a log QUERY's results by every label the
query's own parser stage extracted, and returns that combined set under the
response's `"stream"` key -- indistinguishable, at a glance, from real
indexed labels or Loki "structured metadata". A raw `{job=~"..."}` line
(*no* `| json`) confirmed via `/loki/api/v1/series` that the only genuinely
indexed labels are `cluster`/`exporter`/`job`/`service_name`; every
`attributes_*`/`resources_*` field is exactly what it is for OpenCode and
Codex -- extracted by `| json` from a JSON body, nothing more exotic.

  - Body: `{"body": "claude_code.<event>", "attributes": {...}, "resources":
    {...}, "instrumentation_scope": {"name": "com.anthropic.claude_code.events"}}`
    -- one scope, no second junk scope to filter out (unlike OpenCode's
    bare-ACP-log scope).
  - Thirteen confirmed `event.name` values, by 24h volume: `api_request`
    (748), `tool_decision` (702), `tool_result` (700), `hook_execution_start`
    / `hook_execution_complete` (849 each), `assistant_response` (330),
    `mcp_server_connection` (116), `plugin_loaded` (76), `retention_sweep`
    (37), `user_prompt` (35), `hook_registered` (12), `api_error` (1),
    `compaction` (1), `subagent_completed` (1).
  - `app.entrypoint` -- flagged in `generate_ai_cli_dashboard.py`'s own
    docstring as gated behind `OTEL_METRICS_INCLUDE_ENTRYPOINT` and "NOT YET
    POPULATED as of this panel shipping" -- IS populated now (confirmed
    value: `claude-desktop`), and `query_source` (confirmed values: `sdk`,
    `prompt_suggestion`, `agent:builtin:general-purpose`, `web_fetch_apply`,
    `web_search_tool`, `compact`) gives a second, independent "how was this
    invoked" axis that dashboard never used at all.

## ⚠️ The same auto-approval finding as Codex, at even higher confidence

Cross-tabulating `tool_decision`'s `decision` by `source`, live, 24h: 702 of
706 decisions (99.4%) were `accept`/`config` (a sandbox/permission-mode
default, not a person), 2 were `accept`/`user_temporary`, 2 were
`reject`/`config`. Restricted to the code-EDITING tools specifically
(`Edit`/`Write`/`NotebookEdit`, the closest analogue to 25052's
`code_edit_tool.decision` metric) over 7d: 12,585 `accept`/`config` against
1 `reject`/`config` -- **zero** `source=user` decisions in that scope in the
window checked. 25052's own "Code Edit Accept Rate" panel computes
`accepted / total` from the SAME `decision` field with no `source` split at
all -- exactly the metric this dashboard's Codex sibling already flagged as
measuring a sandbox default, not engineer behaviour. Every decision panel
below carries the same split Codex's dashboard does, for the same reason.

## A real tool-success signal Codex's telemetry does NOT have

`tool_result` carries `attributes_success` (confirmed live: `true`/`false`,
663/39 over a real 24h window -- 94.4%) and, on a failure, `error_type`
(confirmed value: `ShellError`). `codex-telemetry.json`'s own docstring had
to note Codex's `tool_result` carries no such field; Claude Code's does, and
this dashboard uses it for a genuine reliability panel neither 25052 nor
this repo's own prior Claude Code section built.

## What this dashboard has that 25052 does NOT

Hooks (`hook_execution_start`/`.complete`, with per-hook outcome counts --
`num_success`/`num_blocking`/`num_cancelled`/`num_non_blocking_error` and
`total_duration_ms`), MCP server connections (`status`, `transport_type`),
plugin/marketplace usage (`plugin_loaded`), and data-retention housekeeping
(`retention_sweep`'s `transcripts_deleted`/`session_files_deleted`/
`history_entries_pruned`/`artifacts_deleted`) -- none of these have any
analogue in the official dashboard's 31 panels, and all are confirmed real,
live signal, not speculative additions.

## What 25052 has that this dashboard does NOT, and why

  - **Lines of code (added/removed) and daily commits.** 25052 reads these
    from `claude_code.lines_of_code.count` / `claude_code.commit.count` --
    real OTLP METRICS, not the log events this dashboard (and every other
    Loki-backed generator in this directory) reads. Checked directly in
    Mimir (`kubectl port-forward svc/mimir-nginx`, `/prometheus/api/v1/label/
    __name__/values`, full 5354-name list grepped): zero `claude_code_*` or
    `claude*` application metrics exist there today -- confirmed absent, the
    same gap `generate_ai_cli_dashboard.py`'s own docstring already flagged
    ("checked directly in Mimir ... and NOT found under any prefix"), now
    reverified rather than assumed stale. Whatever this org's collector
    metrics pipeline does with Claude Code's OTLP metrics, Mimir is not it.
  - **Active time.** Same story -- `claude_code.active_time.total` is a
    metric, confirmed absent from Mimir alongside the two above. No log
    event carries an equivalent duration.
  - Both gaps are stated here, not silently dropped or faked with a proxy --
    unlike the tool-count proxies this dashboard's OpenCode/Codex siblings
    use for "lines of code", there is no comparably honest substitute for a
    literal added/removed line count, so none is offered.

## What is NOT confirmed

  - The Loki datasource UID -- same `__DS_LOKI__` /
    `grafanaDashboard.datasources.lokiUid: loki` gap the other three
    generators already document. Reused verbatim, not re-guessed.
  - `terminal_type` diversity. Every sampled line in this session's checks
    read `non-interactive` -- plausible for a fleet where this daemon setup
    is the norm, but this dashboard's own investigation did not independently
    confirm an `interactive` value exists live (Loki was intermittently
    unavailable -- `loki-0` was mid-restart for part of this
    investigation -- before that check could be repeated). The panel using
    `attributes_terminal_type` is unfiltered on its value for exactly this
    reason: it will show whatever is real, rather than assuming one value.

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

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts" / "lightbridge-governance" / "dashboards" / "claude-code-telemetry.json"

# --------------------------------------------------------------------------
# Datasource contract -- same `__DS_LOKI__` token and `lokiUid` value the
# other three Loki-backed generators substitute (see
# templates/grafanadashboard-claude-code.yaml).
# --------------------------------------------------------------------------
LOKI_TYPE = "loki"
LOKI_UID = "__DS_LOKI__"
LOKI_DS = {"type": LOKI_TYPE, "uid": LOKI_UID}

# Every job label value this daemon's Claude Code traffic has been confirmed
# under across this epic's own history (generate_ai_cli_dashboard.py's own
# CLAUDE_CODE_JOB constant covered the first two; `ai-cli/claude-code`
# without the `-desktop` suffix is a third, newer value confirmed live in
# this dashboard's own investigation) -- matched together since they all
# carry the identical event shape.
CLAUDE_JOB = '{job=~"claude-code-desktop|ai-cli/claude-code|ai-cli/claude-code-desktop"}'

# Same convention as the other three generators: an empty panel must read as
# visibly "no data", never as a legitimate zero.
NO_DATA_MAPPING = {
    "type": "special",
    "options": {
        "match": "null+nan",
        "result": {"text": "NO DATA", "color": "red", "index": 0},
    },
}


class Ids:
    """Deterministic, sequential Grafana panel ids -- see
    generate_dashboards.py's own `Ids` for the full rationale. Duplicated
    rather than imported, same reasoning as the other three generators."""

    def __init__(self) -> None:
        self._next = 1

    def take(self) -> int:
        value = self._next
        self._next += 1
        return value


def row(ids: Ids, title: str, y: int) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "row",
        "title": title,
        "collapsed": False,
        "gridPos": {"h": 1, "w": 24, "x": 0, "y": y},
        "panels": [],
    }


def _base_field_config(
    *, unit: str, mappings: list[dict[str, Any]] | None = None,
    stacking: bool = False,
) -> dict[str, Any]:
    defaults: dict[str, Any] = {"unit": unit}
    if mappings:
        defaults["mappings"] = mappings
    defaults["thresholds"] = {"mode": "absolute", "steps": [{"color": "green", "value": None}]}
    if stacking:
        defaults["custom"] = {"stacking": {"mode": "normal"}}
    return {"defaults": defaults, "overrides": []}


def loki_timeseries_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    legend: str,
    unit: str,
    grid: dict[str, int],
    mappings: list[dict[str, Any]] | None = None,
    stacking: bool = False,
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "timeseries",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit, mappings=mappings, stacking=stacking),
        "options": {
            "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "sum"]},
            "tooltip": {"mode": "multi"},
        },
        "targets": [
            {
                "datasource": LOKI_DS,
                "expr": expr,
                "queryType": "range",
                "legendFormat": legend,
                "refId": "A",
            }
        ],
    }


def loki_stat_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    unit: str,
    grid: dict[str, int],
    mappings: list[dict[str, Any]] | None = None,
    reduce_calc: str = "lastNotNull",
    thresholds_steps: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """⚠️ `reduce_calc` default is `lastNotNull`, not `sum` -- same trap the
    other three generators document: a stat panel's `expr` embeds its OWN
    window and still runs as a RANGE query, so Loki returns one
    already-aggregated sample per step; `sum` would multiply by step count."""
    field_config = _base_field_config(unit=unit, mappings=mappings)
    if thresholds_steps:
        field_config["defaults"]["thresholds"] = {"mode": "absolute", "steps": thresholds_steps}
    return {
        "id": ids.take(),
        "type": "stat",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": field_config,
        "options": {
            "reduceOptions": {"calcs": [reduce_calc], "fields": "", "values": False},
            "orientation": "auto",
            "textMode": "auto",
            "colorMode": "value",
            "graphMode": "none",
            "justifyMode": "auto",
        },
        "targets": [
            {
                "datasource": LOKI_DS,
                "expr": expr,
                "queryType": "range",
                "legendFormat": "__auto",
                "refId": "A",
            }
        ],
    }


def loki_table_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    grid: dict[str, int],
    unit: str = "short",
) -> dict[str, Any]:
    """An instant metric query rendered as a table. Same shape as the other
    three generators' own `loki_table_panel`."""
    return {
        "id": ids.take(),
        "type": "table",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": unit}, "overrides": []},
        "options": {"showHeader": True, "cellHeight": "sm"},
        "targets": [
            {
                "datasource": LOKI_DS,
                "expr": expr,
                "queryType": "instant",
                "instant": True,
                "format": "table",
                "refId": "A",
            }
        ],
        "transformations": [
            {"id": "organize", "options": {"excludeByName": {"Time": True}}},
        ],
    }


def loki_piechart_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    grid: dict[str, int],
    legend: str = "__auto",
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "piechart",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": "short"}, "overrides": []},
        "options": {
            "legend": {"displayMode": "table", "placement": "right", "values": ["value", "percent"]},
            "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
            "pieType": "pie",
        },
        "targets": [
            {
                "datasource": LOKI_DS,
                "expr": expr,
                "queryType": "instant",
                "instant": True,
                "legendFormat": legend,
                "refId": "A",
            }
        ],
    }


def loki_joined_table_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    targets: list[dict[str, str]],
    join_field: str,
    grid: dict[str, int],
) -> dict[str, Any]:
    """Combines several independent instant-metric queries into one table by
    a shared field (Grafana's `joinByField` transform) -- the LogQL/Grafana
    equivalent of 25052's single big KQL `join` chain building its "User
    Summary" table: LogQL has no cross-stream join of its own, so each
    metric is its own target (own refId) and Grafana stitches the resulting
    per-target tables together client-side by `join_field`.

    `targets` is a list of {"refId": ..., "expr": ..., "legend": ...}.
    """
    return {
        "id": ids.take(),
        "type": "table",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": "short"}, "overrides": []},
        "options": {"showHeader": True, "cellHeight": "sm"},
        "targets": [
            {
                "datasource": LOKI_DS,
                "expr": t["expr"],
                "queryType": "instant",
                "instant": True,
                "format": "table",
                "legendFormat": t.get("legend", "__auto"),
                "refId": t["refId"],
            }
            for t in targets
        ],
        "transformations": [
            {
                "id": "joinByField",
                "options": {"byField": join_field, "mode": "outer"},
            },
            {"id": "organize", "options": {"excludeByName": {"Time": True, "Time 1": True, "Time 2": True}}},
        ],
    }


def loki_logs_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    grid: dict[str, int],
) -> dict[str, Any]:
    """A native Grafana Loki "Logs" panel -- raw matching log lines, not an
    aggregation. Used once, for "recent errors", where seeing the actual
    lines is the point rather than a count."""
    return {
        "id": ids.take(),
        "type": "logs",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "options": {
            "showTime": True,
            "showLabels": False,
            "wrapLogMessage": True,
            "sortOrder": "Descending",
            "enableLogDetails": True,
        },
        "targets": [
            {
                "datasource": LOKI_DS,
                "expr": expr,
                "queryType": "range",
                "refId": "A",
            }
        ],
    }


def build_dashboard() -> dict[str, Any]:
    ids = Ids()
    panels: list[dict[str, Any]] = []
    y = 0

    # ---------------------------------------------------------------
    # Section 1 -- Summary KPIs. Mirrors 25052's own top strip (h=4 stats,
    # not this repo's usual h=8) -- the layout the user specifically asked
    # to match. "Active Time" (25052's 6th KPI) is replaced with "Engineers
    # active" -- no duration-of-activity signal exists in this dashboard's
    # data (see module docstring); real per-engineer identity is a genuine
    # strength here instead.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Summary KPIs", y))
    y += 1

    panels.append(
        loki_stat_panel(
            ids,
            title="Total Cost (24h)",
            description=(
                f'sum(sum_over_time({CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_cost_usd [24h])). Confirmed live: ~$88.50 in a real 24h "
                "window across this org's Claude Code usage."
            ),
            expr=(
                f'sum(sum_over_time({CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_cost_usd [24h]))"
            ),
            unit="currencyUSD",
            grid={"h": 4, "w": 4, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Sessions (7d)",
            description=(
                "count(count by (attributes_session_id) (count_over_time(...[7d]))). Every "
                "event carries a real session.id -- a distinct-session count, not a metric "
                "counter subject to the Mimir gap this dashboard's docstring documents."
            ),
            expr=f"count(count by (attributes_session_id) (count_over_time({CLAUDE_JOB} | json [7d])))",
            unit="none",
            grid={"h": 4, "w": 4, "x": 4, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="User Prompts (24h)",
            description=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="user_prompt" [24h])).',
            expr=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="user_prompt" [24h]))',
            unit="none",
            grid={"h": 4, "w": 4, "x": 8, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="API Requests (24h)",
            description=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="api_request" [24h])).',
            expr=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="api_request" [24h]))',
            unit="none",
            grid={"h": 4, "w": 4, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="API Errors (24h)",
            description=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="api_error" [24h])).',
            expr=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="api_error" [24h]))',
            unit="none",
            grid={"h": 4, "w": 4, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[{"color": "green", "value": None}, {"color": "red", "value": 1}],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Engineers active (7d)",
            description=(
                "count(count by (attributes_user_email) (count_over_time(...[7d]))). Confirmed "
                "live: 3 distinct @adorsys.com/personal addresses in a real 7d window -- a small "
                "pilot group, not a bug."
            ),
            expr=(
                "count(count by (attributes_user_email) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_user_email=~".+" [7d])))'
            ),
            unit="none",
            grid={"h": 4, "w": 4, "x": 20, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 4

    # ---------------------------------------------------------------
    # Section 2 -- Cost & Token Trends.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Cost & Token Trends", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Daily Cost by Model",
            description=(
                "sum by (attributes_model) (sum_over_time({...} | "
                'attributes_event_name="api_request" | unwrap attributes_cost_usd '
                "[$__interval])). attributes_cost_usd AND attributes_cost_usd_micros are both "
                "confirmed emitted (RFC-0003: Claude Code is the one client shipping both "
                "USD and µUSD) -- cost_usd used here since no stored value in this repo's own "
                "schema is involved (ADR-0008 governs storage, not a read-only Loki panel)."
            ),
            expr=(
                "sum by (attributes_model) (sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_cost_usd [$__interval]))"
            ),
            legend="{{attributes_model}}",
            unit="currencyUSD",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_timeseries_panel(
            ids,
            title="Daily Token Usage by Type",
            description=(
                "input/output/cache-creation/cache-read tokens, each sum_over_time(... | "
                "unwrap attributes_<kind>_tokens [$__interval]), from api_request lines. All "
                "four fields confirmed live together."
            ),
            expr=(
                "sum(sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_input_tokens [$__interval]))"
            ),
            legend="input",
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    panels[-1]["targets"].extend(
        [
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                    "| unwrap attributes_output_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "output",
                "refId": "B",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                    "| unwrap attributes_cache_creation_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cache creation",
                "refId": "C",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                    "| unwrap attributes_cache_read_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cache read",
                "refId": "D",
            },
        ]
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 3 -- Model Usage Breakdown.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Model Usage Breakdown", y))
    y += 1

    panels.append(
        loki_piechart_panel(
            ids,
            title="Token Usage by Model (24h)",
            description=(
                "sum by (attributes_model) (sum_over_time({...} | unwrap "
                "attributes_input_tokens [24h])) -- input tokens as the volume proxy."
            ),
            expr=(
                "sum by (attributes_model) (sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_input_tokens [24h]))"
            ),
            grid={"h": 8, "w": 8, "x": 0, "y": y},
            legend="{{attributes_model}}",
        )
    )
    panels.append(
        loki_piechart_panel(
            ids,
            title="Cost by Model (24h)",
            description=(
                'sum by (attributes_model) (sum_over_time({...} | unwrap attributes_cost_usd '
                "[24h]))."
            ),
            expr=(
                "sum by (attributes_model) (sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_cost_usd [24h]))"
            ),
            grid={"h": 8, "w": 8, "x": 8, "y": y},
            legend="{{attributes_model}}",
        )
    )
    panels.append(
        loki_piechart_panel(
            ids,
            title="API Requests by Model (24h)",
            description='sum by (attributes_model) (count_over_time({...} | attributes_event_name="api_request" [24h])).',
            expr=(
                "sum by (attributes_model) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_request" [24h]))'
            ),
            grid={"h": 8, "w": 8, "x": 16, "y": y},
            legend="{{attributes_model}}",
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 4 -- Tool Usage & Approvals. See the module docstring's ⚠️
    # section -- decision alone is 99.4% policy default, not a human, and
    # this section makes that split explicit exactly like the Codex sibling.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Tool Usage & Approvals -- decision vs source (tool_decision)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Top tools invoked, over time",
            description=(
                "sum by (attributes_tool_name) (count_over_time({...} | "
                'attributes_event_name="tool_result" [$__interval])). Confirmed live values: '
                "Bash, Edit, Read, Write, ScheduleWakeup, Monitor, WebFetch, ToolSearch, "
                "mcp_tool, WebSearch, Agent, AskUserQuestion, TaskStop, SendUserFile."
            ),
            expr=(
                "sum by (attributes_tool_name) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="tool_result" [$__interval]))'
            ),
            legend="{{attributes_tool_name}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tool call decisions, by decision AND source",
            description=(
                "sum by (attributes_decision, attributes_source) (count_over_time({...} | "
                'attributes_event_name="tool_decision" [$__interval])). `source` is the '
                "load-bearing dimension: `config` is a sandbox/permission-mode default, NOT an "
                "engineer's decision. Confirmed live over 24h: 702 accept/config, 2 "
                "accept/user_temporary, 2 reject/config -- 99.4% never reached a human."
            ),
            expr=(
                "sum by (attributes_decision, attributes_source) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="tool_decision" [$__interval]))'
            ),
            legend="{{attributes_decision}} / {{attributes_source}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        loki_stat_panel(
            ids,
            title="Code edit decisions (Edit/Write/NotebookEdit), source=config share (7d)",
            description=(
                "(count with source=config) / (count total), both restricted to tool_decision "
                "on code-editing tools over 7d. Confirmed live: 12,585 accept/config vs 1 "
                "reject/config in the scope checked -- ZERO source=user decisions observed. "
                "The closest analogue to 25052's `code_edit_tool.decision`-based Accept Rate "
                "panel, but split by source so a near-100% figure here reads as \"policy "
                "default\", not \"engineers love what the agent writes\"."
            ),
            expr=(
                "sum(count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="tool_decision" '
                '| attributes_tool_name=~"Edit|Write|NotebookEdit" | attributes_source="config" [7d])) '
                "/ sum(count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="tool_decision" '
                '| attributes_tool_name=~"Edit|Write|NotebookEdit" [7d]))'
            ),
            unit="percentunit",
            grid={"h": 8, "w": 8, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tool execution success rate, over time",
            description=(
                "sum by (attributes_success) (count_over_time({...} | "
                'attributes_event_name="tool_result" [$__interval])). A signal Codex\'s own '
                "telemetry does not carry at all (see codex-telemetry.json's own docstring) -- "
                "confirmed live: 663 true / 39 false over a real 24h window (94.4%)."
            ),
            expr=(
                "sum by (attributes_success) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="tool_result" [$__interval]))'
            ),
            legend="success={{attributes_success}}",
            unit="ops",
            grid={"h": 8, "w": 8, "x": 8, "y": y},
        )
    )
    panels.append(
        loki_piechart_panel(
            ids,
            title="Invocation source (query_source, 7d)",
            description=(
                "sum by (attributes_query_source) (count_over_time({...} | "
                'attributes_event_name="assistant_response" [7d])). Independent of '
                "app.entrypoint -- confirmed live values: sdk, prompt_suggestion, "
                "agent:builtin:general-purpose, web_fetch_apply, web_search_tool, compact. "
                "\"sdk\" dominating (669/726 in the sample checked) means most traffic in this "
                "fleet is Agent-SDK-driven, not a human typing at an interactive prompt."
            ),
            expr=(
                "sum by (attributes_query_source) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="assistant_response" [7d]))'
            ),
            grid={"h": 8, "w": 8, "x": 16, "y": y},
            legend="{{attributes_query_source}}",
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 5 -- Per-User Metrics. LogQL has no cross-stream join
    # (unlike 25052's own multi-`let` KQL query for its "User Summary"
    # table) -- loki_joined_table_panel combines independent per-metric
    # targets client-side via Grafana's joinByField transform instead.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Per-User Metrics", y))
    y += 1

    panels.append(
        loki_joined_table_panel(
            ids,
            title="User Summary (7d)",
            description=(
                "Sessions, prompts, tool calls, API requests, API errors, cost and tokens, one "
                "row per engineer -- joined by attributes_user_email across five independent "
                "LogQL aggregations (Grafana's joinByField transform), the equivalent of "
                "25052's own multi-`let` KQL join for the same table."
            ),
            targets=[
                {
                    "refId": "A",
                    "legend": "sessions",
                    "expr": (
                        "count by (attributes_user_email) (count by (attributes_user_email, "
                        "attributes_session_id) (count_over_time("
                        f'{CLAUDE_JOB} | json | attributes_user_email=~".+" [7d])))'
                    ),
                },
                {
                    "refId": "B",
                    "legend": "prompts",
                    "expr": (
                        "sum by (attributes_user_email) (count_over_time("
                        f'{CLAUDE_JOB} | json | attributes_event_name="user_prompt" [7d]))'
                    ),
                },
                {
                    "refId": "C",
                    "legend": "tool_calls",
                    "expr": (
                        "sum by (attributes_user_email) (count_over_time("
                        f'{CLAUDE_JOB} | json | attributes_event_name="tool_decision" [7d]))'
                    ),
                },
                {
                    "refId": "D",
                    "legend": "api_requests",
                    "expr": (
                        "sum by (attributes_user_email) (count_over_time("
                        f'{CLAUDE_JOB} | json | attributes_event_name="api_request" [7d]))'
                    ),
                },
                {
                    "refId": "E",
                    "legend": "api_errors",
                    "expr": (
                        "sum by (attributes_user_email) (count_over_time("
                        f'{CLAUDE_JOB} | json | attributes_event_name="api_error" [7d]))'
                    ),
                },
                {
                    "refId": "F",
                    "legend": "cost_usd",
                    "expr": (
                        "sum by (attributes_user_email) (sum_over_time("
                        f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                        "| unwrap attributes_cost_usd [7d]))"
                    ),
                },
                {
                    "refId": "G",
                    "legend": "input_tokens",
                    "expr": (
                        "sum by (attributes_user_email) (sum_over_time("
                        f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                        "| unwrap attributes_input_tokens [7d]))"
                    ),
                },
            ],
            join_field="attributes_user_email",
            grid={"h": 8, "w": 24, "x": 0, "y": y},
        )
    )
    y += 8

    panels.append(
        loki_table_panel(
            ids,
            title="Per-User Cost by Model (7d)",
            description=(
                "sum by (attributes_user_email, attributes_model) (sum_over_time({...} | "
                "unwrap attributes_cost_usd [7d])) -- both dimensions of the same event, so a "
                "single query suffices (no join needed, unlike the User Summary table above)."
            ),
            expr=(
                "sum by (attributes_user_email, attributes_model) (sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_request" '
                "| unwrap attributes_cost_usd [7d]))"
            ),
            grid={"h": 8, "w": 24, "x": 0, "y": y},
            unit="currencyUSD",
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 6 -- Errors & Reliability.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Errors & Reliability", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="API Errors Over Time",
            description=f'count_over_time({CLAUDE_JOB} | json | attributes_event_name="api_error" [$__interval]).',
            expr=(
                "sum(count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="api_error" [$__interval]))'
            ),
            legend="api_error",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_logs_panel(
            ids,
            title="Error Details (raw, 7d)",
            description=(
                'A native Loki "Logs" panel -- the raw api_error/tool_result(success=false) '
                "lines themselves, not an aggregate. Prompt content is never captured here "
                "(redacted at the source)."
            ),
            expr=(
                f'{CLAUDE_JOB} | json | attributes_event_name=~"api_error|tool_result" '
                '| attributes_success="false" or attributes_event_name="api_error"'
            ),
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 7 -- Hooks (hook_execution_start/.complete, hook_registered).
    # No analogue in 25052 -- see module docstring.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Hooks (hook_execution_start / .complete) -- not in 25052", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Hook execution outcomes, over time",
            description=(
                "success/blocking/cancelled/non-blocking-error counts, each sum_over_time(... "
                "| unwrap attributes_num_<kind> [$__interval]) from hook_execution_complete "
                "lines. All four fields confirmed live together."
            ),
            expr=(
                "sum(sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="hook_execution_complete" '
                "| unwrap attributes_num_success [$__interval]))"
            ),
            legend="success",
            unit="none",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
            stacking=True,
        )
    )
    panels[-1]["targets"].extend(
        [
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CLAUDE_JOB} | json | attributes_event_name="hook_execution_complete" '
                    "| unwrap attributes_num_blocking [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "blocking",
                "refId": "B",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CLAUDE_JOB} | json | attributes_event_name="hook_execution_complete" '
                    "| unwrap attributes_num_cancelled [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cancelled",
                "refId": "C",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CLAUDE_JOB} | json | attributes_event_name="hook_execution_complete" '
                    "| unwrap attributes_num_non_blocking_error [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "non-blocking error",
                "refId": "D",
            },
        ]
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top hooks, by invocation count (7d)",
            description=(
                "topk(10, sum by (attributes_hook_name) (count_over_time({...} | "
                'attributes_event_name="hook_execution_complete" [7d]))). Confirmed live: '
                "PostToolUse:Bash dominates volume."
            ),
            expr=(
                "topk(10, sum by (attributes_hook_name) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="hook_execution_complete" [7d])))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 8 -- MCP servers & plugins (mcp_server_connection,
    # plugin_loaded). No analogue in 25052.
    # ---------------------------------------------------------------
    panels.append(row(ids, "MCP servers & plugins -- not in 25052", y))
    y += 1

    panels.append(
        loki_piechart_panel(
            ids,
            title="MCP server connections, by status (7d)",
            description=(
                "sum by (attributes_status) (count_over_time({...} | "
                'attributes_event_name="mcp_server_connection" [7d])). Confirmed live values: '
                "connected, disconnected."
            ),
            expr=(
                "sum by (attributes_status) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="mcp_server_connection" [7d]))'
            ),
            grid={"h": 8, "w": 8, "x": 0, "y": y},
            legend="{{attributes_status}}",
        )
    )
    panels.append(
        loki_piechart_panel(
            ids,
            title="MCP server connections, by transport (7d)",
            description=(
                "sum by (attributes_transport_type) (count_over_time({...} | "
                'attributes_event_name="mcp_server_connection" [7d])). Confirmed live values: '
                "stdio, http."
            ),
            expr=(
                "sum by (attributes_transport_type) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="mcp_server_connection" [7d]))'
            ),
            grid={"h": 8, "w": 8, "x": 8, "y": y},
            legend="{{attributes_transport_type}}",
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Plugins loaded, by marketplace (7d)",
            description=(
                "sum by (attributes_plugin_name, attributes_marketplace_name) "
                '(count_over_time({...} | attributes_event_name="plugin_loaded" [7d])).'
            ),
            expr=(
                "sum by (attributes_plugin_name, attributes_marketplace_name) (count_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="plugin_loaded" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 9 -- Data retention & privacy hygiene (retention_sweep). No
    # analogue in 25052 -- ties directly to this platform's own governance
    # framing (redaction, content-capture policy) rather than being a
    # generic "more metrics" addition.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Data retention & privacy hygiene (retention_sweep) -- not in 25052", y))
    y += 1

    panels.append(
        loki_stat_panel(
            ids,
            title="Retention sweeps run (7d)",
            description=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="retention_sweep" [7d])).',
            expr=f'sum(count_over_time({CLAUDE_JOB} | json | attributes_event_name="retention_sweep" [7d]))',
            unit="none",
            grid={"h": 6, "w": 6, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Transcripts deleted (7d)",
            description=(
                "sum(sum_over_time({...} | unwrap attributes_transcripts_deleted [7d])), from "
                "retention_sweep lines."
            ),
            expr=(
                "sum(sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="retention_sweep" '
                "| unwrap attributes_transcripts_deleted [7d]))"
            ),
            unit="none",
            grid={"h": 6, "w": 6, "x": 6, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Session files deleted (7d)",
            description="sum(sum_over_time({...} | unwrap attributes_session_files_deleted [7d])).",
            expr=(
                "sum(sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="retention_sweep" '
                "| unwrap attributes_session_files_deleted [7d]))"
            ),
            unit="none",
            grid={"h": 6, "w": 6, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="History entries pruned (7d)",
            description="sum(sum_over_time({...} | unwrap attributes_history_entries_pruned [7d])).",
            expr=(
                "sum(sum_over_time("
                f'{CLAUDE_JOB} | json | attributes_event_name="retention_sweep" '
                "| unwrap attributes_history_entries_pruned [7d]))"
            ),
            unit="none",
            grid={"h": 6, "w": 6, "x": 18, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 6

    dashboard: dict[str, Any] = {
        "id": None,
        "uid": "governance-claude-code-telemetry",
        "title": "Claude Code telemetry",
        "description": (
            "Cost, tokens, tool/approval activity (human-vs-policy-approval split made "
            "explicit), hooks, MCP servers, plugins, data-retention hygiene and per-engineer "
            "spend for Claude Code, sourced from the aiCliOtel collector's forwarded OTLP logs "
            "in Loki. Benchmarked against Grafana's own official integration dashboard "
            "(grafana.com/grafana/dashboards/25052) and built to match its panel coverage and "
            "exceed it (hooks/MCP/plugins/retention have no analogue there); lines-of-code and "
            "active-time are the two 25052 panels this dashboard cannot reproduce, and it says "
            "so rather than faking them (see this script's own docstring). Generated by "
            "scripts/generate_claude_code_dashboard.py -- do not hand-edit; regenerate instead."
        ),
        "tags": ["governance", "claude-code"],
        "style": "dark",
        "timezone": "browser",
        "editable": True,
        "graphTooltip": 1,
        "schemaVersion": 39,
        "version": 1,
        "refresh": "1m",
        "time": {"from": "now-7d", "to": "now"},
        "timepicker": {},
        "templating": {"list": []},
        "annotations": {"list": []},
        "links": [],
        "panels": panels,
    }
    return dashboard


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
