#!/usr/bin/env python3
"""Generate the Codex user/session dashboard from verified Loki event metadata.

Source semantics and live validation: docs/integrations/codex-dashboard.md.
Snapshots are instant queries (#318); trends use bounded rolling windows.
Run with --check to verify the committed JSON without modifying it.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from dashboard_common import dashboard_shell

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts/lightbridge-governance/dashboards/codex-telemetry.json"
LOKI_TYPE = "loki"
LOKI_UID = "__DS_LOKI__"
LOKI_DS = {"type": LOKI_TYPE, "uid": LOKI_UID}
CODEX_JOB = '{job=~"codex-app-server|codex_cli_rs"}'
UID = "governance-codex-telemetry"
NO_DATA_MAPPING = {"type": "special", "options": {"match": "null+nan",
    "result": {"text": "NO DATA", "index": 0}}}

# Extract only dashboard metadata, never prompts, tool arguments or outputs.
FIELDS = {"event": "event.name", "user": "user.email", "session": "conversation.id",
          "model": "model", "kind": "event.kind", "input_tokens": "input_token_count",
          "output_tokens": "output_token_count", "cached_tokens": "cached_token_count",
          "ttft_ms": "ttft_ms", "duration_ms": "duration_ms"}
PARSER = " | json " + ", ".join(f"{alias}=" + json.dumps('attributes["' + key + '"]')
                                      for alias, key in FIELDS.items())


def logs(event: str = "", *, scoped: bool = True) -> str:
    q = CODEX_JOB
    if event:
        q += ' |= ' + json.dumps(event)
    q += PARSER + ' | __error__=""'
    if event:
        q += ' | event=' + json.dumps(event)
    if scoped:
        # Textbox variables are escaped as regex literals. Empty search matches
        # everyone, including absent email; data links populate these same fields.
        q += ' | user=~`.*${user:regex}.*` | session=~`.*${session:regex}.*`'
    return q


def completed() -> str:
    return logs("codex.sse_event") + ' | kind="response.completed"'


def meaningful() -> str:
    return (
        logs() + ' | event=~"codex.user_prompt|codex.tool_result|codex.sse_event"'
        + ' | (event!="codex.sse_event" or kind="response.completed")'
    )


def count(q: str, window: str = "$__range", group: str = "") -> str:
    by = f" by ({group})" if group else ""
    return f"sum{by}(count_over_time({q}[{window}]))"


def total(field: str, *, group: str = "") -> str:
    by = f" by ({group})" if group else ""
    q = completed() + (' | session!=""' if group == "session" else "")
    return f'sum{by}(sum_over_time({q} | {field}!="" | unwrap {field} | __error__=""[$__range]))'


def latency(event: str, field: str, percentile: float, window: str = "$__range", group: str = "") -> str:
    q = completed() if event == "codex.sse_event" else logs(event)
    if group == "session":
        q += ' | session!=""'
    by = f"({group})" if group else "()"
    return f'quantile_over_time({percentile}, {q} | {field}!="" | unwrap {field} | __error__=""[{window}]) by {by}'


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
    """Return a single snapshot, matching the #318 Loki OOM fix in the
    AI CLI and OpenCode generators. Each expression embeds the selected window
    (`[$__range]`); a range query needlessly recomputes that whole
    window at every dashboard step. With no sparkline (`graphMode: none`),
    an instant query supplies the same final value without that amplification.
    Keep `lastNotNull` as the reducer: summing already-aggregated samples
    would multiply the value if a range query were ever reintroduced.
    """
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
                "queryType": "instant",
                "instant": True,
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


def target(expr: str, ref: str = "A", legend: str = "__auto", *, instant: bool = True) -> dict[str, Any]:
    result = {"datasource": LOKI_DS, "expr": expr, "refId": ref,
              "legendFormat": legend, "queryType": "instant" if instant else "range"}
    if instant:
        result.update(instant=True)
    return result


def build_dashboard() -> dict[str, Any]:
    ids = Ids()
    panels: list[dict[str, Any]] = []
    panels.append({"id": ids.take(), "type": "text", "title": "Your Codex activity",
        "gridPos": {"x": 0, "y": 0, "w": 24, "h": 3}, "options": {"mode": "markdown", "content":
        "Search by email or click a session. Clear searches for everyone. "
        "Session times are observed bounds; identity coverage is fleet-wide."}})
    sessions = 'count(' + count(meaningful() + ' | session!=""', group="session") + ')'
    fleet = (
        logs(scoped=False) + ' | event=~"codex.user_prompt|codex.tool_result|codex.sse_event"'
        + ' | (event!="codex.sse_event" or kind="response.completed")'
    )
    attributed = count(fleet + ' | user!=""')
    coverage = f"100 * ({attributed} or vector(0)) / {count(fleet)}"
    stats = [
        ("Active sessions", sessions, "short", "Distinct conversations with meaningful events in the selected period; not session launches."),
        ("Prompts", count(logs("codex.user_prompt")), "short", "Observed prompt events; prompts may include system-generated follow-ups, not only human-typed messages."),
        ("Input tokens", total("input_tokens"), "short", "Reported input tokens on completed responses, including cached input."),
        ("Output tokens", total("output_tokens"), "short", "Reported output tokens on completed responses. Reasoning is not added again."),
        ("Cached input share", f'100 * {total("cached_tokens")} / {total("input_tokens")}', "percent", "Cached input divided by input tokens. Missing data is not zero."),
        ("Turn wait · median", latency("codex.turn_ttft", "duration_ms", .5), "ms", "Time to first response at turn level; source codex.turn_ttft. Not total session waiting."),
        ("Turn wait · p95", latency("codex.turn_ttft", "duration_ms", .95), "ms", "95th percentile of observed turn-level time to first response."),
        ("Identity coverage · fleet", coverage, "percent", "Share of meaningful events with a nonempty Codex user email. Ignores user/session searches. Identifies telemetry coverage, not verified employee identity."),
    ]
    for n, (title, expr, unit, description) in enumerate(stats):
        p = loki_stat_panel(ids, title=title, description=description, expr=expr, unit=unit,
            grid={"x": n % 4 * 6, "y": 3 + n // 4 * 4, "w": 6, "h": 4}, mappings=[NO_DATA_MAPPING])
        p["fieldConfig"]["defaults"]["decimals"] = 1 if unit in ("ms", "percent") else 0
        p["options"]["colorMode"] = "none"
        panels.append(p)

    activity = loki_timeseries_panel(ids, title="Meaningful activity", description=
        "Events in each rolling interval, not events per second. Completed responses count model completions; "
        "transport attempts, stream chunks, startup and approval diagnostics are excluded. Tool calls include orchestration tools.",
        expr="", legend="", unit="short", grid={"x": 0, "y": 11, "w": 12, "h": 8})
    activity["targets"] = [target(count(q, "$__interval"), ref, name, instant=False) for ref, name, q in [
        ("A", "Prompts", logs("codex.user_prompt")),
        ("B", "Model responses", completed()),
        ("C", "Tool calls", logs("codex.tool_result"))]]
    activity["interval"] = "5m"
    activity["options"]["legend"]["calcs"] = []
    panels.append(activity)
    for x, title, expr, description in [
        (12, "Models · responses", count(completed(), group="model"), "Share of completed model responses; excludes failed requests and transport retries."),
        (18, "Models · tokens", total("input_tokens", group="model") + ' + ' + total("output_tokens", group="model"),
         "Share of input plus output tokens. Cached input is already included; reasoning and tool token fields are not added again. Missing either component leaves that model unavailable."),
    ]:
        panels.append({"id": ids.take(), "type": "piechart", "title": title, "description": description,
            "datasource": LOKI_DS, "gridPos": {"x": x, "y": 11, "w": 6, "h": 8},
            "fieldConfig": {"defaults": {"unit": "short", "color": {"mode": "palette-classic"}}, "overrides": []},
            "options": {"pieType": "donut", "displayLabels": [], "tooltip": {"mode": "single"},
                "legend": {"displayMode": "table", "placement": "bottom", "values": ["value", "percent"]},
                "reduceOptions": {"values": False, "calcs": ["lastNotNull"], "fields": ""}},
            "transformations": [{"id": "filterFieldsByName", "options": {"include": {"names": ["model", "Value #A"]}}},
                                {"id": "rowsToFields", "options": {"mappings": [
                {"fieldName": "model", "handlerKey": "field.name"},
                {"fieldName": "Value #A", "handlerKey": "field.value"}]}}],
            "targets": [target(expr, legend="{{model}}") ]})

    # Each query emits one row per session; outer-tabular join preserves sessions
    # even when token or timing signals are unavailable. Never fill those with 0.
    observed = meaningful() + ' | session!="" | label_format observed_ms=`{{ unixEpochMillis (__timestamp__) }}`'
    def stamp(op: str) -> str:
        return f'{op}_over_time({observed} | unwrap observed_ms | __error__=""[$__range]) by (session)'
    session_queries = [
        ("A", "Observed start", stamp("min"), "dateTimeAsIso"),
        ("B", "Last activity", stamp("max"), "dateTimeAsIso"),
        ("C", "Prompts", count(logs("codex.user_prompt") + ' | session!=""', group="session"), "short"),
        ("D", "Model responses", count(completed() + ' | session!=""', group="session"), "short"),
        ("E", "Input tokens", total("input_tokens", group="session"), "short"),
        ("F", "Output tokens", total("output_tokens", group="session"), "short"),
        ("G", "Turn wait p95", latency("codex.turn_ttft", "duration_ms", .95, group="session"), "ms"),
    ]
    table = loki_table_panel(ids, title="Sessions", description=
        "One row per conversation. Observed start and last activity are bounded by the selected period, "
        "not lifecycle timestamps. Blank cells mean no matching signal. Click a session to filter this dashboard. "
        "Prompt and response counts are observed events, not deduplicated billing records.", expr="", grid={"x": 0, "y": 19, "w": 24, "h": 9})
    table["targets"] = [dict(target(expr, ref), format="table") for ref, _, expr, _ in session_queries]
    table["transformations"] = [
        {"id": "joinByField", "options": {"byField": "session", "mode": "outerTabular"}},
        {"id": "organize", "options": {
            "excludeByName": {name: True for name in ["Time"] + [f"Time {n}" for n in range(1, 9)]},
            "renameByName": {"session": "Session", **{f"Value #{ref}": name for ref, name, _, _ in session_queries}}}},
        {"id": "sortBy", "options": {"sort": [{"field": "Last activity", "desc": True}]}}
    ]
    table["fieldConfig"]["defaults"].update(noValue="—", custom={"width": 100})
    table["fieldConfig"]["overrides"] = [
        {"matcher": {"id": "byName", "options": name}, "properties": [
            {"id": "unit", "value": unit},
            {"id": "custom.width", "value": 170 if unit == "dateTimeAsIso" else (110 if name == "Model responses" else 100)}]}
        for _, name, _, unit in session_queries] + [{"matcher": {"id": "byName", "options": "Session"}, "properties": [
            {"id": "custom.width", "value": 285},
            {"id": "links", "value": [{"title": "Open session", "url":
                "/d/" + UID + "?${__url_time_range}&var-user=${user:percentencode}&var-session=${__value.raw:percentencode}",
                "targetBlank": False}]}]}]
    panels.append(table)

    for x, title, event, field, description in [
        (0, "Turn waiting time", "codex.turn_ttft", "duration_ms", "Turn-level time to first response. These samples back the summary; never sum percentiles."),
        (12, "Request waiting time", "codex.sse_event", "ttft_ms", "Time to first token on completed model responses. A turn can make multiple requests; this is a different population from turn waiting time."),
    ]:
        p = loki_timeseries_panel(ids, title=title, description=description, expr="", legend="", unit="ms",
            grid={"x": x, "y": 28, "w": 12, "h": 7})
        p["targets"] = [target(latency(event, field, quantile, "$__interval"), ref, name, instant=False)
                        for ref, name, quantile in [("A", "Median", .5), ("B", "p95", .95)]]
        p["interval"] = "5m"
        p["options"]["legend"]["calcs"] = []
        panels.append(p)
    d = dashboard_shell(UID, "Codex telemetry", "User and session activity, model usage and waiting time from Codex telemetry. Generated; see docs/integrations/codex-dashboard.md.", panels)
    d["tags"].append("codex")
    d["time"] = {"from": "now-24h", "to": "now"}
    d["templating"]["list"] = [{"name": name, "label": label, "type": "textbox", "query": "",
        "current": {"text": "", "value": ""}, "options": [{"text": "", "value": "", "selected": True}],
        "hide": 0, "skipUrlSync": False, "description": description} for name, label, description in [
            ("user", "User email", "Literal email search; blank includes everyone, including unattributed traffic."),
            ("session", "Session", "Literal conversation ID search; blank includes all sessions. Click a session row to select it.")]]
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
