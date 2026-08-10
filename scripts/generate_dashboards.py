#!/usr/bin/env python3
"""Generate the Copilot connector Grafana dashboard JSON.

DEV-TIME TOOL ONLY. This repo's toolchain is `cargo`/`cratestack` (see
AGENTS.md: "There is no `npm` and no Python here"). Python is used here
purely as a convenient, dependency-free templating language for a JSON blob
that Grafana consumes -- nothing at deploy time or in the running system
requires a Python interpreter. The **generated JSON is committed** at
`charts/lightbridge-governance/dashboards/copilot-connector.json`; that
committed file, not this script, is what `helm template`/the Grafana
Operator actually reads. CI can optionally run this script's `--check` mode
to catch a stale commit, but nothing in the deploy path re-runs it -- if
Python is ever unavailable in CI, that check step is skippable without
breaking the deploy.

Standard library only: no grafanalib, no pyyaml, no requests. Targets the
Python 3.14 available in this environment but avoids anything newer than
what a stock 3.9+ interpreter provides, since "python3" in CI/dev images
is not otherwise pinned here.

Determinism contract: running this script twice, or running it a year from
now, must produce a byte-identical file. That means:
  - no timestamps, no `random`, no `uuid`, no dict ordering that depends on
    hash seed (plain `dict` literals in insertion order are fine -- CPython
    3.7+ guarantees insertion order, and this script never iterates a `set`
    or unordered mapping into JSON output);
  - panel ids are assigned by a plain counter over a fixed code path, not
    derived from anything environmental;
  - `json.dumps(..., sort_keys=False)` relies on the *code's* insertion
    order being stable, which it is, since the dashboard is built by a
    fixed sequence of function calls below, not by iterating a dict/set
    whose order Python does not guarantee.

Usage:
    python3 scripts/generate_dashboards.py           # regenerate the file
    python3 scripts/generate_dashboards.py --check    # verify it's current
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT_PATH = REPO_ROOT / "charts" / "lightbridge-governance" / "dashboards" / "copilot-connector.json"

# --------------------------------------------------------------------------
# Datasource contract (charts/lightbridge-governance/values.yaml's
# `grafanaDashboard.datasources` block substitutes these literal tokens at
# `helm template` time -- see that file and templates/grafanadashboard.yaml).
# Do NOT use dashboard __inputs or a datasource template variable; the
# deployment pins these two datasources.
# --------------------------------------------------------------------------
PROM_TYPE = "prometheus"
PROM_UID = "__DS_PROMETHEUS__"
# Grafana renamed the built-in PostgreSQL datasource plugin id from
# "postgres" to "grafana-postgresql-datasource" (the rename shipped between
# 10.2.2 and 10.2.3; provisioning YAML's `type:` field still accepts the old
# "postgres" string, but a *dashboard JSON's* panel/target `datasource.type`
# must use the new plugin id or a modern Grafana will not resolve the panel
# to the right plugin). Verified via the plugin's own source tree
# (`grafana/grafana` -> `public/app/plugins/datasource/
# grafana-postgresql-datasource/`) and grafana.com's current plugin page for
# "PostgreSQL data source", both listing this exact id.
PG_TYPE = "grafana-postgresql-datasource"
PG_UID = "__DS_POSTGRES__"

PROM_DS = {"type": PROM_TYPE, "uid": PROM_UID}
PG_DS = {"type": PG_TYPE, "uid": PG_UID}

# Standard "null/NaN -> explicit text" value mapping, applied to every
# Section 2 (dashboard-grade, per ADR-0011) panel so an empty panel renders
# as visibly "no data" rather than a bare, legitimate-looking blank. Also
# applied to the Section 1 "has synced" panel, whose whole point is that
# never-synced must not look like a healthy default.
NO_DATA_MAPPING = {
    "type": "special",
    "options": {
        "match": "null+nan",
        "result": {"text": "NO DATA", "color": "red", "index": 0},
    },
}


class Ids:
    """Deterministic, sequential Grafana panel ids.

    A plain counter over a fixed call sequence in `build_dashboard()` below
    -- not derived from anything environmental -- so two runs (or a run a
    year apart) assign identical ids to identical panels.
    """

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


def text_panel(
    ids: Ids, *, title: str, content: str, grid: dict[str, int]
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "text",
        "title": title,
        "gridPos": grid,
        "options": {"mode": "markdown", "content": content},
    }


def _base_field_config(
    *, unit: str, mappings: list[dict[str, Any]] | None = None,
    thresholds_steps: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    defaults: dict[str, Any] = {"unit": unit}
    if mappings:
        defaults["mappings"] = mappings
    defaults["thresholds"] = {
        "mode": "absolute",
        "steps": thresholds_steps or [{"color": "green", "value": None}],
    }
    return {"defaults": defaults, "overrides": []}


def prom_stat_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    expr: str,
    unit: str,
    grid: dict[str, int],
    mappings: list[dict[str, Any]] | None = None,
    thresholds_steps: list[dict[str, Any]] | None = None,
    instant: bool = True,
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "stat",
        "title": title,
        "description": description,
        "datasource": PROM_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(
            unit=unit, mappings=mappings, thresholds_steps=thresholds_steps
        ),
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
                "instant": instant,
                "range": not instant,
                "legendFormat": "__auto",
                "refId": "A",
            }
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
    mappings: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "timeseries",
        "title": title,
        "description": description,
        "datasource": PROM_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit, mappings=mappings),
        "options": {
            "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull"]},
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


def pg_timeseries_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    sql: str,
    unit: str,
    grid: dict[str, int],
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "timeseries",
        "title": title,
        "description": description,
        "datasource": PG_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(unit=unit),
        "options": {
            "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull"]},
            "tooltip": {"mode": "multi"},
        },
        "targets": [
            {
                "datasource": PG_DS,
                "rawSql": sql,
                "format": "time_series",
                "editorMode": "code",
                "refId": "A",
            }
        ],
    }


def pg_stat_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    sql: str,
    unit: str,
    grid: dict[str, int],
    mappings: list[dict[str, Any]] | None = None,
    thresholds_steps: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "stat",
        "title": title,
        "description": description,
        "datasource": PG_DS,
        "gridPos": grid,
        "fieldConfig": _base_field_config(
            unit=unit, mappings=mappings, thresholds_steps=thresholds_steps
        ),
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
                "datasource": PG_DS,
                "rawSql": sql,
                "format": "table",
                "editorMode": "code",
                "refId": "A",
            }
        ],
    }


def pg_table_panel(
    ids: Ids,
    *,
    title: str,
    description: str,
    sql: str,
    grid: dict[str, int],
    overrides: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    return {
        "id": ids.take(),
        "type": "table",
        "title": title,
        "description": description,
        "datasource": PG_DS,
        "gridPos": grid,
        "fieldConfig": {"defaults": {"unit": "short"}, "overrides": overrides or []},
        "options": {
            "showHeader": True,
            "cellHeight": "sm",
        },
        "targets": [
            {
                "datasource": PG_DS,
                "rawSql": sql,
                "format": "table",
                "editorMode": "code",
                "refId": "A",
            }
        ],
    }


# --------------------------------------------------------------------------
# Section 3 SQL. All queries filter tenant_id = '$tenant_id' (house rule:
# tenant_id on every query, ADR-0001) and bind time ranges to `report_day`
# (the "data about" day) via Grafana's $__timeFilter() macro, NOT
# `created_at`/`updated_at` (write-time bookkeeping columns, not what the
# row is about).
#
# Money: net_cost_micro_usd is stored as bigint integer micro-USD (ADR-0008
# -- no float ever touches a *stored* monetary value). The `/ 1e6` cast
# below happens only inside the SQL SELECT projection, i.e. at display time
# in a read-only reporting query -- it never writes back, and no column in
# any `copilot_*_dailys` table changes type. This is a presentation-layer
# conversion for the panel's "USD" unit, not a violation of ADR-0008.
# --------------------------------------------------------------------------

SQL_ADOPTION_OVER_TIME = """\
SELECT
  report_day AS time,
  SUM(active_users) AS active_users,
  SUM(engaged_users) AS engaged_users
FROM copilot_org_dailys
WHERE tenant_id = '$tenant_id'
  AND $__timeFilter(report_day)
GROUP BY report_day
ORDER BY report_day"""

SQL_ACCEPTANCE_RATE = """\
SELECT
  report_day AS time,
  CASE
    WHEN SUM(code_generations) = 0 THEN 0
    ELSE SUM(code_acceptances)::float8 / SUM(code_generations)
  END AS acceptance_rate
FROM copilot_org_dailys
WHERE tenant_id = '$tenant_id'
  AND $__timeFilter(report_day)
GROUP BY report_day
ORDER BY report_day"""

SQL_TOP_USERS_BY_INTERACTIONS = """\
SELECT
  user_login,
  SUM(total_interactions) AS total_interactions
FROM copilot_user_dailys
WHERE tenant_id = '$tenant_id'
  AND $__timeFilter(report_day)
GROUP BY user_login
ORDER BY total_interactions DESC
LIMIT 20"""

SQL_COST_OVER_TIME = """\
SELECT
  report_day AS time,
  SUM(net_cost_micro_usd)::float8 / 1e6 AS cost_usd
FROM copilot_org_dailys
WHERE tenant_id = '$tenant_id'
  AND $__timeFilter(report_day)
GROUP BY report_day
ORDER BY report_day"""

SQL_TOP_USERS_BY_COST = """\
SELECT
  user_login,
  SUM(net_cost_micro_usd)::float8 / 1e6 AS cost_usd
FROM copilot_user_dailys
WHERE tenant_id = '$tenant_id'
  AND $__timeFilter(report_day)
GROUP BY user_login
ORDER BY cost_usd DESC
LIMIT 20"""

SQL_REPO_ACTIVITY = """\
SELECT
  report_day AS time,
  repository_id,
  coding_agent_activity,
  code_review_activity,
  pull_request_activity
FROM copilot_repo_dailys
WHERE tenant_id = '$tenant_id'
  AND $__timeFilter(report_day)
ORDER BY report_day, repository_id"""

# Intentionally NOT time-filtered: this is a point-in-time health check
# ("what is the latest state per report_type right now"), not a trend --
# constraining it to the dashboard's selected time range would hide a
# report_type whose latest manifest happens to fall outside the window,
# which is exactly the situation an operator most needs to see.
SQL_INGEST_MANIFEST_HEALTH = """\
SELECT DISTINCT ON (report_type)
  report_type,
  report_day,
  status,
  record_count,
  completed_at
FROM ingest_manifests
WHERE tenant_id = '$tenant_id'
  AND provider = 'github_copilot'
ORDER BY report_type, report_day DESC, completed_at DESC NULLS LAST"""

# --------------------------------------------------------------------------
# Seat hygiene SQL (copilot_seat_snapshots). RFC-0001 Motivation, verbatim:
# "who has a seat and has never used it", "what does adoption look like by
# team", "what are we paying per active user".
#
# copilot_seat_snapshots is current-state, one row per user per
# snapshot_day, with no history before ingestion started -- there is no
# meaningful trend yet. Every query below therefore pins to the LATEST
# snapshot_day via a `MAX(snapshot_day)` subquery scoped to $tenant_id,
# rather than assuming the dashboard's selected time range contains data
# (it may not, especially on day one). Note the column is `snapshot_day`,
# not `report_day` like every other copilot_* table.
#
# NULL semantics are load-bearing, not incidental:
#   - last_activity_at IS NULL means "never used" -- this is the exact
#     signal RFC-0001 asks for and must never be coalesced to a date or a
#     zero, which would silently misrepresent an unused seat as a used one.
#   - seat_assigned_at IS NULL means "assignment time unknown" -- also left
#     as NULL rather than defaulted, so a table column renders it visibly
#     blank instead of a fabricated date.
#
# seat_state is NOT NULL and only ever 'active' or 'pending_cancellation'
# (derived by the connector from GitHub's pending_cancellation_date; a
# cancelled seat simply disappears from the listing, so there is no
# 'cancelled' state to see here).
# --------------------------------------------------------------------------

SQL_SEAT_NEVER_USED_COUNT = """\
SELECT COUNT(*) AS never_used_seats
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
  AND last_activity_at IS NULL"""

SQL_SEAT_NEVER_USED_TABLE = """\
SELECT
  user_login,
  seat_assigned_at,
  (snapshot_day::date - seat_assigned_at::date) AS days_assigned
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
  AND last_activity_at IS NULL
ORDER BY seat_assigned_at ASC NULLS FIRST"""

# $idle_threshold_days is a dashboard variable (custom, default 30) rather
# than a hardcoded number -- interpolated as a plain integer into the
# interval literal below.
SQL_SEAT_IDLE_COUNT = """\
SELECT COUNT(*) AS idle_seats
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
  AND last_activity_at IS NOT NULL
  AND last_activity_at < snapshot_day - ('$idle_threshold_days days')::interval"""

SQL_SEAT_IDLE_TABLE = """\
SELECT
  user_login,
  last_activity_at,
  last_activity_editor,
  (snapshot_day::date - last_activity_at::date) AS days_idle
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
  AND last_activity_at IS NOT NULL
  AND last_activity_at < snapshot_day - ('$idle_threshold_days days')::interval
ORDER BY last_activity_at ASC"""

SQL_SEAT_TOTAL_COUNT = """\
SELECT COUNT(*) AS seats_assigned
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )"""

SQL_SEAT_PENDING_CANCELLATION_COUNT = """\
SELECT COUNT(*) AS pending_cancellation_seats
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
  AND seat_state = 'pending_cancellation'"""

SQL_SEAT_STATE_BREAKDOWN = """\
SELECT
  seat_state,
  COUNT(*) AS seats
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
GROUP BY seat_state
ORDER BY seat_state"""

SQL_SEAT_EDITOR_BREAKDOWN = """\
SELECT
  COALESCE(last_activity_editor, 'never used') AS editor,
  COUNT(*) AS seats
FROM copilot_seat_snapshots
WHERE tenant_id = '$tenant_id'
  AND snapshot_day = (
    SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
  )
GROUP BY COALESCE(last_activity_editor, 'never used')
ORDER BY seats DESC"""

# Active users = seats with any recorded usage ever (last_activity_at NOT
# NULL) on the latest snapshot day, joined against the most recent
# available copilot_org_dailys cost row for the tenant (seat snapshots and
# org daily reports are independent ingests and are not guaranteed to share
# a day). Money is integer micro-USD in storage (ADR-0008); the /1e6 below
# is a presentation-only conversion inside this read-only SELECT, exactly
# like the existing cost panels above -- no stored column changes type.
# A NULL result (no active seats, or no cost row yet for the tenant) is
# left as NULL rather than defaulted to 0, so the panel visibly reads "no
# data" instead of a misleading zero cost.
SQL_SEAT_COST_PER_ACTIVE_USER = """\
SELECT
  CASE
    WHEN (
      SELECT COUNT(*) FROM copilot_seat_snapshots
      WHERE tenant_id = '$tenant_id'
        AND snapshot_day = (
          SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
        )
        AND last_activity_at IS NOT NULL
    ) = 0 THEN NULL
    ELSE (
      (SELECT net_cost_micro_usd FROM copilot_org_dailys
       WHERE tenant_id = '$tenant_id'
       ORDER BY report_day DESC LIMIT 1)::float8 / 1e6
    ) / (
      SELECT COUNT(*) FROM copilot_seat_snapshots
      WHERE tenant_id = '$tenant_id'
        AND snapshot_day = (
          SELECT MAX(snapshot_day) FROM copilot_seat_snapshots WHERE tenant_id = '$tenant_id'
        )
        AND last_activity_at IS NOT NULL
    )
  END AS cost_per_active_user_usd"""


def build_dashboard() -> dict[str, Any]:
    ids = Ids()
    panels: list[dict[str, Any]] = []
    y = 0

    # ---------------------------------------------------------------
    # Section 1 -- Connector health (Prometheus/Mimir). Alert-grade.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Connector health (alert-grade -- ADR-0007)", y))
    y += 1

    panels.append(
        prom_stat_panel(
            ids,
            title="Freshness (authoritative)",
            description=(
                "THE authoritative freshness signal (docs/runbooks/copilot-sync-failed.md). "
                "time() - governance_connector_last_success_timestamp_seconds, derived by the "
                "always-on API from ingest_manifests on every /metrics scrape (ADR-0007). "
                "Survives API restarts and is unaffected by the copilot-sync OTel collector's "
                "own restarts or NetworkPolicy misconfiguration. Absent (never a fabricated 0) "
                "until a refresh has actually observed a successful day -- an absent value here "
                "means 'unknown', not 'just synced'. Alert thresholds mirror the runbook: "
                "36h (129600s) and 72h (259200s)."
            ),
            expr='time() - governance_connector_last_success_timestamp_seconds{provider="github_copilot"}',
            unit="s",
            grid={"h": 8, "w": 8, "x": 0, "y": y},
            thresholds_steps=[
                {"color": "green", "value": None},
                {"color": "orange", "value": 129600},
                {"color": "red", "value": 259200},
            ],
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Has synced",
            description=(
                "governance_connector_has_synced{provider=\"github_copilot\"} (ADR-0007). "
                "1 once a manifest row has ever landed, 0 once a refresh has confirmed it "
                "never has. A never-synced deployment must read as obviously broken, not as a "
                "healthy default -- hence the explicit 0/'NEVER SYNCED' mapping below rather "
                "than leaving 0 to render as a plain number."
            ),
            expr='governance_connector_has_synced{provider="github_copilot"}',
            unit="none",
            grid={"h": 8, "w": 8, "x": 8, "y": y},
            mappings=[
                {
                    "type": "value",
                    "options": {
                        "0": {"text": "NEVER SYNCED", "color": "red", "index": 0},
                        "1": {"text": "SYNCED", "color": "green", "index": 1},
                    },
                },
                NO_DATA_MAPPING,
            ],
            thresholds_steps=[{"color": "red", "value": None}, {"color": "green", "value": 1}],
        )
    )
    panels.append(
        prom_timeseries_panel(
            ids,
            title="Metrics scrape errors",
            description=(
                "rate(governance_connector_metrics_scrape_errors_total[5m]) by reason "
                "(timeout | query_error). Always present starting at 0 (ADR-0007) -- unlike "
                "the freshness/has-synced gauges above, a flat zero here is a legitimate, "
                "healthy reading, not a 'no data' condition."
            ),
            expr="rate(governance_connector_metrics_scrape_errors_total[5m])",
            legend="{{reason}}",
            unit="ops",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 2 -- Last sync run detail (Prometheus/Mimir). Dashboard-grade
    # only per ADR-0011: lives in the collector's memory, a restart blanks
    # it, and it expires to *absent* (not stale) after 30h.
    # ---------------------------------------------------------------
    dashboard_grade_caveat = (
        "Dashboard-grade only (ADR-0011), NOT alert-grade. This series lives solely in the "
        "copilot-sync OTel collector's in-memory Prometheus cache (replicas: 1, no "
        "PodDisruptionBudget) -- a restart (node drain, image bump, OOM, reschedule) blanks it "
        "until the next copilot-sync run, up to 6h later. It also expires to *absent* (a cliff, "
        "not a gradual staleness signal) after 30h. An empty panel here means 'no run this "
        "cycle', not 'zero activity' -- see docs/runbooks/copilot-sync-failed.md. For whether "
        "the connector is actually healthy, use the Section 1 panels above instead."
    )
    panels.append(row(ids, "Last sync run detail (dashboard-grade only -- ADR-0011)", y))
    y += 1

    panels.append(
        prom_timeseries_panel(
            ids,
            title="Age since last run, by command",
            description=f"time() - governance_copilot_last_run_timestamp_seconds{{command}}. {dashboard_grade_caveat}",
            expr='time() - governance_copilot_last_run_timestamp_seconds{command=~".+"}',
            legend="{{command}}",
            unit="s",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Report days ingested (last run)",
            description=f"governance_copilot_days -- report days covered by the most recent run. {dashboard_grade_caveat}",
            expr="governance_copilot_days",
            unit="none",
            grid={"h": 8, "w": 6, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        prom_stat_panel(
            ids,
            title="Manifest drift (verify)",
            description=(
                "governance_copilot_manifest_drift -- manifest rows whose stored count "
                f"disagrees, `verify` runs only. Non-zero is a problem. {dashboard_grade_caveat}"
            ),
            expr="governance_copilot_manifest_drift",
            unit="none",
            grid={"h": 8, "w": 6, "x": 18, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[{"color": "green", "value": None}, {"color": "red", "value": 1}],
        )
    )
    y += 8

    panels.append(
        prom_timeseries_panel(
            ids,
            title="Reports fetched, by report type and outcome",
            description=f"governance_copilot_reports{{report,status}}. {dashboard_grade_caveat}",
            expr='governance_copilot_reports{report=~".+"}',
            legend="{{report}} / {{status}}",
            unit="none",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        prom_timeseries_panel(
            ids,
            title="Rows upserted, by report type",
            description=f"governance_copilot_rows{{report}}. {dashboard_grade_caveat}",
            expr='governance_copilot_rows{report=~".+"}',
            legend="{{report}}",
            unit="none",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    panels.append(
        prom_stat_panel(
            ids,
            title="Unmapped users",
            description=(
                "governance_copilot_unmapped_users -- users with usage but no team row, "
                f"latest day. {dashboard_grade_caveat}"
            ),
            expr="governance_copilot_unmapped_users",
            unit="none",
            grid={"h": 8, "w": 8, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[{"color": "green", "value": None}, {"color": "yellow", "value": 1}],
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 3 -- Adoption and cost (Postgres, ADR-0003). Usernames,
    # repos, teams and money live here as columns, not Prometheus labels.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Adoption and cost (Postgres -- ADR-0003)", y))
    y += 1

    panels.append(
        pg_timeseries_panel(
            ids,
            title="Adoption over time: active vs engaged users",
            description="copilot_org_dailys, summed per report_day, filtered to $tenant_id.",
            sql=SQL_ADOPTION_OVER_TIME,
            unit="none",
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        pg_timeseries_panel(
            ids,
            title="Acceptance rate",
            description=(
                "code_acceptances / code_generations from copilot_org_dailys, guarded against "
                "division by zero (0 when no generations that day)."
            ),
            sql=SQL_ACCEPTANCE_RATE,
            unit="percentunit",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        pg_table_panel(
            ids,
            title="Top users by interactions",
            description="copilot_user_dailys, summed over the selected time range, filtered to $tenant_id.",
            sql=SQL_TOP_USERS_BY_INTERACTIONS,
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        pg_timeseries_panel(
            ids,
            title="Cost over time",
            description=(
                "net_cost_micro_usd from copilot_org_dailys, integer micro-USD in storage "
                "(ADR-0008); divided by 1e6 in this SELECT for display only -- a presentation "
                "conversion, not a stored float."
            ),
            sql=SQL_COST_OVER_TIME,
            unit="currencyUSD",
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        pg_table_panel(
            ids,
            title="Top users by cost",
            description=(
                "copilot_user_dailys, summed over the selected time range. net_cost_micro_usd "
                "divided by 1e6 for display only (ADR-0008 -- no stored float)."
            ),
            sql=SQL_TOP_USERS_BY_COST,
            grid={"h": 8, "w": 12, "x": 0, "y": y},
            overrides=[
                {
                    "matcher": {"id": "byName", "options": "cost_usd"},
                    "properties": [{"id": "unit", "value": "currencyUSD"}],
                }
            ],
        )
    )
    panels.append(
        pg_table_panel(
            ids,
            title="Repo activity",
            description="copilot_repo_dailys, filtered to $tenant_id and the selected time range.",
            sql=SQL_REPO_ACTIVITY,
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        pg_table_panel(
            ids,
            title="Ingest manifest health, by report type",
            description=(
                "Latest ingest_manifests row per report_type for $tenant_id's github_copilot "
                "provider -- not time-range filtered by design, so a report_type whose latest "
                "manifest falls outside the dashboard's selected window is still visible. Local "
                "dev data may show a few status='completed' rows and a stray "
                "'user_teams_1_day' report_type that no production path writes -- test residue, "
                "not a bug."
            ),
            sql=SQL_INGEST_MANIFEST_HEALTH,
            grid={"h": 8, "w": 24, "x": 0, "y": y},
        )
    )
    y += 8

    # ---------------------------------------------------------------
    # Section 4 -- Seat hygiene (Postgres, ADR-0003). RFC-0001's headline
    # motivation: "who has a seat and has never used it", "what does
    # adoption look like by team", "what are we paying per active user".
    # copilot_seat_snapshots is current-state only (PR #70) -- every panel
    # here pins to the latest snapshot_day rather than the dashboard's time
    # range; see the SQL_SEAT_* constants above for the NULL-semantics and
    # latest-snapshot rationale in full.
    # ---------------------------------------------------------------
    panels.append(row(ids, "Seat hygiene (Postgres -- ADR-0003, RFC-0001)", y))
    y += 1

    panels.append(
        pg_stat_panel(
            ids,
            title="Never-used seats",
            description=(
                "Seats on the latest snapshot day with last_activity_at IS NULL -- assigned "
                "but never touched. This is the licence you are paying for and nobody has "
                "used; NULL here is 'never used', deliberately not coalesced to a date or a "
                "zero. See the table below for names."
            ),
            sql=SQL_SEAT_NEVER_USED_COUNT,
            unit="none",
            grid={"h": 8, "w": 6, "x": 0, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[{"color": "green", "value": None}, {"color": "orange", "value": 1}],
        )
    )
    panels.append(
        pg_stat_panel(
            ids,
            title="Idle seats (> $idle_threshold_days d)",
            description=(
                "Seats used at some point (last_activity_at NOT NULL) but not within the last "
                "$idle_threshold_days days as of the latest snapshot day. Threshold is a "
                "dashboard variable, not hardcoded -- change it in the picker above."
            ),
            sql=SQL_SEAT_IDLE_COUNT,
            unit="none",
            grid={"h": 8, "w": 6, "x": 6, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[{"color": "green", "value": None}, {"color": "orange", "value": 1}],
        )
    )
    panels.append(
        pg_stat_panel(
            ids,
            title="Seats assigned (latest snapshot)",
            description="Total copilot_seat_snapshots rows for $tenant_id on the latest snapshot_day.",
            sql=SQL_SEAT_TOTAL_COUNT,
            unit="none",
            grid={"h": 8, "w": 6, "x": 12, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    panels.append(
        pg_stat_panel(
            ids,
            title="Pending cancellation seats",
            description=(
                "seat_state = 'pending_cancellation' on the latest snapshot day -- derived by "
                "the connector from GitHub's pending_cancellation_date. GitHub exposes no "
                "other lifecycle field; a fully cancelled seat simply disappears from the "
                "listing rather than appearing as a third state."
            ),
            sql=SQL_SEAT_PENDING_CANCELLATION_COUNT,
            unit="none",
            grid={"h": 8, "w": 6, "x": 18, "y": y},
            mappings=[NO_DATA_MAPPING],
            thresholds_steps=[{"color": "green", "value": None}, {"color": "yellow", "value": 1}],
        )
    )
    y += 8

    panels.append(
        pg_table_panel(
            ids,
            title="Never-used seats -- who",
            description=(
                "Latest snapshot day, last_activity_at IS NULL. days_assigned is NULL when "
                "seat_assigned_at itself is unknown (also NULL, not defaulted)."
            ),
            sql=SQL_SEAT_NEVER_USED_TABLE,
            grid={"h": 8, "w": 12, "x": 0, "y": y},
        )
    )
    panels.append(
        pg_table_panel(
            ids,
            title="Idle seats -- who",
            description=(
                "Latest snapshot day, used at some point but not within $idle_threshold_days "
                "days. last_activity_editor shows which tooling they last used."
            ),
            sql=SQL_SEAT_IDLE_TABLE,
            grid={"h": 8, "w": 12, "x": 12, "y": y},
        )
    )
    y += 8

    panels.append(
        pg_table_panel(
            ids,
            title="Seats by state",
            description=(
                "seat_state breakdown on the latest snapshot day -- only ever 'active' or "
                "'pending_cancellation' (see the connector note above)."
            ),
            sql=SQL_SEAT_STATE_BREAKDOWN,
            grid={"h": 8, "w": 8, "x": 0, "y": y},
        )
    )
    panels.append(
        pg_table_panel(
            ids,
            title="Editor breakdown",
            description=(
                "last_activity_editor on the latest snapshot day, which tooling is actually in "
                "use. 'never used' is its own bucket (last_activity_editor IS NULL because "
                "last_activity_at IS NULL), not folded into any real editor's count."
            ),
            sql=SQL_SEAT_EDITOR_BREAKDOWN,
            grid={"h": 8, "w": 8, "x": 8, "y": y},
        )
    )
    panels.append(
        pg_stat_panel(
            ids,
            title="Cost per active user",
            description=(
                "Latest copilot_org_dailys.net_cost_micro_usd for $tenant_id, divided by the "
                "count of seats with any recorded usage on the latest seat snapshot day. "
                "Integer micro-USD in storage (ADR-0008); the /1e6 happens only in this "
                "read-only SELECT's projection, for display -- a presentation conversion, not "
                "a stored float. Seat snapshots and org daily reports are independent ingests "
                "and are not guaranteed to land on the same day. NULL (not 0) when there are "
                "no active seats or no cost row yet."
            ),
            sql=SQL_SEAT_COST_PER_ACTIVE_USER,
            unit="currencyUSD",
            grid={"h": 8, "w": 8, "x": 16, "y": y},
            mappings=[NO_DATA_MAPPING],
        )
    )
    y += 8

    dashboard: dict[str, Any] = {
        "id": None,
        "uid": "governance-copilot-connector",
        "title": "Copilot connector",
        "description": (
            "Copilot connector health, last-run detail, and adoption/cost. Generated by "
            "scripts/generate_dashboards.py -- do not hand-edit; regenerate instead."
        ),
        "tags": ["governance", "copilot"],
        "style": "dark",
        "timezone": "browser",
        "editable": True,
        "graphTooltip": 1,
        "schemaVersion": 39,
        "version": 1,
        "refresh": "5m",
        "time": {"from": "now-7d", "to": "now"},
        "timepicker": {},
        "templating": {
            "list": [
                {
                    "current": {},
                    "datasource": PG_DS,
                    # Union across every table this dashboard queries by tenant_id, not just
                    # ingest_manifests -- copilot_seat_snapshots (PR #70) has ~21 tenants in
                    # local dev with no ingest_manifests row at all (seat-only test fixtures),
                    # and a tenant-scoped variable that can't select them would silently break
                    # every Section 4 panel for those tenants. `tenant_id <> ''` excludes the
                    # empty-string tenant a real deployment can have from a misconfigured
                    # TENANT_ID env var (see AGENTS.md's secretKeyRef trap and the values.yaml
                    # fix landing alongside this) -- Grafana's query-variable "current" default
                    # picks the alphabetically-first option, and '' sorts before every real
                    # tenant name, so leaving it in would make a broken deployment's blank
                    # tenant the dashboard's silent default. Excluding it does not error even
                    # when such rows exist; it just never offers them.
                    "definition": (
                        "SELECT DISTINCT tenant_id FROM (\n"
                        "  SELECT tenant_id FROM ingest_manifests\n"
                        "  UNION\n"
                        "  SELECT tenant_id FROM copilot_seat_snapshots\n"
                        ") all_tenants\n"
                        "WHERE tenant_id <> ''\n"
                        "ORDER BY tenant_id"
                    ),
                    "hide": 0,
                    "includeAll": False,
                    "label": "Tenant",
                    "multi": False,
                    "name": "tenant_id",
                    "options": [],
                    "query": (
                        "SELECT DISTINCT tenant_id FROM (\n"
                        "  SELECT tenant_id FROM ingest_manifests\n"
                        "  UNION\n"
                        "  SELECT tenant_id FROM copilot_seat_snapshots\n"
                        ") all_tenants\n"
                        "WHERE tenant_id <> ''\n"
                        "ORDER BY tenant_id"
                    ),
                    "refresh": 1,
                    "regex": "",
                    "skipUrlSync": False,
                    "sort": 1,
                    "type": "query",
                },
                {
                    "current": {"text": "30", "value": "30"},
                    "hide": 0,
                    "includeAll": False,
                    "label": "Idle threshold (days)",
                    "multi": False,
                    "name": "idle_threshold_days",
                    "options": [
                        {"text": "7", "value": "7", "selected": False},
                        {"text": "14", "value": "14", "selected": False},
                        {"text": "30", "value": "30", "selected": True},
                        {"text": "60", "value": "60", "selected": False},
                        {"text": "90", "value": "90", "selected": False},
                    ],
                    "query": "7,14,30,60,90",
                    "queryValue": "",
                    "skipUrlSync": False,
                    "type": "custom",
                },
            ]
        },
        "annotations": {"list": []},
        "links": [],
        "panels": panels,
    }
    return dashboard


def render(dashboard: dict[str, Any]) -> str:
    # sort_keys=False: ordering is determined entirely by this script's own
    # (fixed) insertion order, which is itself deterministic -- see the
    # module docstring. A trailing newline keeps the committed file
    # newline-terminated, matching standard POSIX text-file convention.
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

    # Fail loudly on malformed output rather than writing/comparing garbage.
    json.loads(generated)

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
