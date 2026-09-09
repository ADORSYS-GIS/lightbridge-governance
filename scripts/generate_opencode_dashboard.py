#!/usr/bin/env python3
"""Generate the OpenCode telemetry Grafana dashboard JSON.

DEV-TIME TOOL ONLY, same convention as `generate_dashboards.py` and
`generate_ai_cli_dashboard.py` (see the former's own docstring for the full
rationale -- this one only restates what differs). Standard library only.
The **generated JSON is committed** at
`charts/lightbridge-governance/dashboards/opencode-telemetry.json`; that
file, not this script, is what `helm template`/the Grafana Operator
actually reads.

Determinism contract: identical to the other two generators -- no
timestamps, no `random`, no `uuid`, no hash-seed-dependent ordering, panel
ids from a plain counter over a fixed call sequence.

## Data source: Loki, one job, TWO instrumentation scopes

`otelcollector-opencode.yaml` (values.yaml's `opencodeOtel`, RFC-0003's
amended OpenCode row) forwards to the same Alloy -> Loki pipeline the AI CLI
daemon uses, landing under a single label value: `job="opencode"` (confirmed
live 2026-09-09 against the real `loki-gateway.observability` via
`kubectl port-forward`, run against this org's actual developers' actual
OpenCode sessions -- 25+ distinct hostnames, 9+ distinct git repos, real
model names, over the trailing 7 days).

That one job carries lines from TWO different instrumentation scopes,
confirmed by direct inspection, not guessed:

  - `@vymalo/opencode-otel` -- a structured, purpose-built governance event
    shape. Every line has `attributes."event.name"` (`opencode.session_start`,
    `.user_prompt`, `.assistant_response`, `.tool_decision`, `.tool_result`,
    `.session_idle`, `.api_error`, `.todo_updated`, `.compaction`,
    `.compaction_autocontinue`), rich `gen_ai.*`/`opencode.*` attributes, and
    `resources` carrying `host.name`, `opencode.project.name`,
    `opencode.directory`, and (only when the working tree is a real git repo
    -- ~48% of lines in the sample pulled) `vcs.*`. This is what every panel
    below reads.
  - A bare `opencode` scope -- OpenCode's own internal trace/debug logging
    (an ACP-client code path, `resources."opencode.client"="acp"`), shaped as
    `body: [message, json-details-string]` with NO `event.name` and NO
    `resources."host.name"`. Not a governance event at all; every query below
    excludes it via the `attributes_event_name=~".+"` filter baked into
    `OPENCODE_EVENTS` -- a line without that attribute simply isn't matched.

Loki's `json` parser flattens the SAME way confirmed in
`generate_ai_cli_dashboard.py`: a nested key's path AND any literal `.` inside
a JSON key name both become `_`. So `attributes."event.name"` ->
`attributes_event_name`, `resources."host.name"` -> `resources_host_name`,
`attributes."gen_ai.usage.input_tokens"` -> `attributes_gen_ai_usage_input_tokens`.
Every field name below was round-tripped through a real query against the
live cluster, exactly like the AI CLI dashboard's own confirmation discipline
-- none of this is read off documentation.

## No user identity -- confirmed absent, not just unused

Unlike Claude Code's `attributes_user_email` (which the AI CLI dashboard
already leans on for its "distinct users" panel), OpenCode's payload carries
NONE of `email`/`user_id`/`display_name` -- checked directly:
`/loki/api/v1/label/email/values` (and `user_id`, `display_name`) scoped to
`{job="opencode"}` all returned empty. RFC-0003's taxonomy table lists this
row's Identity column as "user", but that is aspirational relative to what
the payload actually contains today -- `resources_host_name` (a developer's
machine hostname) is the closest per-developer signal this dashboard has, and
every panel that uses it is labelled as a machine, not a verified identity.
`client.address` also exists but is a raw IP (liable to NAT sharing) and adds
nothing `host.name` doesn't already give more legibly.

## Cost/tokens are self-reported by the OpenCode client, not gateway-verified

RFC-0003's taxonomy table lists this row's cost units as "none emitted" --
true of the *gateway*, but the `@vymalo/opencode-otel` package itself computes
and emits `opencode.cost.usage` (and the four `gen_ai.usage.*_tokens` fields)
client-side, presumably from the model's own reported usage plus a local
price table. Confirmed real, non-zero, per-model-plausible values live. This
is NOT the µUSD, gateway-computed figure ADR-0008/the VS Code LM provider row
has; every cost/token panel below is labelled "self-reported by the client"
so nobody mistakes it for an authoritative billing number.

⚠️ **Cost/token fields appear on TWO event types with OVERLAPPING, not
additive, meaning** -- caught by directly comparing sums, not assumed:
`opencode.assistant_response` carries a PER-REQUEST cost/token figure (one row
per model call); `opencode.session_idle` carries a PER-SESSION CUMULATIVE
figure emitted once when a session goes idle. Over the same trailing 24h on
the live cluster: `sum(assistant_response cost)` = 36.14, `sum(session_idle
cost)` = 35.59 -- nearly identical, because `session_idle`'s number is
approximately the sum of that session's own `assistant_response` costs.
Summing BOTH event types together (as an earlier draft of this query did)
returns ~71.7, roughly double-counting the same spend. Every cost/token expr
below therefore filters to `attributes_event_name="opencode.assistant_response"`
specifically -- `session_idle`'s cumulative figure is deliberately not
queried here, since it duplicates rather than adds to what assistant_response
already reports.

## What is NOT confirmed

  - The Loki datasource UID. Same `__DS_LOKI__` / values.yaml
    `grafanaDashboard.datasources.lokiUid: loki` gap `generate_ai_cli_dashboard.py`
    already documents -- reused verbatim here, not re-guessed, since it's the
    same Grafana instance and the same unconfirmed provisioning path.
  - Whether `opencode.compaction` / `opencode.compaction_autocontinue`
    (23 events each over 7d, confirmed to exist in the event-name breakdown)
    carry any attributes beyond `event.name` and `gen_ai.conversation.id` --
    volume is low enough, and the fields unconfirmed enough, that this
    dashboard does not build panels for them yet.

Usage:
    python3 scripts/generate_opencode_dashboard.py           # regenerate the file
    python3 scripts/generate_opencode_dashboard.py --check   # verify it's current
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts" / "lightbridge-governance" / "dashboards" / "opencode-telemetry.json"

# --------------------------------------------------------------------------
# Datasource contract (charts/lightbridge-governance/values.yaml's
# `grafanaDashboard.datasources.lokiUid` substitutes this literal token at
# `helm template` time -- see templates/grafanadashboard-opencode.yaml). Same
# token, same Loki instance, as generate_ai_cli_dashboard.py already uses.
# --------------------------------------------------------------------------
LOKI_TYPE = "loki"
LOKI_UID = "__DS_LOKI__"
LOKI_DS = {"type": LOKI_TYPE, "uid": LOKI_UID}

# The base log-range selector every panel below builds on: job="opencode",
# JSON-parsed, restricted to lines that actually carry the structured
# `@vymalo/opencode-otel` event shape. See the module docstring's "one job,
# TWO instrumentation scopes" section for why the `attributes_event_name`
# filter is what excludes the bare-`opencode`-scope ACP debug logging, not a
# `instrumentation_scope_name` match (both work; this one reads slightly
# clearer in an exported query and was the one actually verified live).
OPENCODE_EVENTS = '{job="opencode"} | json | attributes_event_name=~".+"'

# Cost/token panels only -- see the module docstring's ⚠️ on double counting.
OPENCODE_RESPONSE_EVENTS = (
    '{job="opencode"} | json | attributes_event_name="opencode.assistant_response"'
)

# Same convention as the other two generators: an empty panel must read as
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
    rather than imported, same reasoning as generate_ai_cli_dashboard.py's
    own copy: these are independent dev-time tools, and importing one from
    another would couple their release cycles for four lines of code."""

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
    thresholds_steps: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """⚠️ `reduce_calc` default is `lastNotNull`, not `sum` -- same trap
    `generate_ai_cli_dashboard.py`'s own `loki_stat_panel` documents: a stat
    panel's `expr` here embeds its OWN window (e.g. a literal `[24h]`
    bracket) and still runs as a RANGE query over the dashboard's time
    range, so Loki returns one already-fully-aggregated sample per step.
    Reducing those with `sum` multiplies the true value by the step count.
    `lastNotNull` reads the one correct number."""
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
    """An instant metric query rendered as a table -- for a snapshot ranking
    (e.g. "top tools", "top hosts by cost") rather than a trend over time.
    Same shape as generate_ai_cli_dashboard.py's own `loki_table_panel`."""
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


def build_dashboard() -> dict[str, Any]:
    ids = Ids()
    panels: list[dict[str, Any]] = []
    y = 0

    # ---------------------------------------------------------------
    # Section 1 -- Volume & adoption. Confirmed live 2026-09-09: 288
    # sessions, 31 distinct hosts, 9+ distinct repos over the trailing 7d.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Volume & adoption (RFC-0003 OpenCode row)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Events per minute, by event type",
            description=(
                f"sum by (attributes_event_name) (count_over_time({OPENCODE_EVENTS} "
                "[$__interval])). tool_result and assistant_response dominate volume; "
                "api_error is broken out on its own in the Reliability section below."
            ),
            expr=f"sum by (attributes_event_name) (count_over_time({OPENCODE_EVENTS} [$__interval]))",
            legend="{{attributes_event_name}}",
            unit="ops",
            grid={"h": 8, "w": 16, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Total events (24h)",
            description=f"sum(count_over_time({OPENCODE_EVENTS} [24h])).",
            expr=f"sum(count_over_time({OPENCODE_EVENTS} [24h]))",
            unit="none",
            grid={"h": 8, "w": 4, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Distinct hosts active (24h)",
            description=(
                "count(count by (resources_host_name) (count_over_time(...[24h]))). "
                "OpenCode's payload carries no user email/id (checked live -- see the "
                "module docstring) -- a machine hostname is the closest per-developer "
                "signal available, NOT a verified identity."
            ),
            expr=f"count(count by (resources_host_name) (count_over_time({OPENCODE_EVENTS} [24h])))",
            unit="none",
            grid={"h": 8, "w": 4, "x": 20, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    panels.append(
        loki_stat_panel(
            ids,
            title="Sessions started (7d)",
            description=(
                'count(count by (attributes_gen_ai_conversation_id) (count_over_time({...} '
                '| attributes_event_name="opencode.session_start" [7d]))).'
            ),
            expr=(
                "count(count by (attributes_gen_ai_conversation_id) (count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.session_start" [7d])))'
            ),
            unit="none",
            grid={"h": 8, "w": 4, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Distinct repos active (7d)",
            description=(
                "count(count by (resources_vcs_repository_name) (...)). Only populated "
                "when the working tree is a real git repo -- ~48% of lines in a live sample "
                "carried no vcs.* resources at all (a bare directory, no repo)."
            ),
            expr=(
                "count(count by (resources_vcs_repository_name) (count_over_time("
                f"{OPENCODE_EVENTS} [7d])))"
            ),
            unit="none",
            grid={"h": 8, "w": 4, "x": 4, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top projects by events (7d)",
            description=(
                "topk(10, sum by (resources_opencode_project_name) (count_over_time(...[7d]"
                "))). `opencode.project.name` is a hashed/anonymized project id when a repo "
                "is present, or the literal string \"global\" for sessions outside any "
                "project directory -- both confirmed live."
            ),
            expr=(
                "topk(10, sum by (resources_opencode_project_name) (count_over_time("
                f"{OPENCODE_EVENTS} [7d])))"
            ),
            unit="none",
            grid={"h": 8, "w": 8, "x": 8, "y": y},
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top hosts by events (24h)",
            description=(
                "topk(10, sum by (resources_host_name) (count_over_time(...[24h]))). Same "
                "'machine, not verified identity' caveat as the stat panel above."
            ),
            expr=(
                "topk(10, sum by (resources_host_name) (count_over_time("
                f"{OPENCODE_EVENTS} [24h])))"
            ),
            unit="none",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 2 -- Cost & tokens (opencode.assistant_response). Self-reported
    # by the OpenCode client, NOT gateway-verified -- see the module
    # docstring's dedicated section for why, and for why every expr here
    # filters to assistant_response specifically (session_idle carries an
    # overlapping, not additive, per-session cumulative figure).
    # ---------------------------------------------------------------
    panels.append(row(ids, "Cost & tokens -- self-reported by the client (opencode.assistant_response)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Cost, by model",
            description=(
                "sum by (attributes_gen_ai_request_model) (sum_over_time({...} | "
                "unwrap attributes_opencode_cost_usage [$__interval])). Confirmed live: "
                "real, non-zero, per-model-plausible USD values. This is the OpenCode "
                "client's own cost estimate, NOT a gateway-computed µUSD figure (ADR-0008 "
                "doesn't apply here -- nothing here is stored, this is a read-only Loki "
                "query over the client's self-reported number)."
            ),
            expr=(
                "sum by (attributes_gen_ai_request_model) (sum_over_time("
                f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_opencode_cost_usage [$__interval]))"
            ),
            legend="{{attributes_gen_ai_request_model}}",
            unit="currencyUSD",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Total cost (24h, self-reported)",
            description=(
                "sum(sum_over_time({...} | unwrap attributes_opencode_cost_usage [24h])), "
                "assistant_response only."
            ),
            expr=f"sum(sum_over_time({OPENCODE_RESPONSE_EVENTS} | unwrap attributes_opencode_cost_usage [24h]))",
            unit="currencyUSD",
            grid={"h": 8, "w": 4, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Cost by host (24h, self-reported)",
            description=(
                "topk(10, sum by (resources_host_name) (sum_over_time({...} | unwrap "
                "attributes_opencode_cost_usage [24h]))). Host, not verified identity -- "
                "see Section 1."
            ),
            expr=(
                "topk(10, sum by (resources_host_name) (sum_over_time("
                f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_opencode_cost_usage [24h])))"
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
                "input/output/reasoning/cache-read/cache-write tokens, each "
                "sum_over_time(... | unwrap attributes_gen_ai_usage_<kind>_tokens "
                "[$__interval]), assistant_response only. All five fields confirmed live."
            ),
            expr=(
                "sum(sum_over_time("
                f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_gen_ai_usage_input_tokens [$__interval]))"
            ),
            legend="input",
            unit="none",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    # The remaining four token kinds share the same panel shape; built as
    # separate targets on ONE panel (same technique
    # generate_ai_cli_dashboard.py uses for its own token panel) rather than
    # five panels -- a stacked comparison is the point.
    panels[-1]["targets"].extend(
        [
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_gen_ai_usage_output_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "output",
                "refId": "B",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_gen_ai_usage_reasoning_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "reasoning",
                "refId": "C",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_gen_ai_usage_cache_read_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cache read",
                "refId": "D",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_gen_ai_usage_cache_write_tokens [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cache write",
                "refId": "E",
            },
        ]
    )
    panels.append(
        loki_timeseries_panel(
            ids,
            title="Response duration (avg duration_ms)",
            description=(
                "avg_over_time({...} | unwrap attributes_opencode_response_duration_ms "
                "[$__interval]). The model-response latency OpenCode itself measured, not "
                "this collector's own forwarding time."
            ),
            expr=(
                "avg(avg_over_time("
                f"{OPENCODE_RESPONSE_EVENTS} | unwrap attributes_opencode_response_duration_ms [$__interval]))"
            ),
            legend="avg duration",
            unit="ms",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 3 -- Tool activity (opencode.tool_result / .tool_decision).
    # What the agent is actually doing.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Tool activity (opencode.tool_result / .tool_decision)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tool results, ok vs error",
            description=(
                'sum by (attributes_opencode_tool_status) (count_over_time({...} | '
                'attributes_event_name="opencode.tool_result" [$__interval])). Confirmed '
                'live values: "ok" and "error" -- a rising error share is the signal worth '
                "a human's attention, independent of raw tool volume."
            ),
            expr=(
                "sum by (attributes_opencode_tool_status) (count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.tool_result" [$__interval]))'
            ),
            legend="{{attributes_opencode_tool_status}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top tools (24h)",
            description=(
                'topk(10, sum by (attributes_gen_ai_tool_name) (count_over_time({...} | '
                'attributes_event_name="opencode.tool_result" [24h]))). Confirmed live '
                "values: bash, edit, read, write, grep, glob, task, todowrite, webfetch."
            ),
            expr=(
                "topk(10, sum by (attributes_gen_ai_tool_name) (count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.tool_result" [24h])))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tool duration (avg ms), by tool",
            description=(
                "avg by (attributes_gen_ai_tool_name) (avg_over_time({...} | unwrap "
                "attributes_opencode_tool_duration_ms [$__interval])). tool_result only."
            ),
            expr=(
                "avg by (attributes_gen_ai_tool_name) (avg_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.tool_result" '
                "| unwrap attributes_opencode_tool_duration_ms [$__interval]))"
            ),
            legend="{{attributes_gen_ai_tool_name}}",
            unit="ms",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Tool calls awaiting/needing permission (24h)",
            description=(
                'count_over_time({...} | attributes_event_name="opencode.tool_decision" '
                '[24h]). `opencode.permission.source` is confirmed live as always "user" in '
                "the sample pulled -- `opencode.permission.decision` was null on every line "
                "seen, so this counts the decision EVENT (a permission gate was hit), not "
                "yet a real accept/reject split. Extend once a real non-null decision value "
                "has been observed."
            ),
            expr=(
                "sum(count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.tool_decision" [24h]))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 4 -- Reliability (opencode.api_error). NOT in the AI CLI
    # dashboard's own shape -- added here because live volume warrants it:
    # confirmed 1248 api_error events against 21604 assistant_response over
    # the same trailing 7d on the live cluster, ~5.8%, not noise.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Reliability (opencode.api_error)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="API errors, by error type",
            description=(
                'sum by (attributes_error_type) (count_over_time({...} | '
                'attributes_event_name="opencode.api_error" [$__interval])). Confirmed live '
                "values: APIError, MessageAbortedError, UnknownError, retry."
            ),
            expr=(
                "sum by (attributes_error_type) (count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.api_error" [$__interval]))'
            ),
            legend="{{attributes_error_type}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Error rate (24h)",
            description=(
                "api_error count / (api_error count + assistant_response count) over the "
                "trailing 24h, both restricted to the same OPENCODE_EVENTS filter. An "
                "approximation of 'what share of model interactions hit an error', not a "
                "precise per-request rate (api_error and assistant_response are separate "
                "events on the same conversation, not a 1:1 request/outcome pair)."
            ),
            expr=(
                "sum(count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.api_error" [24h])) '
                "/ ("
                "sum(count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.api_error" [24h])) '
                "+ sum(count_over_time("
                f"{OPENCODE_RESPONSE_EVENTS} [24h])))"
            ),
            unit="percentunit",
            grid={"h": 8, "w": 4, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[
                {"color": "green", "value": None},
                {"color": "orange", "value": 0.05},
                {"color": "red", "value": 0.15},
            ],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top hosts by errors (24h)",
            description=(
                "topk(10, sum by (resources_host_name) (count_over_time({...} | "
                'attributes_event_name="opencode.api_error" [24h]))). A host repeatedly '
                "hitting errors is the operational lead worth following up, not the "
                "aggregate rate above."
            ),
            expr=(
                "topk(10, sum by (resources_host_name) (count_over_time("
                '{job="opencode"} | json | attributes_event_name="opencode.api_error" [24h])))'
            ),
            unit="none",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
        )
    )
    y += 8

    dashboard: dict[str, Any] = {
        "id": None,
        "uid": "governance-opencode-telemetry",
        "title": "OpenCode telemetry",
        "description": (
            "Volume, cost, tokens, tool activity and reliability for OpenCode "
            "(RFC-0003's OpenCode row), sourced from the opencode-otel-collector's forwarded "
            "OTLP logs in Loki. Generated by scripts/generate_opencode_dashboard.py -- do not "
            "hand-edit; regenerate instead."
        ),
        "tags": ["governance", "opencode"],
        "style": "dark",
        "timezone": "browser",
        "editable": True,
        "graphTooltip": 1,
        "schemaVersion": 39,
        "version": 1,
        "refresh": "1m",
        # 7d, matching ai-cli-telemetry's own choice: high enough volume on
        # this job that 24h would also read fine, but 7d keeps the two
        # sibling dashboards' default window consistent for anyone flipping
        # between them.
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
