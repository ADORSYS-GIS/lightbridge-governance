# Spikes

A spike is a bounded, time-boxed investigation that answers a question before we commit to
an approach. It records **what we tried, what we found, and what it means** -- including
negative results, which are expensive to rediscover. A spike is not an ADR (a decision) and
not an RFC (a specification); it is the evidence a later ADR or RFC is built on.

## Index

| # | Title | Ticket | Date |
|---|---|---|---|
| [0007](./0007-github-app-token-on-copilot-reports.md) | GitHub App installation tokens on Copilot report endpoints | [#7](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/7) | 2026-08-02 |
| [0008](./spike-0008-codex-otel-admin-config.md) | Codex admin config cannot pin OTel | [#34](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/34) | 2026-08-04 |

## Writing one

1. Copy `template.md` to `spike-NNNN-short-title.md` (or `NNNN-short-title.md`).
2. Lead with the one-word answer, then the source evidence with `file:line` citations.
3. Spell out the consequence for the downstream story or epic -- a negative result is the
   point, not an afterthought.
4. Add a row to the table above.

## Relationship to ADRs and RFCs

A spike produces evidence; an **ADR** records the decision it informs and freezes it; an
**RFC** specifies what we build. See [`../adr/README.md`](../adr/README.md) and
[`../rfc/README.md`](../rfc/README.md).