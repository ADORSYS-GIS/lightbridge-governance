# Source declaration: VS Code language model provider

RFC-0003 §9 and ADR-0013 require a source to declare its matrix row **before**
its implementation is written. This file is that declaration for the extension
in `ide/vscode/`.

It is written here rather than merged into
[`docs/rfc/0003-telemetry-source-taxonomy-and-roadmap.md`](../../../docs/rfc/0003-telemetry-source-taxonomy-and-roadmap.md)
because adding a row to that matrix — and in particular deciding what happens to
the existing `GitHub Copilot (IDE)` row — is a maintainer decision, not a
scaffolding detail. See *Open decisions* at the bottom.

## The row

| Source | Direction | Grain | Identity | Auth | Cost units | Status |
|---|---|---|---|---|---|---|
| VS Code LM provider | **push, in-band** | request | bound-at-issuance | A | µUSD, gateway-computed | proposed |

Every column, with the reasoning:

**Direction — push, in-band.** This is not the push/pull the matrix was built
around. The telemetry is not a separate export at all: the inference request
*is* the telemetry event, because it traverses the gateway. There is no
collector, no cron, and nothing to retry. If a fourth direction value is
warranted, this row is the reason.

**Grain — request.** One chat turn, one gateway request, one row. It lands in
`executions` / `model_calls` per RFC-0003 §3. No aggregation happens client-side,
so there is no grain to merge and nothing to double-count.

**Identity — bound-at-issuance.** The extension holds no credential of its own.
It shells out to `governance-auth token`, which returns a token minted by the
authorization_code + PKCE flow against the IdP. The principal is fixed when the
token is issued and the payload never asserts one. This is the column that makes
the row worth building.

**Auth — pattern A (user-present push).** Unchanged from the existing pattern A
rows; the developer consents once at `governance-auth login` and the refresh
token lives at `0600` under `governance-auth`'s existing layout (ADR-0012).

**Cost units — µUSD, computed at the gateway.** The extension neither reports
nor estimates cost. It has no pricing table and must not acquire one; per
ADR-0008 the µUSD figure is the gateway's to compute. Note that
`provideTokenCount` in this extension is an *estimate* used only for VS Code's
own prompt budgeting — it is not a billing input and must never become one.

## What this row is for

RFC-0003's open question 4 asks whether the `GitHub Copilot (IDE)` row survives.
That row is the weakest in the matrix: push OTLP, request grain, **no identity in
the payload**, unauthenticated, and §5 of the RFC's *Risks* section records that
VS Code exposes no settings key for OTLP headers, so authentication is only
possible through an environment variable in the process VS Code was launched
from.

This row answers that question by a different route than the RFC anticipated.
Rather than observing Copilot's telemetry from the side, the extension *is* the
model — so attribution comes from the credential rather than from a payload
field, and the env-var problem does not arise because there is no separate
export to authenticate.

It also satisfies RFC-0003 §7 by construction: model capability (context window,
vision, tool calling) is fetched from the gateway's `/models/info` on every
catalogue refresh and is never compiled into the extension. A model whose context
window the gateway does not report is **skipped rather than defaulted** — see
`src/catalogue.ts`, and issue #151 for what an assumed window costs.

## What this row does NOT cover

Stated plainly, because the gap is easy to miss and issue
[#105](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/105) is
written around the part that is missing.

**Inline completions are out of scope and cannot be brought in.** VS Code's BYOK
path covers chat, agent mode and utility tasks. Ghost-text completions stay on
Copilot's own infrastructure ([microsoft/vscode#318545](https://github.com/microsoft/vscode/issues/318545)
is the open upstream ask). So this row yields **no** accept/reject signal, **no**
edit-distance, and **no** time-to-decision — which is exactly the editor-native
detail #105 exists to evaluate. Those require a different mechanism and remain
unsolved.

**Coverage is opt-in per developer.** A developer who leaves the model picker on
a Copilot-hosted model produces nothing for this row. It measures what flows
through us, which is a different population from "what the team did in the IDE",
and any KPI built on it has to say which.

## Measured facts that correct RFC-0003

All three were verified against the live system on 2026-08-31, not inferred.

**1. RFC-0003's *Risks* section is wrong about OTLP headers.** It states VS Code
"exposes no settings key for OTLP headers — authentication is only possible
through an environment variable". `github.copilot.chat.otel.headers` **exists**
(`{ "key": "value" }` map, "Applied directly to the OTLP exporter"), alongside
`exporterType` (enum including `file`), `outfile`, `captureContent` and
`otlpEndpoint`. It does not rescue that row, because the header is *static* and
settings.json is covered by Settings Sync — a long-lived bearer there would sync
off-machine — but the stated fact needs correcting.

**2. Copilot's OTel does not meter turns served by this provider.** With the
file exporter enabled and a chat turn sent through `governed-sonnet`, every
`gen_ai.client.token.usage` datapoint was tagged
`gen_ai.provider.name=github` / `gen_ai.request.model=gpt-4o-mini-…` — Copilot's
own utility calls — while the gateway logged the actual turn. **Copilot
instruments what it serves, not what it delegates.** So this row and the
`GitHub Copilot (IDE)` row do not double-count usage, which was the concern that
made open decision 1 look urgent.

**3. The Copilot OTel spool carries no acceptance signal.** In 38 records the
emitted names were `copilot_chat.session.start`, `copilot_chat.agent.turn`,
`copilot_chat.tool.call`, and latency/usage metrics. There was **no**
accept/reject/dismiss/undo/edit-distance event of any kind. So routing that
spool to the collector does not, on this evidence, deliver the editor-native
signal [#105](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/105)
is about — that gap remains open, for both this row and that one.

Two incidental notes from the same run: `captureContent: false` held (no prompt
or completion text in 73KB, only event names and token counts), and the file
exporter writes the OpenTelemetry **JS SDK's internal object graph**
(`_rawAttributes`, `hrTime` pairs, `dataPointType` enums), not OTLP — so any
pusher must transform rather than relay, against a private shape that can change
between Copilot releases.

## Open decisions

Reserved for a maintainer; none of them are settled by this scaffold.

1. **Does this row replace the `GitHub Copilot (IDE)` row, or sit beside it?**
   Measurement above says they do not overlap on usage — Copilot meters only what
   it serves — so "beside it" is now the cheap answer. What still needs deciding
   is whether the Copilot row is worth keeping at all, given finding 3: its spool
   carries usage and orchestration, not the acceptance signal that was its
   justification.
2. **Does a fourth `Direction` value exist?** "Push, in-band" is not push-as-
   export. Either the axis gains a value or this row is documented as an
   exception.
3. **Does this ship to the VS Code Marketplace?** That decides publisher
   identity, release wiring, and whether npm becomes a CI dependency of this
   repository — see the fence in `ide/vscode/.gitignore` and the note in
   `CLAUDE.md`.
4. **Enterprise policy exposure.** A Copilot Business/Enterprise administrator
   can disable the "Bring Your Own Language Model Key in VS Code" policy, which
   makes this provider unselectable. Whether that is acceptable for a governance
   product sold to those same administrators is a product call.
