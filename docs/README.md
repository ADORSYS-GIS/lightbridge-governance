# Documentation index

| Where | What lives there |
|---|---|
| [`adr/`](./adr/README.md) | Decisions and their consequences. Immutable once accepted. |
| [`rfc/`](./rfc/README.md) | Proposals and specifications. Revised until agreed. |
| [`runbooks/`](./runbooks/README.md) | What to do when something breaks, or when a human has to act on purpose. |
| [`spikes/`](./spikes/) | One-page findings from time-boxed investigations (#7). Evidenced answers, not proposals. |
| [`architecture.md`](./architecture.md) | The system map: components, data flow, where each choice is recorded. |
| [`governance-auth/`](./governance-auth/README.md) | Reference manual for the `governance-auth` credential helper: commands, configuration, files it writes, token exchange, troubleshooting. |

## Which one am I writing?

- Proposing something -> **RFC**.
- Recording a choice that is now settled, with the alternatives it beat -> **ADR**.
- Describing how the pieces fit together, as they currently are -> **architecture.md**.
- Telling a tired person what to type -> **runbook**.
- Documenting every flag, key and file of one component, exhaustively -> a **reference
  manual** like [`governance-auth/`](./governance-auth/README.md). A runbook is one path
  through a tool; a manual is the whole surface.

When a change touches more than one of these, update all of them in the same PR. A stale
architecture doc and a correct one render identically.
