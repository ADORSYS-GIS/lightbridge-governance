#!/usr/bin/env python3
"""Generate VS Code Copilot's dashboard. Standard-library, dev-time tool.

Moved from AI CLI without changing the six Mimir queries at the time. These
copilot_chat_* counters are distinct from the GitHub reports/seat API
consumed by governance-ctl. No user identity is inferred from a host,
session or metric series.

## The "confirmed against production" claim did not hold for one query

Checked live on 2026-09-15 while investigating whether every AI-CLI client
had Claude Code's/Codex's metrics-temporality gap (see
docs/integrations/claude-code-dashboard.md): 26 of this dashboard's
underlying metric names were genuinely populated in Mimir, including a real
Histogram (rules out that same bug -- it would break every Sum uniformly,
not select ones). But `copilot_chat_chat_edit_outcome_count_total` --
matching the official docs' dotted name (`copilot_chat.chat_edit.outcome.count`)
correctly converted -- was completely absent, even after a real accept
dialogue with autosave on. The actual metric that fired for that exact
accept action was `copilot_chat_edit_acceptance_count_total`
(`copilot_chat.edit.acceptance.count`, a different name from the one this
generator had queried), carrying `copilot_chat_edit_outcome="accepted"` and
`copilot_chat_edit_source="chat_editing_hunk"`. Fixed to query the metric
that is actually real. See docs/runbooks/verify-vscode-copilot-edit-metrics.md
for the full live check.

## User/session filtering added 2026-09-15

Copilot's SDK only ever put identity on the OTel Resource, so none of these
metrics carried a per-user/session label -- only a `target_info` series built
from the Resource did. ai-helm-values#442 adds a shared Alloy processor that
promotes `user.email`/`user.id`/`user.name`/`session.id` onto every metric
datapoint, the same place Claude Code's and Codex's own SDKs already put
identity natively (lightbridge-governance#341 measured and closed the
`governance.retry_key` cardinality leak that had to be fixed first). Coverage
depends on that Alloy config having actually rolled out AND on new telemetry
having landed since -- a blank user dropdown, or an unfiltered-looking fleet
total, is a coverage gap on an old sample, not zero adoption.
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

# Same contract as generate_claude_code_dashboard.py's own PROM_FILTER: a
# blank ${user}/${session} textbox value must keep matching everyone, so the
# wrapping `.*...*` substring match (not an exact match) is load-bearing --
# see that file's own comment for the backtick-vs-double-quote escaping trap
# this already sidesteps by using PromQL's backtick raw-string syntax.
PROM_FILTER = 'user_email=~`.*${user:regex}.*`, session_id=~`.*${session:regex}.*`'

USER_DROPDOWN_VAR = {
    "name": "user", "label": "User email", "type": "query", "datasource": PROM_DS,
    "definition": "label_values(copilot_chat_session_count_total, user_email)",
    "query": "label_values(copilot_chat_session_count_total, user_email)",
    "refresh": 2, "sort": 1, "regex": "", "multi": False,
    "includeAll": True, "allValue": ".*", "current": {}, "options": [],
    "hide": 0, "skipUrlSync": False,
    "description": "Pick a user, or All to include everyone, including unattributed traffic "
    "(everyone, until ai-helm-values#442 has rolled out and new telemetry has landed).",
}


def sel(name: str, extra: str = "") -> str:
    """`name{user/session filter[, extra]}` -- the selector every panel below
    builds on, so the ${user}/${session} template variables reach each one
    the same way generate_claude_code_dashboard.py's metric_increase() does."""
    labels = PROM_FILTER + (f", {extra}" if extra else "")
    return f"{name}{{{labels}}}"


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
                f"(increase({sel('copilot_chat_lines_of_code_count_total')}[$__interval]))"
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
                f"sum(increase({sel('copilot_chat_lines_of_code_count_total', 'type=\"added\"')}[7d])) - "
                f"sum(increase({sel('copilot_chat_lines_of_code_count_total', 'type=\"removed\"')}[7d]))"
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
                f"(increase({sel('copilot_chat_lines_of_code_count_total', 'type=\"added\"')}[7d])))"
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
            expr=f"sum(increase({sel('copilot_chat_session_count_total')}[7d]))",
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
            expr=f"sum(increase({sel('copilot_chat_tool_call_count_total')}[7d]))",
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
                "copilot_chat_edit_acceptance_count_total{copilot_chat_edit_outcome=...}. "
                "Corrected 2026-09-15: this dashboard originally queried "
                "copilot_chat_chat_edit_outcome_count_total, a name matching the "
                "official docs' dotted form (copilot_chat.chat_edit.outcome.count) but "
                "never actually emitted -- confirmed absent from Mimir even after a "
                "real accept dialogue, autosave on, in the same live check that found "
                "the real name. copilot_chat_edit_acceptance_count_total matches "
                "copilot_chat.edit.acceptance.count instead, and captured that exact "
                "accept action live (copilot_chat_edit_outcome=\"accepted\", "
                "copilot_chat_edit_source=\"chat_editing_hunk\", value 1) -- see "
                "docs/runbooks/verify-vscode-copilot-edit-metrics.md. A dropping rate "
                "is the signal worth a human's attention -- the same edit outcomes "
                "reported by VS Code Copilot."
            ),
            expr=(
                f"sum(increase({sel('copilot_chat_edit_acceptance_count_total', 'copilot_chat_edit_outcome=\"accepted\"')}[7d])) "
                "/ "
                f"sum(increase({sel('copilot_chat_edit_acceptance_count_total')}[7d]))"
            ),
            unit="percentunit",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    UID = "governance-vscode-copilot"
    d = dashboard_shell(
        UID, "VS Code Copilot",
        "Editor sessions, tools, code edits and acceptance from VS Code Copilot Chat. "
        "GitHub daily reports and seat assignments live in the separate Copilot reports dashboard.",
        panels,
    )
    d["templating"]["list"] = [USER_DROPDOWN_VAR, {
        "name": "session", "label": "Session", "type": "textbox", "query": "",
        "current": {"text": "", "value": ""}, "options": [{"text": "", "value": "", "selected": True}],
        "hide": 0, "skipUrlSync": False,
        "description": "Literal session ID search; blank includes all sessions."}]
    d["links"].append({"type": "link", "title": "Clear user & session", "url": "/d/" + UID + "?var-user=All&var-session=",
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
