"""Contract for the VS Code Copilot dashboard's user/session filter; no live service required."""
import unittest

from generate_vscode_copilot_dashboard import build_dashboard


class VscodeCopilotDashboardTest(unittest.TestCase):
    def setUp(self):
        self.dashboard = build_dashboard()

    def test_user_variable_is_a_mimir_dropdown_session_stays_textbox(self):
        variables = self.dashboard['templating']['list']
        self.assertEqual([v['name'] for v in variables], ['user', 'session'])
        user_var, session_var = variables
        self.assertEqual(user_var['type'], 'query')
        self.assertEqual(user_var['definition'], 'label_values(copilot_chat_session_count_total, user_email)')
        self.assertTrue(user_var['includeAll'])
        self.assertEqual(user_var['allValue'], '.*')
        self.assertEqual(session_var['type'], 'textbox')
        self.assertEqual(session_var['current']['value'], '')

    def test_every_query_is_scoped_by_user_and_session(self):
        queries = 0
        for p in self.dashboard['panels']:
            for t in p.get('targets', []):
                queries += 1
                with self.subTest(panel=p['title'], ref=t['refId']):
                    self.assertIn('${user:regex}', t['expr'])
                    self.assertIn('${session:regex}', t['expr'])
        self.assertGreater(queries, 0)

    def test_clear_link_resets_to_all_not_blank(self):
        link = next(l for l in self.dashboard['links'] if l['title'] == 'Clear user & session')
        self.assertIn('var-user=All', link['url'])
        self.assertIn('var-session=', link['url'])


if __name__ == '__main__':
    unittest.main()
