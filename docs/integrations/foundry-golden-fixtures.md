# Foundry connector: normalizer fixture harness

This document is the fuller writeup for
[`crates/governance-foundry/fixtures/README.md`](../../crates/governance-foundry/fixtures/README.md)
and [`crates/governance-foundry/tests/normalizer_fixtures.rs`](../../crates/governance-foundry/tests/normalizer_fixtures.rs).
Read those two first for the mechanics; this document covers why they exist, the capture
procedure, and — most importantly — what this work does and does not retire from
[RFC-0002](../rfc/0002-microsoft-foundry-otlp-ingestion.md)'s Verification section.

## The gap this addresses

RFC-0002's Verification section requires:

> The golden-dataset fixture replays through the real collector config in CI on every change
> to collector config, normalization, pricing or policy logic.

That fixture never existed. A pre-go-live review flagged it and it has been the standing
caveat on `governance-foundry` since: the attribute names all three normalizers depend on
(`model.name`, `tokens.input`, `tokens.output`, `session.id`, `tool.name`, `duration.ms`,
plus Codex's `input_token_count`/`output_token_count`) are **assumed** from RFC-0002 and each
provider's public OTel documentation, never verified against what the deployed OTel
Collector actually emits. Per that review, a provider renaming an attribute degrades
**silently** to unknown cost (the token-count and model-name extraction paths both fail open
to "absent", not to a hard error) rather than failing loudly — so nothing today would catch
a rename before it reached production.

## What exists now

`crates/governance-foundry/tests/normalizer_fixtures.rs` is a fixture-replay harness: for
every fixture pair under `crates/governance-foundry/fixtures/{synthetic,captured}/<provider>/`,
it runs the input JSON through the matching `Normalizer::normalize()` and diffs the result
against a committed snapshot. It runs as a normal `cargo test` target, so it is already part
of `just test` / `just all-checks` / CI with no additional wiring — any future change to
`claude_code.rs`, `codex.rs`, `foundry.rs`, or the shared `otlp.rs` helpers that alters
normalized output for any pinned case fails this test, and the failure names the exact field
that changed (see [Perturbation test](#perturbation-test-proving-the-harness-catches-a-real-bug)
below for a real example).

This is genuinely useful today: it locks in current, believed-correct behavior so a
regression is caught in code review instead of by a customer's dashboard quietly reading
"unknown cost" more often than it used to.

**It is not the golden-dataset fixture RFC-0002 asks for**, because there is no real
captured payload behind it (see the next section), and because it exercises normalization in
isolation, not "the real collector config" (no collector, no redaction, no transform
pipeline is involved — the harness calls `normalize()` directly on a `serde_json::Value`).
See [What this does and does not retire](#what-this-does-and-does-not-retire) for the precise
scope.

## Why the fixtures are labelled `synthetic/`, not `golden/`

There is no cluster access available to build this, and no real captured OTLP payload exists
in this repository. Every fixture under `crates/governance-foundry/fixtures/synthetic/` is
**hand-authored from the documented attribute contract** — the same names the normalizers
already assume, restated as JSON. That is a real limitation, not a cosmetic one: a fixture
built from the same assumption the code makes cannot verify that assumption against the wire.
It can only prove the code does today what its author believed it should do.

Calling that a "golden dataset" would imply verification against real provider output that
never happened, which would be worse than having no fixture at all — it would look like the
caveat had been retired when it had not. So these fixtures are named and labelled honestly
instead:

- The directory is `fixtures/synthetic/`, never `fixtures/golden/`.
- Every fixture JSON file carries a `_fixture_meta.provenance` field with wording to this
  effect: *"hand-authored from the documented attribute contract ... NOT captured from a real
  \<Provider\> OTLP export. Encodes our ASSUMPTIONS about the wire format, not verified
  reality."*
- `crates/governance-foundry/fixtures/README.md` repeats this at the top, not buried.
- This document repeats it again, here.

## Capture procedure

These are the concrete steps for someone with cluster access to capture a real payload and
turn the standing caveat into an actually-verified one. The `foundry-gateway`
`OpenTelemetryCollector` (the `memory_limiter -> redaction -> transform -> batch` pipeline
RFC-0002 describes) is defined in the `ai-helm` repository
(`charts/core-gateway/templates/otel.yaml`, per RFC-0002's own Design section and ADR-0034) —
**not** in this repository, so none of the following edits anything in this repo.

1. **Add a temporary capture exporter to the collector, after redaction.** Add a `file`
   exporter component (e.g. `file/capture`) to the collector's config, using an OTLP JSON
   encoding, and append it to the *existing* `traces`/`logs`/`metrics` pipeline's exporter
   list — placed **after** the `redaction` processor in that same pipeline, so the captured
   bytes are exactly what the redaction step already permits downstream, never a pre-
   redaction payload. Do **not** create a separate raw-receiver pipeline for this: reusing
   the production pipeline's own exporter list is what guarantees the capture reflects
   post-redaction data, not a bypass of it.
2. **Trigger one controlled execution.** Run a single agent execution (a Foundry hosted
   agent invocation, a `codex exec` run, or a Claude Code session) against a **test**
   tenant/integration, ideally with synthetic prompt content — not a real user's session —
   so there is nothing sensitive to scrub later beyond the identity attributes noted below.
3. **Copy the file out, then remove the exporter.** `kubectl cp` the captured file off the
   collector pod (or off whatever ephemeral volume the `file` exporter wrote to), then revert
   the collector config change so the exporter is gone. This capture point is deliberately
   temporary — it should not persist as a standing feature of the collector config.
4. **Scrub before it leaves your machine.** Replace `user.email` (and any other value that
   identifies a real person or a real workspace) with an obviously-synthetic placeholder
   (`captured-user@example.invalid`), and confirm no prompt/tool content leaked in — it
   shouldn't have, if step 1 captured after redaction and the integration's capture mode was
   `metadata_only`, but confirm before committing rather than trusting that in hindsight.
5. **Drop it in and generate the snapshot.**
   ```text
   cp captured-payload.json crates/governance-foundry/fixtures/captured/<provider>/<case>.json
   GOVERNANCE_FOUNDRY_BLESS_FIXTURES=1 \
     cargo test -p governance-foundry --test normalizer_fixtures
   ```
   Review the generated `<case>.expected.json` by hand — if the real payload's shape differs
   from what the corresponding `synthetic/` fixture assumed (a renamed attribute, a
   differently-shaped `events` array, whatever), **that is the whole point**: it means the
   assumption was wrong, and the fix belongs in the normalizer, not in silently accepting
   whatever the snapshot says.
6. **Commit both files, and update this section** if the real collector's actual config
   differs from what's assumed above (component name, pipeline order, etc.) — this procedure
   is itself unverified against a real collector for the same reason the fixtures are.

### Why this doesn't violate the "never log a body" house rule

AGENTS.md's rule — never log a token, a signed URL, or a request/response body — is about
this platform's *own services* (`lightbridge-governance`, `governance-ctl`, this crate) not
logging what passes through them. A temporary collector-side `file` exporter is neither: it
is a capability of the OTel Collector itself (an infrastructure component, not application
code in this repo), scoped to a single controlled capture window, removed immediately after,
and — critically — placed after the redaction processor so it never sees more than what the
integration's own privacy mode (`metadata_only`/`redacted`/`full`) already permits to reach
Tempo/Loki. It is closer in kind to `kubectl logs` during an incident than to this service
adding a `tracing::info!(body = ?payload)` call. The distinction matters enough to be
explicit about it here rather than assumed.

## Perturbation test proving the harness catches a real bug

To confirm the harness actually catches a regression (not just passes trivially), the
`model_call` span-id suffix in `foundry.rs` was changed from `:mc` to `:model_call` (a
one-line, plausible-looking rename an engineer might make without thinking it changes
observable behavior), the harness was rerun, then the change was reverted. Exact output:

```text
$ cargo test -p governance-foundry --test normalizer_fixtures
4 fixture snapshot(s) mismatched:
synthetic/foundry/absent_user_email: normalizer output no longer matches the committed snapshot at ".../fixtures/synthetic/foundry/absent_user_email.expected.json"
synthetic/foundry/unknown_model: normalizer output no longer matches the committed snapshot at ".../fixtures/synthetic/foundry/unknown_model.expected.json"
synthetic/foundry/missing_token_counts: normalizer output no longer matches the committed snapshot at ".../fixtures/synthetic/foundry/missing_token_counts.expected.json"
synthetic/foundry/valid: normalizer output no longer matches the committed snapshot at ".../fixtures/synthetic/foundry/valid.expected.json"
...
--- expected ---
...
            "span_id": "fy-span-0001:mc",
...
--- actual ---
...
            "span_id": "fy-span-0001:model_call",
...
test normalizer_output_matches_committed_snapshots ... FAILED
```

Exactly the four `foundry/` cases whose normalized output has a `model_calls[].span_id`
field failed (the two error-case fixtures, `malformed_events` and
`end_before_start_timestamp`, correctly did not, since they never reach that line); no
`claude_code/` or `codex/` case failed, since the perturbation was `foundry.rs`-only. The
diff names the exact field (`span_id`) and the exact values (`:mc` vs. `:model_call`) that
moved. The code was then reverted and the suite re-confirmed green
(`git diff --stat crates/governance-foundry/src/normalizer/foundry.rs` empty; full command
output in the PR's verification section).

## What this does and does not retire

**Retired:**

- "There is no automated check that a change to normalization logic hasn't silently altered
  output." A change to any of the four normalizers (or their shared `otlp.rs` helpers) that
  alters output for any pinned case now fails `cargo test` and names the field that moved.
- "The failure modes the connector claims to handle (missing token counts, malformed
  `events`, an end-before-start timestamp, absent `user.email`, an unrecognized model) have
  no committed, reviewable evidence of what actually happens." Each now has one, per
  provider.

**Not retired:**

- **"The attribute names are assumed, never verified against what the deployed OTel
  Collector actually emits."** This is the core of the pre-go-live review's finding and it
  stands exactly as it did before this work — because there is still no real captured
  payload. `synthetic/` fixtures are built from the same assumption the normalizers make, so
  by construction they cannot check that assumption.
- **RFC-0002's literal ask**, "replays through the real collector config" — this harness
  never touches a collector. It calls `Normalizer::normalize()` directly.
- **Pricing and policy logic** — untouched by this work; covered separately by
  `governance-core::ingest`'s own DB-gated test suite.
- **Collector config changes** — RFC-0002 asks for the fixture to replay "on every change to
  collector config" too; this harness has no way to observe a collector config change at all.

The caveat closes in one specific, checkable way: once a real payload is captured (see
[Capture procedure](#capture-procedure) above) and dropped into `fixtures/captured/`, this
same harness — unmodified — verifies it. Until then, treat every `synthetic/` fixture as
documentation of an assumption, not evidence that the assumption is correct.
