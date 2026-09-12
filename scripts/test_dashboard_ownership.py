"""Keep the overview free of usage duplication and export deadlines explicit."""
import unittest
from urllib.parse import parse_qs, urlsplit

from dashboard_common import SOURCES
from generate_ai_cli_dashboard import build_dashboard as overview
from generate_claude_code_dashboard import build_dashboard as claude
from generate_codex_dashboard import build_dashboard as codex
from generate_dashboards import build_dashboard as copilot_reports
from generate_opencode_dashboard import build_dashboard as opencode
from generate_vscode_copilot_dashboard import build_dashboard as vscode

BUILDERS = [overview, claude, codex, copilot_reports, opencode, vscode]


class DashboardOwnershipTest(unittest.TestCase):
    def test_overview_has_only_navigation_and_bounded_presence_queries(self):
        d = overview()
        self.assertEqual(d['uid'], 'governance-ai-cli-telemetry')
        queries = [t['expr'] for p in d['panels'] for t in p.get('targets', [])]
        self.assertEqual(len(queries), 3)
        for q in queries:
            self.assertIn('[1h]', q)
            self.assertIn('> bool 0', q)
            self.assertNotIn('job=~".+"', q)
            self.assertNotIn('unwrap', q)
        for uid, _, _ in SOURCES:
            self.assertIn('/d/' + uid, d['panels'][0]['options']['content'])

    def test_editor_metrics_have_one_owner(self):
        expected = {'copilot_chat_lines_of_code_count_total', 'copilot_chat_session_count_total',
                    'copilot_chat_tool_call_count_total', 'copilot_chat_chat_edit_outcome_count_total'}
        editor_exprs = [t['expr'] for p in vscode()['panels'] for t in p.get('targets', [])]
        self.assertEqual(len(editor_exprs), 6)
        for metric in expected:
            self.assertTrue(any(metric in q for q in editor_exprs), metric)
        for build in BUILDERS:
            if build is vscode:
                continue
            for p in build()['panels']:
                for t in p.get('targets', []):
                    self.assertNotIn('copilot_chat_', t.get('expr', ''))

    def test_export_retains_filters_and_extends_request_deadline(self):
        for build in BUILDERS:
            d = build()
            with self.subTest(dashboard=d['uid']):
                link = next(l for l in d['links'] if l['title'] == 'Export PNG')
                url = urlsplit(link['url'])
                self.assertEqual(url.path, '/render/d/' + d['uid'])
                self.assertEqual(parse_qs(url.query)['timeout'], ['180'])
                self.assertNotIn('refresh', parse_qs(url.query))
                self.assertTrue(link['includeTime'])
                self.assertTrue(link['includeVars'])
