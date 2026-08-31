# Lightbridge Governance — VS Code extension

Contributes governed language models to VS Code chat through the Lightbridge AI
gateway, using VS Code's `LanguageModelChatProvider` API.

**Status: scaffold.** It typechecks, unit-tests and bundles. It has never been
run against a live gateway, and nothing here has been verified inside an
extension host. Treat every claim below about runtime behaviour as designed-for,
not observed.

## Why this exists

[`docs/matrix-row.md`](docs/matrix-row.md) is the RFC-0003 §9 source declaration
and is the thing to read first. The short version: the existing
`GitHub Copilot (IDE)` telemetry row carries no identity in its payload and
cannot be authenticated, because VS Code has no settings key for OTLP headers.
Serving the model instead of observing it makes attribution come from the
credential.

## Design decisions worth knowing before editing

**No credential of its own.** `src/auth.ts` shells out to `governance-auth
token` for every request. This extension implements no OAuth, stores no refresh
token and has no keychain. `governance-auth` already does authorization_code +
PKCE, holds the token at `0600`, locks against concurrent refresh and never
emits a stale credential (ADR-0010, ADR-0012). A second credential path in
TypeScript would be a second thing to get wrong, and it would be the one holding
the long-lived secret.

**Fail closed, everywhere.** Every path out of `getToken` is a token or a throw
— there is no branch returning `undefined` that lets a request proceed
anonymously. `provideLanguageModelChatInformation` contributes **zero** models
when there is no gateway URL, no credential in silent mode, or no reachable
catalogue. It has no stale-catalogue fallback on purpose: serving a cached model
list after the gateway stops answering is how a model that policy has withdrawn
stays selectable.

**Capability comes from the gateway, never from here.** `src/catalogue.ts` maps
`/models/info` into `LanguageModelChatInformation`. A model whose context window
the gateway does not report is skipped, not defaulted — an assumed window is the
defect behind issue #151, and it presents as silent truncation rather than as an
error. Fix those in the gateway catalogue (RFC-0003 §7), not here.

**Nothing logs a body.** `src/redact.ts` is deliberately free of any `vscode`
import so the redaction can be unit-tested with plain `node --test`; a security
control exercisable only by launching an editor stops being exercised. Prompt
and completion text is never logged at any level.

## Sampling parameters

VS Code has no UI for temperature, top-k or top-p, and the provider API cannot
advertise that it accepts them: `modelOptions` is inbound-only and Copilot Chat
never populates it. So `lightbridge.modelOptionDefaults` is the only way a
developer can set them. Anything a caller does pass in `modelOptions` wins.

## Commands

```bash
just ext-install     # npm install, inside ide/vscode only
just ext-check       # typecheck + unit tests
just ext-build       # bundle to dist/
just ext-package     # .vsix
```

`just all-checks` does **not** run these — see the fence comment in the
[justfile](../../justfile). npm stays inside this directory and no CI job invokes
it. If this extension ever gets a release job, that fence stops holding and the
"there is no npm here" line in [CLAUDE.md](../../CLAUDE.md) needs revisiting
rather than quietly becoming false.

## Testing

`src/test/` covers the two pieces that are pure and load-bearing: SSE framing and
redaction. Both were verified by injection — the framing tests were *rewritten*
after the first version passed with a `\n\n` → `\n` bug in place, which is the
whole reason `sse.ts` has tests that look redundant. Two of them are not.

The provider, catalogue and message mapping need an extension host
(`@vscode/test-electron`) and have no coverage yet. That is the largest gap in
this scaffold.

## Known limits

- **Inline completions are unreachable.** BYOK covers chat, agent mode and
  utility tasks; ghost-text completions stay on Copilot's infrastructure. No
  accept/reject or edit-distance signal is available through this API.
- **A Copilot Business/Enterprise admin can disable BYOK**, which makes this
  provider unselectable regardless of what we ship.
- **Images and prompt-tsx parts are dropped** in `src/messages.ts` rather than
  half-encoded, even though the catalogue can advertise `imageInput`.
- **`provideTokenCount` is a documented estimate**, not a tokenizer. It
  over-counts on purpose; the two errors are not symmetric.
