# Lightbridge Governance — VS Code extension

Contributes governed language models to VS Code chat through the Lightbridge AI
gateway, using VS Code's `LanguageModelChatProvider` API (finalized in 1.104).

**Status: verified against the live gateway.** All 18 production models map,
with the catalogue fetched and a real credential resolved through
`governance-auth token`. A chat turn has been streamed end to end in a real
editor. Not yet published to the Marketplace.

## Why this exists

[`docs/matrix-row.md`](docs/matrix-row.md) is the RFC-0003 §9 source declaration
and is the thing to read first. The short version: serving the model rather than
observing it makes attribution come from the credential instead of from a
payload field that carries no identity.

## Design decisions worth knowing before editing

**No credential of its own.** [`src/auth.ts`](src/auth.ts) shells out to
`governance-auth token`. This extension implements no OAuth, stores no refresh
token and has no keychain. `governance-auth` already does authorization_code +
PKCE, holds the token at `0600`, locks against concurrent refresh and never
emits a stale credential (ADR-0010, ADR-0012).

**Fail closed, everywhere.** Every path out of `getToken` is a token or a throw.
`provideLanguageModelChatInformation` contributes **zero** models when there is
no gateway URL, no credential in silent mode, or no reachable catalogue — with
no stale-catalogue fallback, because serving a cached list after the gateway
stops answering is how a model that policy withdrew stays selectable.

**Capability comes from the gateway, never from here.**
[`src/catalogue.ts`](src/catalogue.ts) maps `/v1/models/info`. A model whose
context window the gateway does not report is skipped, not defaulted. If
entries come back but none map, that is logged as an **error** naming the likely
cause — a bare "0 models" is the hardest possible symptom to debug, and it is
what a schema drift produces.

**The context window is not passed through verbatim.** VS Code renders
`maxInputTokens + maxOutputTokens` as the total window, while the catalogue's
`context_length` *is* that total. Passing both through advertised 264k for a
200k model. The input budget therefore excludes the output reserve, so the
displayed sum equals the real window.

**`modelOptions` is filtered against the catalogue's `supported_parameters`.**
Copilot Chat populates `modelOptions` with its own internal fields
(`_capturingTokenCorrelationId`, `_otelTraceContext`, `_telemetryTurn`,
`_enableThinking`). Forwarding those relayed Copilot's telemetry identifiers to
our gateway. An empty allowlist forwards nothing rather than everything.

**Nothing logs a body.** [`src/redact.ts`](src/redact.ts) is deliberately free of
any `vscode` import so redaction is testable with plain `node --test`; a control
exercisable only by launching an editor stops being exercised.

## Commands

```bash
just ext-install     # npm ci, inside ide/vscode only
just ext-check       # typecheck + unit + integration
just ext-build       # bundle to dist/
just ext-package     # .vsix
```

`just all-checks` does not run these — the extension has its own path-filtered
workflow, [`vscode-extension.yml`](../../.github/workflows/vscode-extension.yml).
See AGENTS.md for the rule that fence enforces.

## Testing

- `src/test/` — unit tests for SSE framing and redaction. No `vscode` import, so
  they run under plain `node --test`.
- `tests/` — integration scenarios driving the real provider against an
  in-process fake gateway, with `vscode` aliased to a stub at bundle time.
  Covers catalogue mapping, streaming, tool-call reassembly, the `modelOptions`
  allowlist and every fail-closed branch.

**Two tests exist because the obvious version of them proved nothing**, and both
are worth understanding before editing:

- The SSE tests were rewritten after the first four passed with a deliberate
  `\n\n` → `\n` framing bug injected — their payloads happened to contain no
  internal newline. Two of them now pin the terminator specifically.
- The fail-closed scenarios point at a **permissive control probe**, not the
  strict gateway. Against the strict gateway they passed even with a fail-closed
  bypass injected into `auth.ts`, because the gateway's own 401 produced exactly
  the empty list and the throw they asserted on. They were measuring the
  gateway's strictness, not the extension's refusal. The suite also asserts the
  probe was never contacted at all.

If you change either, re-inject the corresponding bug and confirm the test fails
for the reason you predict.

## Known limits

- **Inline completions are unreachable.** BYOK covers chat, agent mode and
  utility tasks; ghost-text completions stay on Copilot's infrastructure
  ([microsoft/vscode#318545](https://github.com/microsoft/vscode/issues/318545)).
  No accept/reject, edit-distance or time-to-decision signal.
- **A Copilot Business/Enterprise admin can disable the BYOK policy**, which
  makes this provider unselectable regardless of what we ship.
- **Images and prompt-tsx parts are dropped** in `src/messages.ts` rather than
  half-encoded, even though the catalogue can advertise `imageInput`.
- **`provideTokenCount` is a documented estimate**, not a tokenizer. It
  over-counts on purpose; the two errors are not symmetric. It is never a
  billing input — cost is the gateway's to compute in integer µUSD (ADR-0008),
  which is also why `pricing` from the catalogue is deliberately not read.
