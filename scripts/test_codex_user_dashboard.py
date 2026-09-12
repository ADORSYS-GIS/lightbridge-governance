"""Contracts for the verified Codex user/session view; no live service required."""
import unittest
from pathlib import Path

from generate_codex_dashboard import build_dashboard, render


class CodexUserDashboardTest(unittest.TestCase):
    def setUp(self):
        self.dashboard = build_dashboard()
        self.panels = {p['title']: p for p in self.dashboard['panels']}

    def test_committed_output_is_deterministic(self):
        committed = Path(__file__).resolve().parents[1] / 'charts/lightbridge-governance/dashboards/codex-telemetry.json'
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
                    self.assertIn('$__interval' if t['queryType'] == 'range' else '$__range', q)
                    self.assertNotRegex(q, r'\[(?:24h|7d)\]')
                    if p['type'] in ('stat', 'table', 'piechart'):
                        self.assertEqual(t['queryType'], 'instant')
                        self.assertIs(t['instant'], True)
        self.assertEqual(queries, 24)

    def test_activity_excludes_diagnostics_and_stream_chunks(self):
        targets = self.panels['Meaningful activity']['targets']
        self.assertEqual([t['legendFormat'] for t in targets], ['Prompts', 'Model responses', 'Tool calls'])
        for t, event in zip(targets, ['codex.user_prompt', 'codex.sse_event', 'codex.tool_result']):
            self.assertIn('event="' + event + '"', t['expr'])
        self.assertIn('kind="response.completed"', targets[1]['expr'])
        self.assertEqual(self.panels['Meaningful activity']['fieldConfig']['defaults']['unit'], 'short')

    def test_tokens_do_not_add_overlapping_categories(self):
        q = self.panels['Models · tokens']['targets'][0]['expr']
        self.assertIn('unwrap input_tokens', q)
        self.assertIn('unwrap output_tokens', q)
        for field in ('cached_tokens', 'reasoning_token_count', 'tool_token_count', 'cache_write_token_count'):
            self.assertNotIn('unwrap ' + field, q)
        ratio = self.panels['Cached input share']['targets'][0]['expr']
        self.assertIn('unwrap cached_tokens', ratio)
        self.assertIn('unwrap input_tokens', ratio)

    def test_model_rows_become_named_slices_without_timestamp_labels(self):
        for title in ('Models · responses', 'Models · tokens'):
            transforms = self.panels[title]['transformations']
            self.assertEqual(transforms[0], {'id': 'filterFieldsByName', 'options': {'include': {'names': ['model', 'Value #A']}}})
            self.assertEqual(transforms[1]['id'], 'rowsToFields')
            self.assertEqual(transforms[1]['options']['mappings'], [
                {'fieldName': 'model', 'handlerKey': 'field.name'},
                {'fieldName': 'Value #A', 'handlerKey': 'field.value'}])

    def test_waiting_populations_are_separate(self):
        for title in ('Turn waiting time', 'Turn wait · median', 'Turn wait · p95'):
            for t in self.panels[title]['targets']:
                self.assertIn('event="codex.turn_ttft"', t['expr'])
                self.assertIn('unwrap duration_ms', t['expr'])
                self.assertNotIn('unwrap ttft_ms', t['expr'])
        for t in self.panels['Request waiting time']['targets']:
            self.assertIn('kind="response.completed"', t['expr'])
            self.assertIn('unwrap ttft_ms', t['expr'])

    def test_session_join_preserves_missing_signals_and_drilldown(self):
        table = self.panels['Sessions']
        self.assertEqual(table['transformations'][0]['options'], {'byField': 'session', 'mode': 'outerTabular'})
        for t in table['targets']:
            self.assertIn('session!=""', t['expr'])
            self.assertIn('by (session)', t['expr'])
            self.assertNotIn('vector(0)', t['expr'])
        widths = [prop['value'] for override in table['fieldConfig']['overrides']
                  for prop in override['properties'] if prop['id'] == 'custom.width']
        self.assertEqual(len(widths), 8)
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
        self.assertEqual(len(panels), 15)
        self.assertEqual(len({p['id'] for p in panels}), len(panels))
        for i, p in enumerate(panels):
            a = p['gridPos']
            self.assertLessEqual(a['x'] + a['w'], 24)
            for other in panels[i + 1:]:
                b = other['gridPos']
                overlap = a['x'] < b['x'] + b['w'] and b['x'] < a['x'] + a['w'] and a['y'] < b['y'] + b['h'] and b['y'] < a['y'] + a['h']
                self.assertFalse(overlap, (p['title'], other['title']))
        titles = ' '.join(self.panels).lower()
        for invented in ('cost', 'accepted', 'retained', 'active duration', 'approval'):
            self.assertNotIn(invented, titles)


if __name__ == '__main__':
    unittest.main()
