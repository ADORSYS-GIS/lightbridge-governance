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

## House rules

- **Say what you observed, not what you assume.** "The board is blank" and "no telemetry is
  arriving" are different claims; the runbooks separate them deliberately.
- **A green check that never looked is worse than a red one.** Several steps below exist
  only to distinguish "healthy" from "did not run".
- Commands assume `zsh`, the Hetzner workload kubeconfig for workloads, and
  `--context admin@homeos` for anything ArgoCD.
