#!/usr/bin/env python3
"""Generate the Governance overview, retaining AI CLI's existing UID/filename.

Usage details belong exclusively to the source dashboards. Presence checks
cover only known source streams, never all Loki jobs. Silence can mean an idle
client; these checks do not assert that an exporter or a client is healthy.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from dashboard_common import SOURCES, dashboard_shell

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts/lightbridge-governance/dashboards/ai-cli-telemetry.json"


def build_dashboard() -> dict[str, Any]:
    panels: list[dict[str, Any]] = [{
        "id": 1, "type": "text", "title": "Source dashboards",
        "gridPos": {"h": 7, "w": 24, "x": 0, "y": 0},
        "options": {"mode": "markdown", "content": "\n\n".join(
            f"**[{title}](/d/{uid})** — {purpose}" for uid, title, purpose in SOURCES
        )},
    }]
    jobs = [
        ("Claude Code logs", 'job=~"claude-code-desktop|ai-cli/claude-code-desktop"'),
        ("Codex logs", 'job=~"codex-app-server|codex_cli_rs"'),
        ("OpenCode logs", 'job="opencode"'),
    ]
    for i, (title, selector) in enumerate(jobs):
        ds = {"type": "loki", "uid": "__DS_LOKI__"}
        panels.append({
            "id": i + 2, "type": "stat", "title": title,
            "description": "Log records observed in the hour ending at the selected time. "
                "No recent logs can mean the client is idle, unconfigured, or failing; it is not a health verdict.",
            "gridPos": {"h": 4, "w": 8, "x": i * 8, "y": 7},
            "datasource": ds,
            "fieldConfig": {"defaults": {"mappings": [
                {"type": "value", "options": {"0": {"text": "No recent logs", "color": "gray"},
                    "1": {"text": "Receiving logs", "color": "green"}}},
                {"type": "special", "options": {"match": "null+nan", "result": {"text": "Unknown", "color": "gray"}}},
            ]}, "overrides": []},
            "options": {"reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
                "graphMode": "none", "colorMode": "value"},
            "targets": [{"refId": "A", "datasource": ds,
                "expr": f"(sum(count_over_time({{{selector}}}[1h])) or vector(0)) > bool 0",
                "queryType": "instant", "instant": True}],
        })
    panels.append({
        "id": 5, "type": "text", "title": "Reading collection status",
        "gridPos": {"h": 4, "w": 24, "x": 0, "y": 11},
        "options": {"mode": "markdown", "content":
            "These signals check **recent log arrival**, not user activity or end-to-end health. "
            "A query failure remains an error, not a healthy state.\n\n"
            "VS Code Copilot uses metrics; inspect its source dashboard. "
            "GitHub Copilot pull-job freshness and failures belong in **Copilot reports & seats**. "
            "Collector infrastructure diagnostics remain in the existing Collectors dashboards."},
    })
    return dashboard_shell("governance-ai-cli-telemetry", "Governance overview",
        "Source navigation and recent telemetry arrival. Usage details live in each source dashboard.", panels)

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
