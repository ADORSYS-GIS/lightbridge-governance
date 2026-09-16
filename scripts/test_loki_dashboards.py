"""Deterministic dashboard checks: python3 -m unittest discover -s scripts -p 'test_*.py'."""

import unittest

from generate_claude_code_dashboard import build_dashboard as build_claude_dashboard
from generate_codex_dashboard import build_dashboard as build_codex_dashboard


class CodexDashboardQueriesTest(unittest.TestCase):
    build_dashboard = staticmethod(build_codex_dashboard)

    def test_loki_stats_are_instant_snapshots(self):
        panels = [panel for panel in self.build_dashboard()["panels"]
                  if panel["type"] == "stat" and panel["datasource"]["type"] == "loki"]
        self.assertTrue(panels, "must exercise Loki stat panels")
        for panel in panels:
            with self.subTest(panel=panel["title"]):
                self.assertEqual(panel["options"]["graphMode"], "none")
                self.assertTrue(panel["targets"])
                for target in panel["targets"]:
                    self.assertEqual(target["queryType"], "instant")
                    self.assertIs(target.get("instant"), True)

    def test_loki_time_series_keep_range_queries(self):
        panels = [panel for panel in self.build_dashboard()["panels"]
                  if panel["type"] == "timeseries" and panel["datasource"]["type"] == "loki"]
        self.assertTrue(panels, "must exercise Loki time-series panels")
        for panel in panels:
            with self.subTest(panel=panel["title"]):
                self.assertTrue(panel["targets"])
                for target in panel["targets"]:
                    self.assertEqual(target["queryType"], "range")
                    self.assertFalse(target.get("instant", False))


class ClaudeDashboardQueriesTest(CodexDashboardQueriesTest):
    build_dashboard = staticmethod(build_claude_dashboard)

    # Claude Code is the one dashboard among these with a SECOND datasource
    # (Mimir/Prometheus, since the 2026-09-14 metrics-temporality fix --
    # lightbridge-governance#335 -- made lines-of-code/active-time/commits/
    # pull-requests real, queryable series). The base class's two tests
    # above now filter to `datasource.type == "loki"` for exactly this
    # reason; these two cover the Prometheus half with the shape
    # generate_vscode_copilot_dashboard.py's own Prometheus panels already
    # use (`instant`/`range` booleans, no `queryType` key).
    def test_prometheus_stats_are_instant_snapshots(self):
        panels = [panel for panel in self.build_dashboard()["panels"]
                  if panel["type"] == "stat" and panel["datasource"]["type"] == "prometheus"]
        self.assertTrue(panels, "must exercise Mimir/Prometheus stat panels")
        for panel in panels:
            with self.subTest(panel=panel["title"]):
                self.assertEqual(panel["options"]["graphMode"], "none")
                self.assertTrue(panel["targets"])
                for target in panel["targets"]:
                    self.assertIs(target.get("instant"), True)
                    self.assertIs(target.get("range"), False)

    def test_prometheus_time_series_keep_range_queries(self):
        panels = [panel for panel in self.build_dashboard()["panels"]
                  if panel["type"] == "timeseries" and panel["datasource"]["type"] == "prometheus"]
        self.assertTrue(panels, "must exercise Mimir/Prometheus time-series panels")
        for panel in panels:
            with self.subTest(panel=panel["title"]):
                self.assertTrue(panel["targets"])
                for target in panel["targets"]:
                    self.assertIs(target.get("instant"), False)
                    self.assertIs(target.get("range"), True)
