# Fixtures for the normalizer replay harness

Read alongside [`../tests/normalizer_fixtures.rs`](../tests/normalizer_fixtures.rs) (the
harness) and
[`docs/integrations/foundry-golden-fixtures.md`](../../../docs/integrations/foundry-golden-fixtures.md)
(the fuller writeup: design rationale, capture procedure, and what this does and does not
retire from [RFC-0002](../../../docs/rfc/0002-microsoft-foundry-otlp-ingestion.md)'s
Verification section).

## The one thing to not miss

**Nothing under `synthetic/` was captured from a real provider.** Every fixture there is
hand-authored from the attribute names this crate's normalizers already assume --
`model.name`, `tokens.input`, `tokens.output`, `session.id`, `tool.name`, `duration.ms`, plus
Codex's documented `input_token_count`/`output_token_count` -- the same names the RFC-0002
pre-go-live review flagged as **assumed, never verified against what the deployed OTel
Collector actually emits**. A synthetic fixture built from that same assumption cannot verify
it; it can only prove the code does today what its authors believe it should do. Calling
these "golden" would retire that caveat without earning it, so they are not called that
anywhere in this repo -- they are `synthetic/`, and every one of them carries a
`_fixture_meta.provenance` field saying so in the file itself, not just here.

## Two directories, one harness

```
fixtures/
  synthetic/<provider>/<case>.json + <case>.expected.json   # hand-authored, labelled
  captured/<provider>/<case>.json  + <case>.expected.json   # real captures, currently empty
```

`<provider>` is `claude_code`, `codex`, or `foundry` -- matching the module names under
`src/normalizer/`, not the wire provider strings the dispatch table
(`src/normalizer.rs::NORMALIZERS`) keys on (`foundry` here vs. `microsoft_foundry` there;
the harness's own `normalizer_for()` is the one place that mapping lives).

The harness in `tests/normalizer_fixtures.rs` walks both directories identically, by
structure alone. Dropping a real capture into `captured/<provider>/` is a file copy plus a
committed snapshot -- no change to the harness, no change to this crate's `src/`.

## Fixture pair shape

Each case is two files:

- `<case>.json` -- the input, an OTLP JSON payload (real proto3 JSON: attribute arrays,
  decimal-string `int64`s), with one extra top-level `_fixture_meta` key that every
  normalizer already ignores (they only read `resourceSpans`). That key carries the
  honesty label and provenance note for a human reading the file, and (for `synthetic/`)
  what specific behavior the case pins.
- `<case>.expected.json` -- the committed snapshot: `normalize()`'s `Result` serialized as
  `{"ok": <TelemetryPayload>}` or `{"err": "<Display message>"}`.

Snapshots are hand-rolled JSON, not a snapshot-testing crate's format -- no such crate is in
the workspace, and this doesn't need one (see the harness's own module doc for how to
regenerate one after a deliberate change).

## `synthetic/` fixture inventory

Six cases per provider (`claude_code`, `codex`, `foundry`), each pinning one behavior the
connector already claims to handle:

| Case | Pins |
|---|---|
| `valid` | happy path: one execution, one priced model call, one tool call |
| `absent_user_email` | no `user.email` on the resource -> `user_email: null`, not a rejection |
| `missing_token_counts` | no token-count attributes -> `input_tokens`/`output_tokens: null` (unknown cost), never a fabricated `0` (story #31 AC6) |
| `unknown_model` | `model.name` names a model with no pricing row anywhere -> the normalizer passes the string through unmodified; it does not reject, substitute, or zero anything. The "unpriced, not zero" guarantee itself is `governance-core::calculate_model_cost`'s job, already covered by its own DB-gated test (`missing_pricing_is_stored_as_unknown_not_zero` in `crates/governance-core/src/ingest.rs`) -- this fixture only pins the normalizer's half |
| `malformed_events` | `events` is a string, not an array -> rejects (`InvalidFieldType`), never silently treated as "zero tool calls" |
| `end_before_start_timestamp` | `endTimeUnixNano < startTimeUnixNano` -> rejects (`InvalidDuration`), never a clamped/negative duration |

`codex/valid` and `codex/missing_token_counts` deliberately use the **exec-mode** token
attribute names (`input_token_count`/`output_token_count`) rather than the interactive-mode
fallback (`codex.turn.input_tokens`/`codex.turn.output_tokens`), per `codex.rs`'s own module
doc on which path is actually verified (issue #33668).

### `otlp.rs` is covered indirectly, not by its own fixture set

`otlp.rs` has no `Normalizer` impl of its own -- it's the shared parsing layer all three
concrete normalizers call into (attribute-array shape, decimal-string `int64`s, the
`events`-must-be-an-array-or-absent rule). Every fixture above already exercises it through
whichever concrete normalizer owns the directory; `malformed_events` in particular pins
`otlp.rs`'s `events()` behavior identically in all three providers. `otlp.rs` additionally
has its own direct unit tests (in the file itself) for the helpers this harness never calls
in isolation, e.g. `intValue` as a bare JSON number instead of a decimal string. This harness
adds a second, higher-level pin at the "whole payload -> `TelemetryPayload`" boundary; it
does not replace those unit tests.

## `captured/` -- currently empty, waiting for a real export

See the capture procedure in
[`docs/integrations/foundry-golden-fixtures.md`](../../../docs/integrations/foundry-golden-fixtures.md#capture-procedure).
Each provider subdirectory here is a placeholder (`README.md` only, ignored by the harness's
discovery since it doesn't end in `.json`) so the drop-in location is unambiguous the moment
a real capture exists.

## What this retires from RFC-0002, and what it does not

RFC-0002's Verification section asks for "the golden-dataset fixture replays through the
real collector config in CI on every change to normalization, pricing, collector config, or
policy logic." This harness satisfies the "replays on every change to normalization" half,
for the four attribute names it exercises, **at the normalizer boundary only** -- it runs
in `cargo test`, which is already in CI, so a normalization change that alters output for
any pinned case now fails review automatically instead of degrading silently to unknown
cost in production.

It does **not**:

- Verify the attribute contract against a real provider export -- there isn't one in this
  repo yet (see the honesty note above).
- Replay through the real collector config (`redaction -> transform -> batch`) -- this
  harness calls `Normalizer::normalize()` directly on a JSON `Value`, never touching a
  collector.
- Cover pricing or policy logic -- that's `governance-core::ingest`'s job, with its own
  DB-gated tests.

The standing caveat from the pre-go-live review -- that the attribute names are assumed,
not verified -- is **not retired** by this harness. It is retired only once a real capture
lands under `captured/` and this same harness runs it.
