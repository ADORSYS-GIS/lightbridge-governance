# ADR-0008: Money is integer micro-USD, everywhere

- Status: Accepted
- Date: 2026-07-31
- Decision owners: @stephane-segning

## Context

The Foundry spec's normalized execution record carries `"estimated_cost": 0.0124` -- a
float. Its pricing table uses `input_price_per_million`. The Copilot spec has `net_cost`
with a `currency` field.

The platform this reports into already has an answer: the gateway's live spend series is
`gateway_ratelimit_spend_micro_usd`, the rate-limit budgets are expressed in micro-USD, and
`charts/ai-model`'s cost CEL multiplies by 1e6. Lightbridge's grant ledger uses
`amount_micros`.

## Decision

**Every monetary value is an `i64` of micro-USD** (1 USD = 1_000_000). No floating point
appears in any monetary type, column, API field or metric. `governance_core::MicroUsd` is
the only representation.

Pricing tables store micro-USD per million tokens, so the arithmetic stays integral end to
end.

## Consequences

**Positive**
- Copilot spend, Foundry estimated cost, gateway spend and Lightbridge grants are directly
  comparable and can be summed in a single Grafana panel without unit conversion.
- No accumulated rounding error across a month of aggregation, and no
  `assert_eq!` on floats in tests.
- Matches the house rule and the existing ledger.

**Negative**
- Every display path must divide by 1e6. `MicroUsd`'s `Display` does it once, in one place.

**Neutral / follow-ups**
- Cost sourced from a provider is **estimated** until reconciled against that provider's
  billing. Label it so in the API and on the dashboards; do not present it as invoiced spend.
- Currency stays a column but is USD-only for now. Multi-currency is a conversion-rate
  problem, not a representation problem, and this decision does not solve it.

## Alternatives considered

- **Floating-point USD** -- rejected: the source specs' shape, and wrong for money for the
  usual reasons.
- **`NUMERIC` in Postgres, decimal in Rust** -- rejected: correct, but it would not match
  the micro-USD unit the rest of the platform already emits, and the comparability across
  those four sources is the whole point.

## Related

- ADR-0002 (Postgres schema), ADR-0003 (the panels that sum these)
- ai-helm ADR-0028 (cost-recovery pricing), ADR-0070 (the live spend series)
