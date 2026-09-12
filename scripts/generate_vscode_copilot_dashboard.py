#!/usr/bin/env python3
"""Generate VS Code Copilot's dashboard. Standard-library, dev-time tool.

Moved from AI CLI without changing the six validated Mimir queries. These
copilot_chat_* counters were confirmed against production; they are distinct
from the GitHub reports/seat API consumed by governance-ctl. No user identity
is inferred from a host, session or metric series.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from dashboard_common import dashboard_shell

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts/lightbridge-governance/dashboards/vscode-copilot.json"
PROM_DS = {"type": "prometheus", "uid": "__DS_PROMETHEUS__"}
NO_DATA_MAPPING = {"type": "special", "options": {"match": "null+nan", "result": {"text": "NO DATA", "color": "gray", "index": 0}}}

class Ids:
    """Deterministic panel identifiers."""

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
    """A counter trend over the selected interval."""
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
    panels.append(row(ids, "Code changes", y))
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
                "edit outcomes reported by VS Code Copilot."
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

    return dashboard_shell(
        "governance-vscode-copilot", "VS Code Copilot",
        "Editor sessions, tools, code edits and acceptance from VS Code Copilot Chat. "
        "GitHub daily reports and seat assignments live in the separate Copilot reports dashboard.",
        panels,
    )


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
