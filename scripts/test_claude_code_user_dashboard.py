"""Contracts for the reshaped Claude Code user/session view; no live service required."""
import unittest
from pathlib import Path

from generate_claude_code_dashboard import build_dashboard, render


class ClaudeCodeUserDashboardTest(unittest.TestCase):
    def setUp(self):
        self.dashboard = build_dashboard()
        self.panels = {p['title']: p for p in self.dashboard['panels']}

    def test_committed_output_is_deterministic(self):
        committed = Path(__file__).resolve().parents[1] / 'charts/lightbridge-governance/dashboards/claude-code-telemetry.json'
        self.assertEqual(render(self.dashboard), committed.read_text())
        self.assertEqual(self.dashboard, build_dashboard())

    def test_filters_and_time_window_reach_every_usage_query(self):
        queries = 0
        for p in self.panels.values():
            for t in p.get('targets', []):
                queries += 1
                q = t['expr']
                with self.subTest(panel=p['title'], ref=t['refId']):
                    if p['title'] == 'Identity coverage · fleet':
                        self.assertNotIn('${user', q)
                        self.assertNotIn('${session', q)
                    else:
                        self.assertIn('${user:regex}', q)
                        self.assertIn('${session:regex}', q)
                    if p['type'] != 'logs':
                        # Loki targets carry `queryType`; the Mimir/Prometheus
                        # targets (Lines added/removed, Active time, Commits,
                        # Pull requests, Lines of code over time) carry only
                        # the `instant`/`range` booleans instead -- same shape
                        # generate_vscode_copilot_dashboard.py's own Prometheus
                        # panels already use. Either convention answers "is
                        # this a range query".
                        is_range = t.get('queryType') == 'range' or t.get('range') is True
                        self.assertIn('$__interval' if is_range else '$__range', q)
                    self.assertNotRegex(q, r'\[(?:24h|7d)\]')
                    if p['type'] in ('stat', 'table', 'piechart'):
                        self.assertIs(t.get('instant'), True)
                        if 'queryType' in t:
                            self.assertEqual(t['queryType'], 'instant')
        self.assertGreater(queries, 0)

    def test_activity_excludes_housekeeping_events(self):
        targets = self.panels['Meaningful activity']['targets']
        self.assertEqual([t['legendFormat'] for t in targets], ['Prompts', 'Model calls', 'Tool calls'])
        for t, event in zip(targets, ['user_prompt', 'api_request', 'tool_result']):
            self.assertIn('attributes_event_name="' + event + '"', t['expr'])
        for diagnostic in ('hook_execution', 'mcp_server_connection', 'plugin_loaded', 'retention_sweep'):
            for t in targets:
                self.assertNotIn(diagnostic, t['expr'])

    def test_cache_tokens_are_not_turned_into_an_unverified_ratio(self):
        top_titles = [p['title'] for p in self.dashboard['panels'] if p['gridPos']['y'] in (3, 7)]
        self.assertIn('Cache read tokens', top_titles)
        q = self.panels['Cache read tokens']['targets'][0]['expr']
        self.assertIn('unwrap attributes_cache_read_tokens', q)
        self.assertNotIn('attributes_input_tokens', q)  # a plain total, not a ratio over input tokens
        tokens_q = self.panels['Models · tokens']['targets'][0]['expr']
        self.assertIn('unwrap attributes_input_tokens', tokens_q)
        self.assertIn('unwrap attributes_output_tokens', tokens_q)
        self.assertNotIn('cache_read', tokens_q)
        self.assertNotIn('cache_creation', tokens_q)

    def test_model_rows_become_named_slices_without_timestamp_labels(self):
        for title in ('Models · responses', 'Models · tokens'):
            transforms = self.panels[title]['transformations']
            self.assertEqual(transforms[0], {'id': 'filterFieldsByName', 'options': {'include': {'names': ['attributes_model', 'Value #A']}}})
            self.assertEqual(transforms[1]['id'], 'rowsToFields')
            self.assertEqual(transforms[1]['options']['mappings'], [
                {'fieldName': 'attributes_model', 'handlerKey': 'field.name'},
                {'fieldName': 'Value #A', 'handlerKey': 'field.value'}])

    def test_tool_decision_source_split_is_present(self):
        q = self.panels['Tool decisions']['targets'][0]['expr']
        self.assertIn('by (attributes_decision, attributes_source)', q)
        edit = self.panels['Code edits · source=config share']['targets'][0]['expr']
        self.assertIn('attributes_tool_name=~"Edit|Write|NotebookEdit"', edit)
        self.assertIn('attributes_source="config"', edit)
        self.assertIn('$__range', edit)
        self.assertNotRegex(edit, r'\[7d\]')

    def test_session_join_preserves_missing_signals_and_drilldown(self):
        table = self.panels['Sessions']
        self.assertEqual(table['transformations'][0]['options'], {'byField': 'attributes_session_id', 'mode': 'outerTabular'})
        for t in table['targets']:
            self.assertIn('attributes_session_id!=""', t['expr'])
            self.assertIn('by (attributes_session_id)', t['expr'])
            self.assertNotIn('vector(0)', t['expr'])
        widths = [prop['value'] for override in table['fieldConfig']['overrides']
                  for prop in override['properties'] if prop['id'] == 'custom.width']
        self.assertEqual(len(widths), 7)
        self.assertLessEqual(sum(widths), 1200, 'all session columns must fit the 1280px export')
        self.assertIn('min_over_time', table['targets'][0]['expr'])
        self.assertIn('max_over_time', table['targets'][1]['expr'])
        self.assertIn('unixEpochMillis', table['targets'][0]['expr'])
        link = table['fieldConfig']['overrides'][-1]['properties'][-1]['value'][0]['url']
        self.assertIn('${__url_time_range}', link)
        self.assertIn('var-user=${user:percentencode}', link)
        self.assertIn('var-session=${__value.raw:percentencode}', link)
        variables = self.dashboard['templating']['list']
        self.assertEqual([v['name'] for v in variables], ['user', 'session'])
        self.assertTrue(all(v['type'] == 'textbox' and v['current']['value'] == '' for v in variables))

    def test_layout_has_no_overlapping_panels_or_invented_measurements(self):
        panels = list(self.panels.values())
        self.assertEqual(len({p['id'] for p in panels}), len(panels))
        for i, p in enumerate(panels):
            a = p['gridPos']
            self.assertLessEqual(a['x'] + a['w'], 24)
            for other in panels[i + 1:]:
                b = other['gridPos']
                overlap = a['x'] < b['x'] + b['w'] and b['x'] < a['x'] + a['w'] and a['y'] < b['y'] + b['h'] and b['y'] < a['y'] + a['h']
                self.assertFalse(overlap, (p['title'], other['title']))
        titles = ' '.join(self.panels).lower()
        # 'lines of code' was in this guard list until 2026-09-14: the
        # metrics-temporality fix (lightbridge-governance#335) made
        # claude_code_lines_of_code_count_total a real, sourced Mimir
        # series, not an invented one -- see test_prometheus_panels_are_
        # sourced_and_scoped below for what backs the panels that use it.
        for invented in ('actual spend', 'accepted', 'retained', 'active duration'):
            self.assertNotIn(invented, titles)

    def test_prometheus_panels_are_sourced_and_scoped(self):
        # The one thing this dashboard could not show before the
        # 2026-09-14 metrics-temporality fix (see the generator's own
        # docstring) -- confirm each panel is real Mimir data, scoped like
        # everything else, not an invented substitute.
        expected = {
            'Lines added': 'claude_code_lines_of_code_count_total',
            'Lines removed': 'claude_code_lines_of_code_count_total',
            'Active time': 'claude_code_active_time_seconds_total',
            'Commits': 'claude_code_commit_count_total',
            'Pull requests': 'claude_code_pull_request_count_total',
            'Lines of code over time': 'claude_code_lines_of_code_count_total',
        }
        for title, metric in expected.items():
            for t in self.panels[title]['targets']:
                self.assertEqual(t['datasource']['type'], 'prometheus')
                self.assertIn(metric, t['expr'])
                # Backtick-quoted (PromQL raw strings, no escape processing),
                # NOT double-quoted -- caught in review (#336): Grafana's
                # `:regex` format escapes a `.` in an email to `\.`, which a
                # double-quoted PromQL string tries to interpret as an
                # escape sequence and breaks on. A double-quoted assertion
                # here would pin that bug instead of catching it.
                self.assertIn('user_email=~`.*${user:regex}.*`', t['expr'])
                self.assertIn('session_id=~`.*${session:regex}.*`', t['expr'])
                self.assertNotIn('user_email=~".*${user:regex}.*"', t['expr'])
                self.assertNotIn('session_id=~".*${session:regex}.*"', t['expr'])
        for title in ('Lines added', 'Lines removed'):
            self.assertIn('type="added"' if title == 'Lines added' else 'type="removed"',
                           self.panels[title]['targets'][0]['expr'])
        for panel_title, mapping in self.panels.items():
            if mapping['type'] == 'stat' and mapping['datasource']['type'] == 'prometheus':
                self.assertIn({'type': 'special', 'options': {'match': 'null+nan',
                    'result': {'text': 'NO DATA', 'color': 'red', 'index': 0}}},
                    mapping['fieldConfig']['defaults']['mappings'],
                    f'{panel_title} must map NO DATA explicitly -- a blank panel here is an '
                    'un-updated machine, never a fabricated zero')
                # increase() over a counter is a fractional extrapolation,
                # not an exact integer reconciliation -- an un-rounded
                # "Commits"/"Pull requests" stat can render 1.14. Caught in
                # review (#336).
                self.assertEqual(mapping['fieldConfig']['defaults']['decimals'], 0, panel_title)

    def test_hooks_mcp_and_retention_are_scoped_not_only_fleet_wide(self):
        # Pre-reshape, these panels were the only ones NOT restricted to the
        # selected user/session -- confirm they now carry the same scoping
        # as everything else (2026-09-14 live check: a single session_id
        # carried hook/MCP/plugin/retention events alongside prompts and
        # api_request, so scoping them the same way is correct, not a
        # regression).
        for title in ('Hook outcomes', 'Top hooks', 'MCP connections · status',
                      'MCP connections · transport', 'Plugins loaded',
                      'Retention sweeps', 'Transcripts deleted',
                      'Session files deleted', 'History entries pruned'):
            for t in self.panels[title]['targets']:
                self.assertIn('${user:regex}', t['expr'])
                self.assertIn('${session:regex}', t['expr'])

    def test_waiting_and_error_populations_use_duration_ms_only(self):
        for title in ('Request wait · p95', 'Request waiting time'):
            for t in self.panels[title]['targets']:
                self.assertIn('unwrap attributes_duration_ms', t['expr'])
        errors = self.panels['API errors']['targets'][0]['expr']
        self.assertIn('attributes_event_name="api_error"', errors)


if __name__ == '__main__':
    unittest.main()
