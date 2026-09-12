"""Navigation and export settings shared by the dev-time dashboard generators."""
from typing import Any

SOURCES = [
    ("governance-claude-code-telemetry", "Claude Code", "Cost, tokens, tools and reliability."),
    ("governance-codex-telemetry", "Codex", "Tokens, latency, tool decisions and reliability."),
    ("governance-opencode-telemetry", "OpenCode", "Client-reported usage, tools and errors."),
    ("governance-vscode-copilot", "VS Code Copilot", "Editor sessions, code edits and acceptance."),
    ("governance-copilot-connector", "Copilot reports & seats", "GitHub reports, assigned seats and pull-job health."),
]


def dashboard_links(uid: str) -> list[dict[str, Any]]:
    # Grafana 12.3.1's built-in export defaults to 60s. The renderer has a
    # separate 150s readiness allowance; leave 30s for navigation and encoding.
    links = [{
        "type": "link", "title": "Export PNG", "icon": "external link",
        "url": f"/render/d/{uid}?width=1280&height=-1&scale=1&fullPageImage=true&kiosk=true&hideNav=true&timeout=180",
        "targetBlank": True, "includeTime": True, "includeVars": True,
        "tooltip": "Full dashboard image; allow up to 3 minutes. Uses the selected time range and filters.",
    }]
    if uid != "governance-ai-cli-telemetry":
        links.append({"type": "link", "title": "Governance overview", "url": "/d/governance-ai-cli-telemetry",
                      "includeTime": True, "includeVars": False, "targetBlank": False})
    return links


def dashboard_shell(uid: str, title: str, description: str, panels: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "id": None, "uid": uid, "title": title, "description": description,
        "tags": ["governance"], "style": "dark", "timezone": "browser", "editable": True,
        "graphTooltip": 1, "schemaVersion": 39, "version": 1, "refresh": "15m",
        "time": {"from": "now-7d", "to": "now"}, "timepicker": {},
        "templating": {"list": []}, "annotations": {"list": []},
        "links": dashboard_links(uid), "panels": panels,
    }
