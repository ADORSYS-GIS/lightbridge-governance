#!/usr/bin/env python3
"""Generate the AI CLI telemetry Grafana dashboard JSON.

DEV-TIME TOOL ONLY, same convention as `generate_dashboards.py` (see that
file's own docstring for the full rationale -- this one only restates what
differs). Standard library only. The **generated JSON is committed** at
`charts/lightbridge-governance/dashboards/ai-cli-telemetry.json`; that file,
not this script, is what `helm template`/the Grafana Operator actually reads.

Determinism contract: identical to `generate_dashboards.py` -- no
timestamps, no `random`, no `uuid`, no hash-seed-dependent ordering, panel
ids from a plain counter over a fixed call sequence.

## Why Loki, not Prometheus/Postgres

Epic #260's daemon (`serve --otel`, issue #268) forwards every OTLP record
it receives to the real governed collector, which fans logs out to Grafana
Loki (confirmed live: `otelcol_receiver_accepted_log_records` incrementing
in step with a real request, and the exact bytes landing in Loki under
`{job="ai-cli/claude-code-desktop"}` moments later). The Foundry push
pipeline (RFC-0002) that would land rows in this repo's own Postgres is
still Draft, undeployed -- `governance_ingest_executions_total` reads 0 in
the live cluster -- so Loki over the collector's own log stream is the one
telemetry surface this epic can actually visualize today, not a choice
between backends.

## Field names, and how confident this script is in them

Every LogQL expression below was run against the live `loki-gateway` in the
`observability` namespace (not guessed from documentation) before being
written here, against real traffic this session generated (this machine's
own `governance-auth` daemon, live, plus other developers' real Claude Code
sessions sharing the same collector). What was directly confirmed:

  - `job`/`service_name` labels: `ai-cli/claude-code-desktop`,
    `claude-code-desktop`, `opencode` all appear; Codex did not emit real
    traffic in this session, so no Codex-specific job value is confirmed --
    the volume panel's `job=~".+"` is deliberately unfiltered so a Codex (or
    Copilot, once #272's otlp-http path sees real VS Code traffic) job value
    shows up the moment one exists, with no dashboard edit required.
  - Loki's `json` parser flattens nested keys with `_`, confirmed live:
    `attributes.cost_usd` -> `attributes_cost_usd`, `attributes.user.email`
    (itself a dotted key inside the JSON) -> `attributes_user_email`.
  - `attributes_cost_usd`, `attributes_model`, `attributes_decision`,
    `attributes_tool_name`, `attributes_user_email`, `attributes_input_tokens`
    / `attributes_output_tokens` / `attributes_cache_creation_tokens` /
    `attributes_cache_read_tokens`, `attributes_duration_ms` all round-tripped
    through a real `sum by (...) (sum_over_time(... | json | unwrap ...))`
    or `count_over_time(... | json)` query and returned real, sensible
    values (e.g. real `cost_usd` in the low single-digit USD range per
    5-minute window, matching Claude Opus 5 pricing).
  - These fields are confirmed for `claude_code.api_request` / `.tool_decision`
    events on the `ai-cli/claude-code-desktop` job specifically. `opencode`'s
    own event shape was NOT inspected -- this dashboard's cost/token/tool
    panels are scoped to `job="ai-cli/claude-code-desktop"` deliberately,
    not because other clients don't matter, but because asserting a field
    name for a shape nobody looked at is exactly the guessing this docstring
    is trying to avoid. Extend once a real payload from another client has
    been inspected the same way.

## What is NOT confirmed, unlike the panels above

  - The Loki datasource UID. No `GrafanaDatasource` CR for Loki is visible
    to this session's cluster access (only one CR exists in `observability`
    at all: `lci-postgres`, for something unrelated) -- exactly the same
    gap `generate_dashboards.py`'s own `prometheusUid: mimir` already
    documents for Mimir. `loki` is the same class of reasonable-default
    guess (Grafana's own quick-start provisioning commonly names a
    datasource after its product), not a confirmed value. See
    values.yaml's `grafanaDashboard.datasources.lokiUid` for the same loud
    caveat, and CONFIRM AGAINST THE DEPLOYED GRAFANA before trusting a
    panel that renders empty.
  - Claude Code's own OTLP *metrics* signal (`claude_code.*`,
    `OTEL_METRICS_EXPORTER=otlp` is part of this epic's own wiring) --
    checked directly in Mimir (every `__name__` value, substring-matched)
    and NOT found under any prefix. Whatever the `ai-cli-otel-collector`'s
    metrics pipeline does with them, it does not appear to reach Mimir
    today. Left unresolved rather than guessed at.

## Section 4's data source is a DIFFERENT collector than Sections 1-3

Checked in Mimir directly (`__name__` label values, substring search):
`claude_code_*` and `codex_*` metrics do not exist under any prefix, but
`copilot_chat_*` does, richly -- `copilot_chat_lines_of_code_count_total`
(labels `type=added|removed`, `copilot_chat_language_id`),
`copilot_chat_session_count_total`, `copilot_chat_tool_call_count_total`,
`copilot_chat_chat_edit_outcome_count_total` (labels
`copilot_chat_edit_outcome=accepted|rejected`), all with real, non-empty
history over the last 7 days (confirmed via `query_range`, not just the
label existing). Two `job` label values appear on every one of these,
`copilot-chat` and `ai-cli/copilot-chat` -- almost certainly the pre-#272
file-exporter path and the post-#272 daemon path respectively, both
already landing in the same metric. Section 4's panels deliberately do not
split by `job`: comparing the two paths' adoption is a real, separate
question this dashboard does not try to answer yet.

This means Section 4 is Prometheus/Mimir-backed (reusing the SAME
`__DS_PROMETHEUS__` token and `prometheusUid` value
`generate_dashboards.py`'s copilot-connector dashboard already uses -- no
new datasource UID to confirm), while Sections 1-3 are Loki-backed. Two
different backends in one dashboard because that is genuinely where each
client's data actually lives today, not a stylistic choice.

Usage:
    python3 scripts/generate_ai_cli_dashboard.py           # regenerate the file
    python3 scripts/generate_ai_cli_dashboard.py --check   # verify it's current
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts" / "lightbridge-governance" / "dashboards" / "ai-cli-telemetry.json"

# --------------------------------------------------------------------------
# Datasource contract (charts/lightbridge-governance/values.yaml's
# `grafanaDashboard.datasources.lokiUid` substitutes this literal token at
# `helm template` time -- see templates/grafanadashboard-ai-cli.yaml).
# --------------------------------------------------------------------------
LOKI_TYPE = "loki"
LOKI_UID = "__DS_LOKI__"
LOKI_DS = {"type": LOKI_TYPE, "uid": LOKI_UID}

# Section 4 only -- see the module docstring's "different collector" note.
# Same token generate_dashboards.py's copilot-connector dashboard already
# uses; substituted from the same values.yaml `prometheusUid`.
PROM_TYPE = "prometheus"
PROM_UID = "__DS_PROMETHEUS__"
PROM_DS = {"type": PROM_TYPE, "uid": PROM_UID}

# The one job value confirmed to carry the `claude_code.*` event shape this
# script's cost/token/tool panels parse. See the module docstring's "what is
# NOT confirmed" section for why this is not widened to `job=~".+"`.
CLAUDE_CODE_JOB = 'job="ai-cli/claude-code-desktop"'

# Same convention as generate_dashboards.py: an empty panel must read as
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
    rather than imported: these two dev-time scripts are independent
    tools, and importing one from the other would couple their release
    cycles for four lines of code."""

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
) -> dict[str, Any]:
    defaults: dict[str, Any] = {"unit": unit}
    if mappings:
        defaults["mappings"] = mappings
    defaults["thresholds"] = {"mode": "absolute", "steps": [{"color": "green", "value": None}]}
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
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "timeseries",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit, mappings=mappings),
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
) -> dict[str, Any]:
    """⚠️ `reduce_calc` default is `lastNotNull`, not `sum` (review finding:
    every stat panel here embeds its OWN window in `expr` itself, e.g. a
    literal `[24h]` bracket -- the target still runs as a **range** query
    over the dashboard's own (now-7d) time range, so Loki returns one
    sample per step, each ALREADY the full trailing-24h aggregate.
    Reducing those samples with `sum` multiplies the true value by the
    number of steps (~100-1000x on a 7d/1m-refresh dashboard), not
    "total over 24h". `lastNotNull` reads the one number that's actually
    correct -- the most recent already-fully-aggregated sample -- which is
    what every current caller of this function wants."""
    return {
        "id": ids.take(),
        "type": "stat",
        "title": title,
        "description": description,
        "datasource": LOKI_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit, mappings=mappings),
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
    """An instant metric query rendered as a table -- the Loki-datasource
    equivalent of `generate_dashboards.py`'s `pg_table_panel`, for a
    snapshot ranking (e.g. "top tools", "cost by user") rather than a
    trend over time."""
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


def prom_timeseries_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    legend: str,
    unit: str,
    grid: dict[str, int],
) -> dict[str, Any]:
    """Section 4 only -- mirrors generate_dashboards.py's own
    `prom_timeseries_panel` exactly (same shape, `PROM_DS` in place of
    that script's own datasource constant)."""
    return {
        "id": ids.take(),
        "type": "timeseries",
        "title": title,
        "description": description,
        "datasource": PROM_DS,
        "gridPos": grid,
        "fieldConfig": {
            "defaults": {"unit": unit, "thresholds": {"mode": "absolute", "steps": [{"color": "green", "value": None}]}},
            "overrides": [],
        },
        "options": {
            "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "sum"]},
            "tooltip": {"mode": "multi"},
        },
        "targets": [
            {
                "datasource": PROM_DS,
                "expr": expr,
                "instant": False,
                "range": True,
                "legendFormat": legend,
                "refId": "A",
            }
        ],
    }


def prom_stat_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    unit: str,
    grid: dict[str, int],
    mappings: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    defaults: dict[str, Any] = {"unit": unit, "thresholds": {"mode": "absolute", "steps": [{"color": "green", "value": None}]}}
    if mappings:
        defaults["mappings"] = mappings
    return {
        "id": ids.take(),
        "type": "stat",
        "title": title,
        "description": description,
        "datasource": PROM_DS,
        "gridPos": grid,
        "fieldConfig": {"defaults": defaults, "overrides": []},
        "options": {
            "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
            "orientation": "auto",
            "textMode": "auto",
            "colorMode": "value",
            "graphMode": "none",
            "justifyMode": "auto",
        },
        "targets": [
            {
                "datasource": PROM_DS,
                "expr": expr,
                "instant": True,
                "range": False,
                "legendFormat": "__auto",
                "refId": "A",
            }
        ],
    }


def prom_table_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    grid: dict[str, int],
    unit: str = "short",
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "table",
        "title": title,
        "description": description,
        "datasource": PROM_DS,
        "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": unit}, "overrides": []},
        "options": {"showHeader": True, "cellHeight": "sm"},
        "targets": [
            {
                "datasource": PROM_DS,
                "expr": expr,
                "instant": True,
                "range": False,
                "format": "table",
                "refId": "A",
            }
        ],
        "transformations": [
            {"id": "organize", "options": {"excludeByName": {"Time": True}}},
        ],
    }


def build_dashboard() -> dict[str, Any]:
    ids = Ids()
    panels: list[dict[str, Any]] = []
    y = 0

    # ---------------------------------------------------------------
    # Section 1 -- Volume & adoption, across every AI-CLI client this
    # epic's daemon serves (issue #268/#272). Unfiltered on `job` on
    # purpose -- see the module docstring.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Volume & adoption (issue #268/#272)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Events per minute, by client",
            description=(
                "sum by (job) (count_over_time({job=~\".+\"}[1m])). Confirmed live: "
                "ai-cli/claude-code-desktop, claude-code-desktop and opencode all appear "
                "under this collector today. A Codex or Copilot (#272) job value showing up "
                "here needs no dashboard change -- this query is unfiltered."
            ),
            expr='sum by (job) (count_over_time({job=~".+"}[$__interval]))',
            legend="{{job}}",
            unit="ops",
            grid={"h": 8, "w": 16, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Total events (24h)",
            description="sum(count_over_time({job=~\".+\"}[24h])) at the dashboard's own refresh.",
            expr='sum(count_over_time({job=~".+"}[24h]))',
            unit="none",
            grid={"h": 8, "w": 4, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Distinct users active (24h)",
            description=(
                "count(count by (attributes_user_email) (count_over_time({"
                + CLAUDE_CODE_JOB
                + "} | json [24h]))). Scoped to Claude Code's own event shape "
                "(attributes.user.email) -- see the module docstring's confirmed-field list."
            ),
            expr=(
                "count(count by (attributes_user_email) (count_over_time({"
                + CLAUDE_CODE_JOB
                + "} | json [24h])))"
            ),
            unit="none",
            grid={"h": 8, "w": 4, "x": 20, "y": y},
            mappings=[NO_DATA_MAPPING],
            reduce_calc="lastNotNull",
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 2 -- Cost & tokens (Claude Code's own claude_code.api_request
    # event). The direct answer to "what is this costing us", the same
    # question copilot-connector's Section 4 answers for Copilot seats.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Cost & tokens -- Claude Code (claude_code.api_request)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Cost, by model",
            description=(
                "sum by (attributes_model) (sum_over_time({...} |= \"api_request\" | json | "
                "unwrap attributes_cost_usd [$__interval])). attributes_cost_usd is the "
                "same field Claude Code's own cost accounting derives from -- confirmed live "
                "returning real per-request USD values (low single digits per 5-minute "
                "window under Opus 5 at the time this was checked)."
            ),
            expr=(
                "sum by (attributes_model) (sum_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "api_request" | json | unwrap attributes_cost_usd [$__interval]))'
            ),
            legend="{{attributes_model}}",
            unit="currencyUSD",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Total cost (24h)",
            description="sum(sum_over_time({...} |= \"api_request\" | json | unwrap attributes_cost_usd [24h])).",
            expr=(
                "sum(sum_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "api_request" | json | unwrap attributes_cost_usd [24h]))'
            ),
            unit="currencyUSD",
            grid={"h": 8, "w": 4, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Cost by user (24h)",
            description=(
                "topk(10, sum by (attributes_user_email) (sum_over_time({...} | json | "
                "unwrap attributes_cost_usd [24h]))), as an instant snapshot table. Mirrors "
                "copilot-connector's own 'Cost per active user' panel for the Copilot seat "
                "data -- same question, this epic's data source."
            ),
            expr=(
                "topk(10, sum by (attributes_user_email) (sum_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "api_request" | json | unwrap attributes_cost_usd [24h])))'
            ),
            unit="currencyUSD",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
        )
    )
    y += 8

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tokens, by kind",
            description=(
                "input/output/cache-creation/cache-read tokens, each "
                "sum_over_time(... | json | unwrap attributes_<kind>_tokens [$__interval]). "
                "All four fields confirmed live against real claude_code.api_request lines."
            ),
            expr=(
                "sum(sum_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "api_request" | json | unwrap attributes_input_tokens [$__interval]))'
            ),
            legend="input",
            unit="none",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    # The remaining three token kinds share the same panel shape; built as
    # separate targets on ONE panel (Loki's datasource supports multiple
    # queries per panel exactly like Prometheus) rather than four panels --
    # a stacked comparison is the point, not four numbers nobody compares.
    panels[-1]["targets"].extend(
        [
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time({"
                    + CLAUDE_CODE_JOB
                    + '} |= "api_request" | json | unwrap attributes_output_tokens [$__interval]))'
                ),
                "queryType": "range",
                "legendFormat": "output",
                "refId": "B",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time({"
                    + CLAUDE_CODE_JOB
                    + '} |= "api_request" | json | unwrap attributes_cache_creation_tokens [$__interval]))'
                ),
                "queryType": "range",
                "legendFormat": "cache creation",
                "refId": "C",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time({"
                    + CLAUDE_CODE_JOB
                    + '} |= "api_request" | json | unwrap attributes_cache_read_tokens [$__interval]))'
                ),
                "queryType": "range",
                "legendFormat": "cache read",
                "refId": "D",
            },
        ]
    )
    panels.append(
        loki_timeseries_panel(
            ids,
            title="API request latency (avg duration_ms)",
            description=(
                "avg_over_time({...} |= \"api_request\" | json | unwrap attributes_duration_ms "
                "[$__interval]). The request-level latency Claude Code itself measured, not "
                "this daemon's own forwarding time."
            ),
            expr=(
                "avg(avg_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "api_request" | json | unwrap attributes_duration_ms [$__interval]))'
            ),
            legend="avg duration",
            unit="ms",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 3 -- Tool activity (claude_code.tool_decision). What the
    # agent is actually doing, and how often a developer accepts it.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Tool activity -- Claude Code (claude_code.tool_decision)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tool decisions, accept vs reject",
            description=(
                "sum by (attributes_decision) (count_over_time({...} |= \"tool_decision\" | "
                "json [$__interval])). A rising reject share is the signal worth alerting a "
                "human to look at, independent of raw volume."
            ),
            expr=(
                "sum by (attributes_decision) (count_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "tool_decision" | json [$__interval]))'
            ),
            legend="{{attributes_decision}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top tools (24h)",
            description=(
                "topk(10, sum by (attributes_tool_name) (count_over_time({...} |= "
                "\"tool_decision\" | json [24h]))). attributes_tool_name confirmed live "
                "(e.g. \"Bash\")."
            ),
            expr=(
                "topk(10, sum by (attributes_tool_name) (count_over_time({"
                + CLAUDE_CODE_JOB
                + '} |= "tool_decision" | json [24h])))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 4 -- Code changes, VS Code Copilot Chat. A DIFFERENT
    # collector/backend than Sections 1-3 -- see the module docstring's
    # "different collector" note for why, and for what `job=copilot-chat`
    # vs `job=ai-cli/copilot-chat` most likely means.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Code changes -- VS Code Copilot Chat (Mimir)", y))
    y += 1

    panels.append(
        prom_timeseries_panel(
            ids,
            title="Lines added / removed, by language",
            description=(
                "sum by (copilot_chat_language_id, type) "
                "(increase(copilot_chat_lines_of_code_count_total[$__interval])). "
                "Confirmed live in Mimir with real, non-empty history over the last 7 "
                "days -- languages seen: just, rust, yaml, shellscript. Not filtered by "
                "`job`: both copilot-chat (pre-#272 file exporter) and ai-cli/copilot-chat "
                "(post-#272 daemon) already report the same metric, and comparing the two "
                "paths' adoption is a separate question this panel does not answer."
            ),
            expr=(
                "sum by (copilot_chat_language_id, type) "
                "(increase(copilot_chat_lines_of_code_count_total[$__interval]))"
            ),
            legend="{{copilot_chat_language_id}} ({{type}})",
            unit="short",
            grid={"h": 8, "w": 16, "x": 0, "y": y},
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Net lines (7d)",
            description=(
                "sum(increase(...{type=\"added\"}[7d])) - sum(increase(...{type=\"removed\"}"
                "[7d])). A single top-line answer to 'is the agent writing code, on net' -- "
                "the same question 'lines of code the agents are writing' asks, reduced to "
                "one number."
            ),
            expr=(
                'sum(increase(copilot_chat_lines_of_code_count_total{type="added"}[7d])) - '
                'sum(increase(copilot_chat_lines_of_code_count_total{type="removed"}[7d]))'
            ),
            unit="short",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    panels.append(
        prom_table_panel(
            ids,
            title="Lines added by language (7d)",
            description=(
                "topk(10, sum by (copilot_chat_language_id) (increase(...{type=\"added\"}"
                "[7d]))), as an instant snapshot table."
            ),
            expr=(
                "topk(10, sum by (copilot_chat_language_id) "
                '(increase(copilot_chat_lines_of_code_count_total{type="added"}[7d])))'
            ),
            unit="short",
            grid={"h": 8, "w": 8, "x": 0, "y": y},
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Sessions (7d)",
            description="sum(increase(copilot_chat_session_count_total[7d])).",
            expr="sum(increase(copilot_chat_session_count_total[7d]))",
            unit="none",
            grid={"h": 8, "w": 4, "x": 8, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Tool calls (7d)",
            description="sum(increase(copilot_chat_tool_call_count_total[7d])).",
            expr="sum(increase(copilot_chat_tool_call_count_total[7d]))",
            unit="none",
            grid={"h": 8, "w": 4, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Edit acceptance rate (7d)",
            description=(
                "accepted / (accepted + rejected), from "
                "copilot_chat_chat_edit_outcome_count_total{copilot_chat_edit_outcome=...}. "
                "A dropping rate is the signal worth a human's attention -- the same "
                "'is what the agent proposes actually good' question Section 3's tool-decision "
                "accept/reject panel asks for Claude Code's own tool calls."
            ),
            expr=(
                'sum(increase(copilot_chat_chat_edit_outcome_count_total{copilot_chat_edit_outcome="accepted"}[7d])) '
                "/ "
                "sum(increase(copilot_chat_chat_edit_outcome_count_total[7d]))"
            ),
            unit="percentunit",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    dashboard: dict[str, Any] = {
        "id": None,
        "uid": "governance-ai-cli-telemetry",
        "title": "AI CLI telemetry",
        "description": (
            "Volume, cost, tokens, tool activity and lines of code for Claude Code / Codex / "
            "VS Code Copilot, sourced from the loopback daemon's forwarded OTLP -- Loki for "
            "Sections 1-3, Mimir for Section 4's Copilot Chat code-change metrics (issue "
            "#268/#269/#270/#271/#272). Generated by scripts/generate_ai_cli_dashboard.py -- "
            "do not hand-edit; regenerate instead."
        ),
        "tags": ["governance", "ai-cli"],
        "style": "dark",
        "timezone": "browser",
        "editable": True,
        "graphTooltip": 1,
        "schemaVersion": 39,
        "version": 1,
        "refresh": "1m",
        # 7d, not 24h: Section 4's copilot_chat_* metrics are genuinely sparse
        # (points hours to days apart in the confirmed live query), so a 24h
        # window would render several of its panels emptier than the data
        # actually is. Sections 1-3's Loki data (much higher volume) reads
        # fine over 7d too -- the window is chosen for the sparser source.
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
