# Verify (or refute) VS Code Copilot's lines-of-code / edit-outcome metrics

**Status: executed on 2026-09-15. Two different findings, not one.** Lines
of code was a real usage gap, now closed by exercising the workflow once.
The edit-outcome metric was a genuine naming bug in the dashboard --
`docs/integrations`-style fixed in `scripts/generate_vscode_copilot_dashboard.py`.
See [Observed result](#observed-result-2026-09-15).

## Observed result (2026-09-15)

Live check, real accept action (Copilot Chat's agentic edit mode, a
one-line comment added to a real `.rs` file, an explicit accept dialogue
confirmed by the repo owner, autosave on):

| Metric queried by the dashboard | In Mimir after the trigger? | What it actually was |
|---|---|---|
| `copilot_chat_lines_of_code_count_total` | **Yes** -- `type="added"`=1, `type="removed"`=0, `copilot_chat_language_id="rust"` | Usage gap. The metric name was always correct; nobody had exercised the accepted-edit workflow in the window originally checked. |
| `copilot_chat_chat_edit_outcome_count_total` | **No** -- absent even after the explicit accept, even with autosave | Wrong metric name. Not what VS Code Copilot Chat actually emits. |

The real metric for accept/reject decisions, found by an unconstrained
tenant-wide name search (`{__name__=~".*(edit|outcome).*"}`, no job/label
filter) after both hypotheses above (no accept step; needs an explicit
save) were ruled out by the repo owner directly:

```
copilot_chat_edit_acceptance_count_total{
  copilot_chat_edit_outcome="accepted",
  copilot_chat_edit_source="chat_editing_hunk",
  copilot_chat_language_id="rust",
} = 1
```

This matches the official docs' `copilot_chat.edit.acceptance.count`
("Records edit accept and reject decisions") -- a *different* metric from
`copilot_chat.chat_edit.outcome.count` ("File-level chat editing session
outcomes"), which the dashboard had queried. Both are real, documented
names; the generator had simply queried the wrong one. The same
unconstrained search also turned up `copilot_chat_edit_survival_four_gram_*`
and `copilot_chat_edit_survival_no_revert_*` (matching
`copilot_chat.edit.survival.four_gram`/`.no_revert`) as real, live series --
not investigated further here, but worth knowing they exist if a future
dashboard pass wants edit-durability panels.

**Fix applied:** `scripts/generate_vscode_copilot_dashboard.py`'s "Edit
acceptance rate" panel now queries `copilot_chat_edit_acceptance_count_total`
instead. Regenerated JSON, `scripts/test_dashboard_ownership.py`'s
`test_editor_metrics_have_one_owner` updated to expect the real name, both
verified: `python3 -m unittest discover -s scripts -p 'test_*.py'` -- 30
passed. The corrected expression evaluated against Mimir immediately after
the fix returned `NaN` (expected and harmless: `increase()` over a series
with exactly one sample in the whole range has no earlier point to diff
against; the panel's own `NO_DATA_MAPPING` renders this as "NO DATA", not
a misleading zero, and it resolves to a real ratio once a second sample
lands on the next periodic export).

## Why this exists

`scripts/generate_vscode_copilot_dashboard.py` queried
`copilot_chat_lines_of_code_count_total` and
`copilot_chat_chat_edit_outcome_count_total`. Its own docstring claimed
"these copilot_chat_* counters were confirmed against production." Checked
live on 2026-09-15, before the trigger below: **both were completely absent
from Mimir**, unconstrained by any label, over the full 7-day window.

This was a different shape of problem from Claude Code (#335) or Codex
(`verify-codex-metrics-temporality.md`), and the evidence already ruled out
the same root cause:

- `job="copilot-chat"` had 28 *other* distinct metric names in that same
  7-day window, all real and populated: `copilot_chat_session_count_total`,
  `copilot_chat_tool_call_count_total`, `copilot_chat_agent_turn_count_bucket`
  (a genuine Histogram -- these cannot silently misrepresent under the wrong
  temporality the way a Sum can), `gen_ai_client_token_usage_*`, and more.
  A systemic delta-vs-cumulative bug would affect every Sum from this client
  uniformly, not two specific ones while 26 others work.
- The two missing metric names matched the [official VS Code monitoring
  docs](https://code.visualstudio.com/docs/agents/guides/monitoring-agents)
  exactly (dot-to-underscore conversion, correct `_total` suffix) -- not a
  wrong-name guess going in, which is exactly why the second finding above
  (the name that's actually real is a *third*, different documented name)
  was worth writing down rather than assuming a typo.
- The dedicated `lightbridge-governance-copilot-otel` collector's own deployed
  config (`kubectl get opentelemetrycollector lightbridge-governance-copilot-otel
  -n governance -o jsonpath='{.spec.config}'`) has exactly one processor,
  `resource` (renames `service.name`, strips a few k8s/process attributes) --
  no filter, no metricstransform. Nothing in this org's own pipeline drops
  any metric name.
- Fleet `target_info` for `job="copilot-chat"` shows three real installed
  versions across real engineers: `0.61.0`, `0.64.1`, `0.65.0`. The
  `microsoft/vscode-copilot-chat` repo is archived (as of 2026-05-20) and its
  public `CHANGELOG.md` stops at `0.41` (2026-03-25) -- well before any
  version in this fleet -- so it could not have confirmed or ruled out
  either finding above from changelog alone; the live Mimir check did.

Per the docs, both original metrics specifically require Copilot Chat's
*file-editing* workflow (not just asking a question or calling a read-only
tool) -- which is exactly why nobody had triggered either one recently, and
why a deliberate real accept was the right next step rather than more
static analysis.

## The check (as run)

1. Confirmed `governance-auth doctor` reports all checks passed first --
   an out-of-date binary can reject its own config file outright, as
   happened on 2026-09-15: `no_claude`/`no_codex`/`no_vscode`/
   `codex_telemetry_only` were added to the config schema in v2.8.0 (PR
   #329); a v2.7.0 binary refuses to start with a config file a newer
   `configure` already wrote. Fix: `governance-auth self update`.
2. In VS Code, used Copilot Chat's agentic edit mode to propose a real
   one-line comment addition to `app/redact-extproc/src/main.rs`.
3. Accepted the edit via the explicit accept/reject dialogue Copilot Chat
   presented (confirmed present -- ruling out "no decision point exists"
   as an explanation for the missing metric). Autosave was already on
   (ruling out "needs an explicit save" too).
4. Waited a few minutes for the periodic metrics export, then queried
   Mimir directly for the two originally-assumed names, and -- once those
   came back only half right -- for an unconstrained `edit`/`outcome`
   name search across the whole tenant to find what was actually real.
