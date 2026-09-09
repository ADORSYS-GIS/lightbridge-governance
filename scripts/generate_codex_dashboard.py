#!/usr/bin/env python3
"""Generate the Codex telemetry Grafana dashboard JSON.

DEV-TIME TOOL ONLY, same convention as `generate_dashboards.py` and
`generate_ai_cli_dashboard.py`/`generate_opencode_dashboard.py` (see the
first's own docstring for the full rationale). Standard library only. The
**generated JSON is committed** at
`charts/lightbridge-governance/dashboards/codex-telemetry.json`; that file,
not this script, is what `helm template`/the Grafana Operator actually reads.

Determinism contract: identical to the other generators -- no timestamps, no
`random`, no `uuid`, no hash-seed-dependent ordering, panel ids from a plain
counter over a fixed call sequence.

## Why Loki, not this repo's own `executions`/`model_calls`/`tool_calls`

This repo has a `CodexNormalizer` (`crates/governance-foundry/src/normalizer/
codex.rs`) that turns OTLP spans into `executions`/`model_calls`/`tool_calls`
rows with a real, gateway-computed integer-µUSD cost -- a much richer
Postgres story than anything below. It is NOT used here, for two confirmed
reasons, not a guess:

  1. Its only HTTP entry point, `/internal/v1/ingest`, was deleted in #243
     (2026-09-04): "Nothing has ever called it -- not in the running cluster
     and not in the committed chart." The normalizer and tables exist in the
     schema; nothing in production ever wrote to them via this path.
  2. ADR-0014 (2026-08-31, Accepted) decommissions this repo's own usage
     telemetry store outright: "All AI-usage telemetry ... lands in the
     `lightbridge-authz-usage` database" instead, and "the dashboards read
     that surface" -- a different repo's store this chart has no
     datasource for today.

So the ONE surface with real, live Codex data as of this writing is the same
Loki instance `generate_ai_cli_dashboard.py`/`generate_opencode_dashboard.py`
already use, fed by the SAME `aiCliOtel` public collector Claude Code uses
(`governance.source: "ai-cli"`, confirmed on every sampled line) --
`docs/integrations/codex-telemetry-rollout.md`'s `otel.ai.camer.digital`
target, not a dedicated Codex collector.

## Confirmed live 2026-09-09 (kubectl port-forward svc/loki-gateway, real
## developers' real Codex sessions, not documentation)

  - TWO job label values, `codex-app-server` and `codex_cli_rs`, carry the
    IDENTICAL event shape (same `codex_otel.*` instrumentation scopes, only
    `service.version` differs slightly between the desktop app and the CLI
    build) -- every query below matches both via `job=~"codex-app-server|
    codex_cli_rs"`.
  - Eleven confirmed `event.name` values, roughly by volume over 7d:
    `codex.tool_result`, `codex.websocket_request`, `codex.api_request`,
    `codex.tool_decision`, `codex.startup_phase`, `codex.sse_event`,
    `codex.auth_recovery`, `codex.user_prompt`, `codex.conversation_starts`,
    `codex.agent_communication`, `codex.websocket_connect`,
    `codex.turn_ttft`, `codex.sandbox_outcome`.
  - Loki's `json` parser flattens the same way already confirmed in the
    other two generators: a nested key's path AND a literal `.` inside a
    JSON key name both become `_`. So `attributes."user.email"` ->
    `attributes_user_email`, `attributes."auth.mode"` -> `attributes_auth_mode`
    -- which, non-obviously, COLLIDES with the separate, already-flat key
    `attributes."auth_mode"` some other event types use (both mean "how this
    session authenticated"; they never co-occur on the same line, so the
    collision is harmless and, in fact, convenient -- one label reads both
    upstream spellings).

## Real per-engineer identity -- unlike OpenCode's dashboard

Codex's payload carries `user.email` and `user.account_id` directly
(confirmed live, real adorsys.com addresses) -- a genuine improvement over
`generate_opencode_dashboard.py`'s host-name-only proxy. The caveat, straight
from `docs/integrations/codex-telemetry-rollout.md` and confirmed live (every
sampled line so far has `auth_mode: "Chatgpt"`, never `"ApiKey"`): **identity
is populated only under ChatGPT sign-in**; a developer using Codex with an
API key is invisible to every `attributes_user_email` panel below, not
merely under-counted to zero -- there is no row to filter out, the
developer's traffic simply carries no email at all. Every such panel is
described accordingly.

## ⚠️ The most important finding: "approved" is mostly not a human decision

RFC-0003 and this dashboard's whole premise treat `codex.tool_decision`'s
`decision` field as the "does the engineer accept what the agent did"
signal. Checked directly, not assumed: `codex.tool_decision` also carries a
`source` attribute, and cross-tabulating `decision` by `source` over a live
7d window gives:

    decision                 source              count
    approved                 Config                713
    approved                 AutomatedReviewer        6
    approved_with_amendment  User                    14

**719 of 733 decisions (98%) were auto-approved by sandbox/approval policy
(`source=Config`) or a review bot (`source=AutomatedReviewer`), not a human
clicking accept.** Only `source="User"` is an actual engineer decision, and
in this sample every one of those was `approved_with_amendment` (accepted,
but only after the engineer amended it) -- not enough volume to call a rate,
but real. A dashboard that reported "98% approval rate" from the `decision`
field alone, without this split, would be reporting the sandbox's
`approval_policy` setting (seen live as `"never"` -- meaning "never ask a
human" -- on most sessions in `codex.conversation_starts`), not engineer
behavior. Every panel below that touches `decision` therefore also breaks
out `source`, and there is a dedicated "human-reviewed only" panel filtered
to `source="User"` so the two are never conflated.

## What Codex's telemetry does NOT carry -- confirmed absent, not omitted

  - **Lines of code.** No `lines`/`diff`/`patch`/`edit`/`file`-shaped
    attribute exists anywhere in the full confirmed vocabulary above (grepped
    across a live 2000-line/7d sample spanning all eleven event types).
    Unlike VS Code Copilot Chat's `copilot_chat_lines_of_code_count_total`
    (Mimir), Codex's own OTel emission has nothing comparable. The closest
    available proxy is `codex.tool_result` count where
    `attributes_tool_name="apply_patch"` -- a COUNT OF PATCH-APPLY CALLS, not
    a line count -- and every panel using it is labelled as exactly that.
  - **Cost.** RFC-0003's taxonomy table lists this row's cost units as "none
    emitted", and nothing in the confirmed vocabulary contradicts that (no
    `cost`/`price`/`usd` field anywhere). Token counts ARE emitted (see
    below) and are shown; no cost figure is fabricated from them, since this
    dashboard has no trustworthy per-model Codex pricing source to multiply
    by (the one that exists, `model_pricing`, sits behind the same
    decommissioned `/internal/v1/ingest` path described above).

## Tokens -- confirmed real, all six kinds, one event

Every `codex.sse_event` with `event.kind="response.completed"` carries six
token-count fields together: `input_token_count`, `output_token_count`,
`cached_token_count`, `reasoning_token_count`, `tool_token_count` and
`cache_write_token_count`. Confirmed live over a real 24h window: input
~24.7M, cached ~23.2M (heavy prompt-cache reuse), tool ~24.8M (tool output
consumes roughly as many tokens as the prompt itself), output ~93.7K,
reasoning ~33K, cache-write 0 in the sampled window. This matches
`docs/integrations/codex-telemetry-rollout.md`'s note that exec-mode token
counts live on span/log attributes, not the `codex.turn.token_usage` metric
(upstream bug openai/codex#33668) -- consistent with what a Loki-only
dashboard can see.

## What is NOT confirmed

  - The Loki datasource UID -- same `__DS_LOKI__` / values.yaml
    `grafanaDashboard.datasources.lokiUid: loki` gap the other two Loki-backed
    generators already document. Reused verbatim, not re-guessed.
  - `codex.agent_communication` / `codex.startup_phase` / `codex.
    websocket_connect` -- real, confirmed event names with real volume, but
    not built into a panel here: their fields (`communication_id`, `state`,
    `startup.phase`) describe internal client plumbing, not an engineer's
    development process, which is this dashboard's stated purpose.

Usage:
    python3 scripts/generate_codex_dashboard.py           # regenerate the file
    python3 scripts/generate_codex_dashboard.py --check   # verify it's current
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts" / "lightbridge-governance" / "dashboards" / "codex-telemetry.json"

# --------------------------------------------------------------------------
# Datasource contract (charts/lightbridge-governance/values.yaml's
# `grafanaDashboard.datasources.lokiUid` substitutes this literal token at
# `helm template` time -- see templates/grafanadashboard-codex.yaml). Same
# token, same Loki instance, as the other two Loki-backed generators.
# --------------------------------------------------------------------------
LOKI_TYPE = "loki"
LOKI_UID = "__DS_LOKI__"
LOKI_DS = {"type": LOKI_TYPE, "uid": LOKI_UID}

# Both job label values carry the identical event shape -- see the module
# docstring. Every panel below builds its own event-name filter on top of
# this base matcher.
CODEX_JOB = '{job=~"codex-app-server|codex_cli_rs"}'

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
    rather than imported, same reasoning as the other two generators: these
    are independent dev-time tools, and importing one from another would
    couple their release cycles for four lines of code."""

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
    """⚠️ `reduce_calc` default is `lastNotNull`, not `sum` -- same trap the
    other two Loki-backed generators document: a stat panel's `expr` here
    embeds its OWN window (e.g. a literal `[24h]` bracket) and still runs as
    a RANGE query over the dashboard's time range, so Loki returns one
    already-fully-aggregated sample per step. Reducing those with `sum`
    multiplies the true value by the step count. `lastNotNull` reads the one
    correct number."""
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
    or breakdown rather than a trend over time. Same shape as the other two
    generators' own `loki_table_panel`."""
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
    # Section 1 -- Volume & adoption. Confirmed live 2026-09-09: 85
    # conversations, 5 distinct hosts, 2 distinct ChatGPT-signed-in
    # engineers over the trailing 7d (a small pilot group, not a bug).
    # ---------------------------------------------------------------
    panels.append(row(ids, "Volume & adoption (RFC-0003 Codex row)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Events per minute, by event type",
            description=(
                f"sum by (attributes_event_name) (count_over_time({CODEX_JOB} | json | "
                'attributes_event_name=~".+" [$__interval])). tool_result, websocket_request '
                "and api_request dominate volume -- both job label values "
                "(codex-app-server, codex_cli_rs) share the identical event shape and are "
                "matched together."
            ),
            expr=(
                f"sum by (attributes_event_name) (count_over_time({CODEX_JOB} | json | "
                'attributes_event_name=~".+" [$__interval]))'
            ),
            legend="{{attributes_event_name}}",
            unit="ops",
            grid={"h": 8, "w": 14, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Total events (24h)",
            description=f'sum(count_over_time({CODEX_JOB} | json | attributes_event_name=~".+" [24h])).',
            expr=f'sum(count_over_time({CODEX_JOB} | json | attributes_event_name=~".+" [24h]))',
            unit="none",
            grid={"h": 8, "w": 5, "x": 14, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Engineers active (24h, ChatGPT sign-in only)",
            description=(
                "count(count by (attributes_user_email) (count_over_time(...[24h]))). "
                "user.email is populated ONLY under ChatGPT sign-in (confirmed live -- every "
                "sampled line carries auth_mode=Chatgpt); a developer on API-key auth is "
                "invisible here, not counted as zero -- there is no row to see at all."
            ),
            expr=(
                "count(count by (attributes_user_email) (count_over_time("
                f'{CODEX_JOB} | json | attributes_user_email=~".+" [24h])))'
            ),
            unit="none",
            grid={"h": 8, "w": 5, "x": 19, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    panels.append(
        loki_stat_panel(
            ids,
            title="Conversations started (7d)",
            description=(
                "count(count by (attributes_conversation_id) (count_over_time({...} | "
                'attributes_event_name="codex.conversation_starts" [7d]))).'
            ),
            expr=(
                "count(count by (attributes_conversation_id) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.conversation_starts" [7d])))'
            ),
            unit="none",
            grid={"h": 8, "w": 4, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Distinct hosts active (7d)",
            description=(
                "count(count by (resources_host_name) (count_over_time(...[7d]))). Available "
                "regardless of sign-in method, unlike the email-based panels above."
            ),
            expr=(
                "count(count by (resources_host_name) (count_over_time("
                f"{CODEX_JOB} | json [7d])))"
            ),
            unit="none",
            grid={"h": 8, "w": 4, "x": 4, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top engineers by events (24h, ChatGPT sign-in only)",
            description=(
                "topk(10, sum by (attributes_user_email) (count_over_time(...[24h]))). Same "
                "ChatGPT-sign-in caveat as the stat panel above."
            ),
            expr=(
                "topk(10, sum by (attributes_user_email) (count_over_time("
                f'{CODEX_JOB} | json | attributes_user_email=~".+" [24h])))'
            ),
            unit="none",
            grid={"h": 8, "w": 8, "x": 8, "y": y},
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top hosts by events (7d)",
            description="topk(10, sum by (resources_host_name) (count_over_time(...[7d]))).",
            expr=f"topk(10, sum by (resources_host_name) (count_over_time({CODEX_JOB} | json [7d])))",
            unit="none",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 2 -- Tool activity & approvals. See the module docstring's ⚠️
    # section -- "approved" is 98% policy auto-approval, not a human
    # decision, and every panel here makes that split explicit rather than
    # reporting a single misleading "acceptance rate".
    # ---------------------------------------------------------------
    panels.append(
        row(
            ids,
            "Tool activity & approvals -- decision vs source (codex.tool_decision)",
            y,
        )
    )
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tool call decisions, by decision AND source",
            description=(
                "sum by (attributes_decision, attributes_source) (count_over_time({...} | "
                'attributes_event_name="codex.tool_decision" [$__interval])). `source` is the '
                "load-bearing dimension: Config/AutomatedReviewer are policy/bot auto-approval, "
                "NOT an engineer's decision. Confirmed live over 7d: 713 approved/Config, 6 "
                "approved/AutomatedReviewer, 14 approved_with_amendment/User -- 98% of "
                "'approved' never reached a human."
            ),
            expr=(
                "sum by (attributes_decision, attributes_source) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.tool_decision" [$__interval]))'
            ),
            legend="{{attributes_decision}} / {{attributes_source}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Human-reviewed decisions only (source=User, 7d)",
            description=(
                'sum by (attributes_decision) (count_over_time({...} | attributes_source="User" '
                "[7d])), restricted to source=User. THIS is the actual rate at which an "
                "engineer accepted/amended/rejected what the agent proposed -- everything else "
                "in this dashboard's `decision` field is a policy default, not a person."
            ),
            expr=(
                "sum by (attributes_decision) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.tool_decision" '
                '| attributes_source="User" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        loki_stat_panel(
            ids,
            title="Auto-approved share of all decisions (7d)",
            description=(
                "(count with source != User) / (count total), both restricted to "
                "codex.tool_decision over 7d. High is expected when approval_policy=never "
                "(seen live in codex.conversation_starts) -- this stat exists so nobody reads "
                "the decision breakdown above as pure engineer behaviour."
            ),
            expr=(
                "sum(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.tool_decision" '
                '| attributes_source!="User" [7d])) '
                "/ sum(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.tool_decision" [7d]))'
            ),
            unit="percentunit",
            grid={"h": 8, "w": 6, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Tools invoked (7d)",
            description=(
                "sum by (attributes_tool_name) (count_over_time({...} | "
                'attributes_event_name="codex.tool_decision" [7d])). Confirmed live values: '
                "apply_patch (the code-editing tool) and exec_command."
            ),
            expr=(
                "sum by (attributes_tool_name) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.tool_decision" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 6, "x": 6, "y": y},
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Patch applications (7d) -- proxy for code edits, NOT lines changed",
            description=(
                'sum(count_over_time({...} | attributes_event_name="codex.tool_result" | '
                'attributes_tool_name="apply_patch" [7d])). Codex\'s OTel emission carries no '
                "lines-added/removed field at all (confirmed absent across the full sampled "
                "vocabulary) -- this counts apply_patch CALLS, the closest available signal for "
                "\"how much code the agent changed\", not a line count."
            ),
            expr=(
                "sum(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.tool_result" '
                '| attributes_tool_name="apply_patch" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 6, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Sandbox denials, by outcome (7d)",
            description=(
                "sum by (attributes_outcome) (count_over_time({...} | "
                'attributes_event_name="codex.sandbox_outcome" [7d])). A DIFFERENT gate than '
                "tool_decision above -- the sandbox blocking a call outright (confirmed live "
                'value: "denied" on exec_command), not an approval policy allowing or refusing '
                "it. Low volume (3 in the confirmed sample) but a real signal, not noise."
            ),
            expr=(
                "sum by (attributes_outcome) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.sandbox_outcome" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 6, "x": 18, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 3 -- Tokens (self-reported by Codex; NO cost -- RFC-0003
    # confirms "none emitted" and nothing in the confirmed vocabulary
    # contradicts that). See the module docstring for the full token
    # breakdown confirmed live.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Tokens -- codex.sse_event (no cost figure: RFC-0003 confirms none emitted)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Tokens, by kind",
            description=(
                "input/output/cached/reasoning/tool/cache-write tokens, each "
                "sum_over_time(... | unwrap attributes_<kind>_token_count [$__interval]), all "
                'from codex.sse_event lines with event.kind="response.completed". All six '
                "fields confirmed live together on the same event."
            ),
            expr=(
                "sum(sum_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                "| unwrap attributes_input_token_count [$__interval]))"
            ),
            legend="input",
            unit="none",
            grid={"h": 8, "w": 14, "x": 0, "y": y},
        )
    )
    panels[-1]["targets"].extend(
        [
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                    "| unwrap attributes_output_token_count [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "output",
                "refId": "B",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                    "| unwrap attributes_cached_token_count [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cached",
                "refId": "C",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                    "| unwrap attributes_reasoning_token_count [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "reasoning",
                "refId": "D",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                    "| unwrap attributes_tool_token_count [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "tool",
                "refId": "E",
            },
            {
                "datasource": LOKI_DS,
                "expr": (
                    "sum(sum_over_time("
                    f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                    "| unwrap attributes_cache_write_token_count [$__interval]))"
                ),
                "queryType": "range",
                "legendFormat": "cache write",
                "refId": "F",
            },
        ]
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Avg time to first token (24h)",
            description=(
                'avg(avg_over_time({...} | attributes_event_name="codex.turn_ttft" | unwrap '
                "attributes_duration_ms [24h])). Confirmed live: ~5.2s average in a real 24h "
                "window."
            ),
            expr=(
                "avg(avg_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.turn_ttft" '
                "| unwrap attributes_duration_ms [24h]))"
            ),
            unit="ms",
            grid={"h": 8, "w": 10, "x": 14, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    panels.append(
        loki_timeseries_panel(
            ids,
            title="Avg prompt length (chars), by model",
            description=(
                "avg by (attributes_model) (avg_over_time({...} | "
                'attributes_event_name="codex.user_prompt" | unwrap attributes_prompt_length '
                "[$__interval])). Length only -- log_user_prompt is false by policy "
                "(codex-telemetry-rollout.md), so prompt CONTENT is never captured here or "
                "anywhere in this pipeline."
            ),
            expr=(
                "avg by (attributes_model) (avg_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.user_prompt" '
                "| unwrap attributes_prompt_length [$__interval]))"
            ),
            legend="{{attributes_model}}",
            unit="short",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Session config: reasoning effort x approval policy (7d)",
            description=(
                "sum by (attributes_reasoning_effort, attributes_approval_policy) "
                '(count_over_time({...} | attributes_event_name="codex.conversation_starts" '
                "[7d])). Context for the approvals section above: "
                'approval_policy="never" means no human is ever asked, at the sandbox\'s own '
                "config level, independent of anything an engineer did in that session."
            ),
            expr=(
                "sum by (attributes_reasoning_effort, attributes_approval_policy) "
                "(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.conversation_starts" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 4 -- Reliability (codex.api_request / .websocket_request /
    # .auth_recovery / .sse_event). What breaks, not what the agent did.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Reliability (codex.api_request / .auth_recovery / .sse_event)", y))
    y += 1

    panels.append(
        loki_timeseries_panel(
            ids,
            title="API/websocket requests, success vs failure",
            description=(
                "sum by (attributes_success) (count_over_time({...} | "
                'attributes_event_name=~"codex.api_request|codex.websocket_request" '
                "[$__interval])). Confirmed live: overwhelmingly true (408/409 in a real 24h "
                "sample) -- a rising false share is the signal worth attention."
            ),
            expr=(
                "sum by (attributes_success) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name=~"codex.api_request|codex.websocket_request" '
                "[$__interval]))"
            ),
            legend="success={{attributes_success}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        loki_timeseries_panel(
            ids,
            title="Auth recovery attempts, by outcome",
            description=(
                "sum by (attributes_auth_outcome) (count_over_time({...} | "
                'attributes_event_name="codex.auth_recovery" [$__interval])). Confirmed live '
                'values: recovery_failed_transient, recovery_not_run -- a token-refresh health '
                "signal Codex emits on its own, independent of any request outcome above."
            ),
            expr=(
                "sum by (attributes_auth_outcome) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.auth_recovery" [$__interval]))'
            ),
            legend="{{attributes_auth_outcome}}",
            unit="ops",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        loki_stat_panel(
            ids,
            title="SSE stream errors (7d)",
            description=(
                'sum(count_over_time({...} | attributes_event_name="codex.sse_event" | '
                'attributes_error_message=~".+" [7d])). Confirmed live example: "stream '
                'disconnected before completion: websocket closed by server before '
                'response.completed".'
            ),
            expr=(
                "sum(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.sse_event" '
                '| attributes_error_message=~".+" [7d]))'
            ),
            unit="none",
            grid={"h": 8, "w": 6, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        loki_stat_panel(
            ids,
            title="Auth recovery failure rate (7d)",
            description=(
                'recovery_failed_transient count / total codex.auth_recovery count, over 7d. '
                "Confirmed live: 222 failed_transient of 269 total in the sampled window."
            ),
            expr=(
                "sum(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.auth_recovery" '
                '| attributes_auth_outcome="recovery_failed_transient" [7d])) '
                "/ sum(count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.auth_recovery" [7d]))'
            ),
            unit="percentunit",
            grid={"h": 8, "w": 6, "x": 6, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[
                {"color": "green", "value": None},
                {"color": "orange", "value": 0.3},
                {"color": "red", "value": 0.6},
            ],
        )
    )
    panels.append(
        loki_table_panel(
            ids,
            title="Top hosts by auth recovery failures (7d)",
            description=(
                "topk(10, sum by (resources_host_name) (count_over_time({...} | "
                'attributes_auth_outcome="recovery_failed_transient" [7d]))). A host stuck in '
                "repeated auth recovery is the operational lead worth following up."
            ),
            expr=(
                "topk(10, sum by (resources_host_name) (count_over_time("
                f'{CODEX_JOB} | json | attributes_event_name="codex.auth_recovery" '
                '| attributes_auth_outcome="recovery_failed_transient" [7d])))'
            ),
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    dashboard: dict[str, Any] = {
        "id": None,
        "uid": "governance-codex-telemetry",
        "title": "Codex telemetry",
        "description": (
            "Volume, real per-engineer identity, tool/approval activity (with the "
            "human-vs-policy-approval split made explicit), tokens and reliability for OpenAI "
            "Codex (RFC-0003's Codex row), sourced from the aiCliOtel collector's forwarded "
            "OTLP logs in Loki -- NOT this repo's own executions/model_calls/tool_calls tables "
            "(decommissioned per ADR-0014, and their only write path was never live -- see #243). "
            "Generated by scripts/generate_codex_dashboard.py -- do not hand-edit; regenerate "
            "instead."
        ),
        "tags": ["governance", "codex"],
        "style": "dark",
        "timezone": "browser",
        "editable": True,
        "graphTooltip": 1,
        "schemaVersion": 39,
        "version": 1,
        "refresh": "1m",
        # 7d, matching the other two Loki-backed dashboards' own choice, for
        # the same reason: keeps the sibling dashboards' default window
        # consistent for anyone flipping between them.
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
