# ADR-0013: Bind every telemetry source to six ingest invariants, declared before implementation

- Status: Proposed
- Date: 2026-08-27
- Decision owners: @stephane-segning

## Context

This platform will ingest AI-usage telemetry from a dozen or more sources that agree on
almost nothing — direction, grain, identity origin and cost units all differ per source
(RFC-0003 §2). The connector backlog is already
[#98](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/98),
[#99](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/99),
[#100](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/100),
[#105](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/105),
[#144](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/144), RFC-0001 and
RFC-0002, and there is no written rule a new one must satisfy. Each has so far been designed
on its own terms, by whoever implemented it.

The cost of not having such a rule is documented next door, in detail.
`lightbridge-authz`'s ingestion audit (`docs/research/2026-08-25-genai-usage-ingestion.md`)
found six defects in a store built the same way: cost off by 10⁶ because two layers disagreed
on units (F1); a table that was never a hypertable because `create_hypertable()` refused it
and the migration swallowed the error (F2); every KPI double-counting because request-grain,
pre-aggregated and span data landed in one table and were summed together (F3); unknown cost
stored as `0`, destroying a distinction both consumers rely on (F4); non-idempotent ingest
that double-bills whenever an OTLP exporter retries (F5); and PII written wholesale into a
JSONB column on a table with no retention policy (F6).

None of those are exotic. Each is what happens when a source is onboarded without a rule that
says otherwise. This repository already satisfies several of them by construction — grain is
already partitioned across `executions`/`model_calls`/`tool_calls` versus the daily and seat
tables, and money is already integer micro-USD (ADR-0008) — but by convention rather than by
decision, which is precisely how it would be lost.

## Decision

**Adopt six invariants binding on every telemetry source, and gate implementation on a
declared taxonomy row.**

### The invariants

1. **Grain partitions storage; vendor never does.** One table per grain (request,
   day-aggregate, seat-snapshot). Onboarding a vendor adds a *mapper*. A pull request that
   adds a vendor-named table is rejected on that ground alone.

2. **Identity is bound at credential issuance.** The principal is whatever the credential was
   issued to. Identity present in a payload — `user.email`, `sub`, a vendor login — is a
   **cross-check that raises an alert on mismatch**, never the value stored, and never
   sufficient on its own.

3. **Money is integer micro-USD, and `NULL` means unknown.** Never `0`. The per-source mapper
   owns unit conversion and declares its input unit in the matrix row. (Extends ADR-0008 to
   the ingest path.)

4. **Every row carries a deterministic dedup key, and writes are upserts.** Reprocessing a day
   must not change row counts. This is a property to test directly, not to infer from
   coverage.

5. **One authoritative source per measure.** A query must never sum across grains. Where two
   sources could answer the same question, the matrix names which one does.

6. **No content, at any grain.** Prompts, completions, tool arguments and tool results are
   never stored. Not truncated, not hashed, not behind a flag.

### The gate

**A source declares its RFC-0003 matrix row — Direction, Grain, Identity origin, Auth
pattern, Cost units — before its implementation is written.** If any column cannot be filled
in, the integration is not ready to build: the unanswerable column is a design question that
was about to be answered by accident, in code, by whoever got there first.

Storage-level invariants that depend on the physical schema (partition-key compatibility,
retention, compression) must be **asserted after being applied**, not assumed from a
statement that ran without raising. A migration that cannot fail is not a migration.

## Consequences

**Positive**

- The thirteenth source costs a mapper and a matrix row, not a design argument.
- The six failure modes above become reviewable in the diff rather than discoverable in
  production. Each maps to exactly one invariant.
- Grain separation makes the KPI layer honest by construction: "which table answers this?"
  has one answer.
- The gate surfaces auth *before* implementation, which is where pattern B's missing
  machine-to-machine grant (RFC-0003 §4) becomes visible as a blocker instead of as an
  improvised workaround.
- Invariant 2 makes attribution defensible. A vendor payload asserting a different user is
  evidence of a problem, not an update.

**Negative**

- Invariant 1 forces a migration that is not otherwise urgent: the day-grain tables are
  Copilot-named today and must be generalised by source. Cheaper now, while Copilot is the
  only live pull source, than after four more land.
- The gate adds a step before coding, and will occasionally block an integration that "just
  needs a quick script".
- Invariant 6 forecloses features that would need content — prompt-quality scoring, semantic
  clustering of failures. That is deliberate, and it is a real capability being given up.
- Invariant 4 requires every source to have a natural key. For a vendor API that exposes none,
  finding one is work, and it may constrain which endpoint is used.

**Neutral / follow-ups**

- RFC-0003's open questions stay open; this ADR does not settle them. In particular the
  machine-to-machine grant (Q1) and cost recomputation (Q3) are decisions this ADR depends on
  but does not make.
- The store boundary with `lightbridge-authz` (RFC-0003 §6) needs its own ADR in each
  repository.
- Invariant 3 interacts with a possible `model_pricing` recomputation posture; if cost is
  recomputed rather than trusted, the emitter's figure becomes a seventh cross-check.
- Microsoft Copilot (M365) has no ticket and no verified API surface; it cannot be gated until
  it is filed.

## Alternatives considered

- **Per-connector design, no shared rule** — the status quo. Rejected because it is exactly
  what produced the six defects next door, in a store with the same purpose and a comparable
  team. The failure is not attributable to inattention; it is what the absence of a rule
  produces at this number of sources.

- **One table with a `signal_type` discriminator and a JSONB tail.** Rejected: it is the shape
  the audit found, and its F3 is intrinsic — the discriminator is an *optional filter*, so
  every query that forgets it double-counts, and forgetting is the default. A schema whose
  correct use depends on remembering a filter will be used incorrectly.

- **One table per vendor.** Rejected: it makes vendor onboarding a schema change and makes
  every cross-vendor query a union that grows without bound. It also makes invariant 5
  unenforceable, since two vendor tables can answer the same question with different numbers.

- **Invariants as lint rules rather than an ADR.** Rejected as insufficient, not wrong. Three
  of the six (grain partitioning, identity binding, no content) are not mechanically
  detectable in general. Where a lint *can* enforce one, it should — but the rule has to exist
  before it can be encoded.

- **A looser gate — declare the row in the PR rather than before implementation.** Rejected
  because the value is in surfacing the unanswerable column while the design is still cheap to
  change. Declaring afterwards documents a decision already made in code.

## Related

- RFC: [`docs/rfc/0003-telemetry-source-taxonomy-and-roadmap.md`](../rfc/0003-telemetry-source-taxonomy-and-roadmap.md)
- RFC: [`docs/rfc/0001-github-copilot-connector.md`](../rfc/0001-github-copilot-connector.md),
  [`docs/rfc/0002-microsoft-foundry-otlp-ingestion.md`](../rfc/0002-microsoft-foundry-otlp-ingestion.md)
- ADR: [`0008-money-is-integer-micro-usd.md`](./0008-money-is-integer-micro-usd.md) — invariant 3
  extends it to ingest
- ADR: [`0009-cratestack-only-rest-transport-cbor-payloads.md`](./0009-cratestack-only-rest-transport-cbor-payloads.md)
  — `ingest_telemetry`'s raw SQL is the sanctioned escape hatch the storage invariants are
  applied through
- `lightbridge-authz` `docs/research/2026-08-25-genai-usage-ingestion.md` — the audit whose six
  findings this ADR is written against
