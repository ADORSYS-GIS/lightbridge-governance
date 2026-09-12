"""Deterministic dashboard checks: python3 -m unittest discover -s scripts -p 'test_*.py'."""

import unittest

from generate_claude_code_dashboard import build_dashboard as build_claude_dashboard
from generate_codex_dashboard import build_dashboard as build_codex_dashboard


class CodexDashboardQueriesTest(unittest.TestCase):
    build_dashboard = staticmethod(build_codex_dashboard)

    def test_loki_stats_are_instant_snapshots(self):
        panels = [panel for panel in self.build_dashboard()["panels"] if panel["type"] == "stat"]
        self.assertTrue(panels, "must exercise stat panels")
        for panel in panels:
            with self.subTest(panel=panel["title"]):
                self.assertEqual(panel["datasource"]["type"], "loki")
                self.assertEqual(panel["options"]["graphMode"], "none")
                self.assertTrue(panel["targets"])
                for target in panel["targets"]:
                    self.assertEqual(target["queryType"], "instant")
                    self.assertIs(target.get("instant"), True)

    def test_loki_time_series_keep_range_queries(self):
        panels = [panel for panel in self.build_dashboard()["panels"] if panel["type"] == "timeseries"]
        self.assertTrue(panels, "must exercise time-series panels")
        for panel in panels:
            with self.subTest(panel=panel["title"]):
                self.assertTrue(panel["targets"])
                for target in panel["targets"]:
                    self.assertEqual(target["queryType"], "range")
                    self.assertFalse(target.get("instant", False))


class ClaudeDashboardQueriesTest(CodexDashboardQueriesTest):
    build_dashboard = staticmethod(build_claude_dashboard)
