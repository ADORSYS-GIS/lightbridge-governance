# Runbooks

One page per thing that goes wrong or that a human has to do on purpose. Each starts with
the **symptom** as it is actually observed -- an alert name, a dashboard that is blank, a
user complaint -- not with the subsystem name, because at 3am you know the symptom.

| Runbook | Open it when |
|---|---|
| [copilot-sync-failed.md](./copilot-sync-failed.md) | The sync alert fires, or the Copilot boards stop moving |
| [onboard-a-foundry-integration.md](./onboard-a-foundry-integration.md) | Someone needs an OTLP endpoint and token for a hosted agent |
| [revoke-an-integration-token.md](./revoke-an-integration-token.md) | A token leaked, or an integration is being retired |
| [replay-from-the-raw-archive.md](./replay-from-the-raw-archive.md) | Normalized data is wrong or missing and the source objects are intact |
| [otel-daemon-misrouted-signals.md](./otel-daemon-misrouted-signals.md) | Codex metrics receive log decoder errors; includes Claude Code and Copilot endpoint checks |
| [otel-daemon-wedged.md](./otel-daemon-wedged.md) | The local `serve --otel` daemon stops forwarding telemetry, logging repeated "collector refused ... held, not discarded" |
| [verify-codex-metrics-temporality.md](./verify-codex-metrics-temporality.md) | Confirmed: Codex has the same delta-vs-cumulative gap Claude Code had (#335), but the env-var fix that worked there doesn't work for Codex -- needs a `deltatocumulative` processor downstream instead |
| [verify-vscode-copilot-edit-metrics.md](./verify-vscode-copilot-edit-metrics.md) | The VS Code Copilot dashboard's lines-of-code/edit-outcome panels show no data, and it isn't the same bug as Claude Code/Codex -- sibling counters from the same client work fine |

## House rules

- **Say what you observed, not what you assume.** "The board is blank" and "no telemetry is
  arriving" are different claims; the runbooks separate them deliberately.
- **A green check that never looked is worse than a red one.** Several steps below exist
  only to distinguish "healthy" from "did not run".
- Commands assume `zsh`, the Hetzner workload kubeconfig for workloads, and
  `--context admin@homeos` for anything ArgoCD.
