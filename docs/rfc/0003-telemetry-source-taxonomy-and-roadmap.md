# RFC-0003: Telemetry source taxonomy and integration roadmap

- Status: Draft
- Date: 2026-08-27
- Author: @stephane-segning
- Source of truth: [#95](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/95)
  (AI-powered IDE observability epic), [#30](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/30)
  (per-user usage via native OTLP push), and
  [#96](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/96) (vendor-neutral
  acceptance schema)

## Summary

This platform will ingest AI-usage telemetry from a dozen or more sources that share almost
nothing: some push, some must be polled; some carry a real user identity, some carry none;
some report per request, some only a daily aggregate; some report cost in USD, some in
micro-USD, most not at all. This RFC proposes a **taxonomy** that classifies every source on
three axes — direction, grain, identity origin — a **matrix** placing every known and
anticipated source on those axes, four **authentication patterns** keyed to direction, and a
**declaration gate**: a source declares its row before its code is written. The invariants
this produces are proposed separately as ADR-0013.

## Motivation

The connector backlog is already a dozen issues
([#98](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/98),
[#99](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/99),
[#100](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/100),
[#105](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/105),
[#144](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/144), RFC-0001,
RFC-0002) and there is no written rule for what a new source must satisfy. Three concrete
consequences, all already observed:

**Sources get missed because nobody enumerated them.** GitHub Copilot is *two* integrations
— `governance-auth configure` already writes `github.copilot.chat.otel.*` into VS Code's
settings (a push source, today unauthenticated) and the daily-reports API is a separate pull
source. They have different grain, different identity and different auth, and only the second
one has a ticket. Microsoft Copilot (M365) — distinct from Microsoft Foundry — has no ticket
at all.

**Without a rule, grains get merged and every KPI silently double-counts.** This is not
hypothetical: `lightbridge-authz`'s own ingestion audit
(`docs/research/2026-08-25-genai-usage-ingestion.md`, finding F3) records request-grain access
logs, pre-aggregated OTLP counters and spans landing in one table and being summed together by
the query API, with the signal filter defaulting to "no filter". Their F1, F4, F5 and F6 are
the same class of defect: cost off by 10⁶ because two layers disagreed on units, unknown cost
stored as `0`, non-idempotent ingest that double-bills on an OTLP retry, and PII written
wholesale into a JSONB column on a table with no retention policy.

**Auth gets decided per-source, late, by whoever implements it.** Six of the sources below are
headless cron jobs, each holding a long-lived third-party admin credential. That is the
highest-value secret in the system and it should have one designed pattern, not six incidental
ones.

The purpose of this document is that the thirteenth source costs a mapper and a matrix row,
not a design argument.

## Design

### 1. The three axes

Push-versus-pull is the obvious axis and it is not the primary one — it is a *consequence* of
where the identity comes from. The three that carry weight:

| Axis | Values | What it determines |
|---|---|---|
| **Direction** | push · pull · push-in-band | **Auth**, because it decides whether a human is present to consent |
| **Grain** | request · day-aggregate · seat-snapshot | **Storage**, because mixing grains is what makes KPIs double-count |
| **Identity origin** | bound-at-issuance · payload-asserted · none | **Trust**, because it decides whether attribution is provable |

`push-in-band` was added 2026-08-31 (§2a): the request being measured is itself the telemetry
event, so there is no export to authenticate and identity is bound at issuance. It is the only
Direction value where the auth question answers itself.

A fourth column, **cost units**, is not an axis but must be declared, because the unit differs
per source and the conversion has to live somewhere deliberate.

### 2. The matrix

| Source | Direction | Grain | Identity | Auth | Cost units | Status |
|---|---|---|---|---|---|---|
| Claude Code | push OTLP | request | user, built-in | A | USD *and* µUSD both emitted | [#84](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/84) blocks |
| Codex | push OTLP | request | user, weak | A | none emitted | [#144](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/144) |
| OpenCode | push OTLP | request | user | A | none emitted | `lightbridge-opencode-toolbeit` |
| GitHub Copilot (IDE) | push OTLP | request | **none in payload** | A | none | settings written, **unauthenticated** — see amendment |
| VS Code LM provider | **push, in-band** | request | **bound-at-issuance** | A | µUSD, gateway-computed | built, verified live ([#215](https://github.com/ADORSYS-GIS/lightbridge-governance/pull/215)) |
| Microsoft Foundry | push OTLP | request | integration | B | provider-dependent | RFC-0002 |
| Envoy AI Gateway | push logs | request | Authorino-stamped | D | µUSD | routes to `lightbridge-authz`, not here |
| GitHub Copilot (API) | pull cron | day + seat | org-scoped | C | none — seats | RFC-0001, built |
| Anthropic Platform | pull cron | day | org-scoped | C | USD | not built |
| OpenAI Platform | pull cron | day | org-scoped | C | USD | not built |
| Microsoft Copilot (M365) | pull cron | day | tenant-scoped | C | none — seats | **no ticket** |
| Cursor | pull cron | day | org-scoped | C | — | [#99](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/99) |
| JetBrains AI / Amazon Q / Tabnine | pull cron | day | org-scoped | C | — | [#100](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/100) |

> **Amendment (2026-09-02) — the OpenCode row now has an ingest endpoint.** Pattern A's
> public collector is not one endpoint but **two**, because the OTel Collector's
> `oidcauthextension` accepts exactly one `audience` string per extension instance, and per
> lightbridge-authz ADR-0011 Decision 5 a minted token's `aud` is always exactly the
> requesting `client_id` — so `opencode-cli` tokens can never present `governance-auth-cli`.
> `otel-opencode.ai.camer.digital` (chart values `opencodeOtel`, `governance.source=opencode`)
> was added alongside `otel.ai.camer.digital` (`aiCliOtel`, `governance.source=ai-cli`).
> Proven by probing production 2026-09-02: an OpenCode token against the existing host
> returned `401 … expected audience "governance-auth-cli" got ["opencode-cli"]`, with the
> issuer/signature check passing first. The axes of the row itself are unchanged — still
> push OTLP, request grain, user identity, pattern A. **Not fixed by this:** the Codex and
> Copilot-IDE rows authenticate with `aud: lightbridge-api-key`, which no collector trusts;
> that needs a third audience and is unaddressed. Mapping:
> `docs/integrations/ai-client-flows.md`, "Two public collectors, one audience each".

**The tally, since the rest of this document counts against it.** Thirteen rows:
**A** — 5 (Claude Code, Codex, OpenCode, Copilot IDE, VS Code LM provider) ·
**B** — 1 (Foundry) ·
**C** — 6 (Copilot API, Anthropic, OpenAI, M365, Cursor, JetBrains/Q/Tabnine) ·
**D** — 1 (Envoy).

> **Amendment (2026-08-31).** The thirteenth row was added when the VS Code
> language model provider shipped. It is the first row where **Direction is not
> a separate export**: the inference request *is* the telemetry event, because
> it traverses the gateway. See §2a. The derived counts below are unchanged —
> the new row has a user present, needs no machine-to-machine grant, and holds
> no third-party credential.

Three derived counts, which are not interchangeable:

| Count | Rows | Which |
|---|---|---|
| No user present at any point | **8** | B + C + D |
| Blocked on a machine-to-machine grant | **7** | B + C — D is in-cluster and topologically trusted |
| Holding a long-lived third-party admin credential | **6** | C only |

Two rows deserve emphasis. **GitHub Copilot appears twice** and must stay twice — collapsing
them into one "Copilot connector" is how a request-grain stream and a day-grain aggregate end
up summed. **Envoy AI Gateway is in the table but not in this database**; it is listed so the
boundary is visible rather than inferred (see §6).

### 2a. Amendment — what shipping the VS Code provider measured

Added 2026-08-31. Everything here was **verified against the live system**, not
inferred, and four items correct statements made elsewhere in this document.

**The fourth Direction value: `push, in-band`.** The VS Code LM provider row has
no exporter, no collector, no cron and nothing to retry. The developer's request
goes to the gateway, and the gateway's own record of it *is* the telemetry.
Identity is therefore bound at issuance rather than asserted in a payload, which
makes it the strongest row in the matrix on the axis this taxonomy cares most
about. Either the Direction axis gains this value or the row is documented as
the exception it is; this RFC now names it rather than leaving it implicit.

**Correction 1 — VS Code *does* expose a settings key for OTLP headers.** The
*Risks* section below states authentication "is only possible through an
environment variable in the process VS Code was launched from".
`github.copilot.chat.otel.headers` exists — an `{ "key": "value" }` map,
documented as "applied directly to the OTLP exporter" — alongside
`exporterType` (enum: `otlp-grpc`, `otlp-http`, `console`, `file`), `outfile`,
`captureContent` and `otlpEndpoint`, all `scope: application` with an enterprise
`policyReference`.

It does not rescue that row, for a reason the original text did not anticipate:
the header is **static**, with no helper hook to refresh it, and `settings.json`
is covered by Settings Sync — so a long-lived bearer written there syncs
off-machine. The conclusion stands; the stated reason was wrong.

**Correction 2 — the Copilot IDE row does not double-count against the provider
row.** With Copilot's file exporter enabled and a chat turn sent through the
Lightbridge provider, every `gen_ai.client.token.usage` datapoint was tagged
`gen_ai.provider.name=github` with Copilot's own model — its utility calls —
while the gateway recorded the actual turn. **Copilot meters what it serves, not
what it delegates.** The two rows measure disjoint populations and may both be
enabled without violating §3.

**Correction 3 — the Copilot OTLP stream carries no acceptance signal.** Across
38 records the emitted names were `copilot_chat.session.start`,
`copilot_chat.agent.turn`, `copilot_chat.tool.call` and latency/usage metrics.
There was **no** accept, reject, dismiss, undo or edit-distance event of any
kind. This matters for
[#105](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/105): neither
this row nor the provider row delivers the editor-native decision detail that
spike exists to evaluate, and routing the Copilot spool to the collector would
not change that.

Two incidental findings from the same run, both relevant to anyone building the
spool-and-push path: `captureContent: false` held (no prompt or completion text
in 73KB), and the file exporter writes the OpenTelemetry **JS SDK's internal
object graph** — `_rawAttributes`, `hrTime` pairs, `dataPointType` enums — not
OTLP. A pusher must transform rather than relay, against a private shape that
can change between Copilot releases.

**Correction 4 — `client_credentials` is now advertised.** §4 calls the missing
machine-to-machine grant "the single widest blocker in this RFC", blocking seven
of the rows. The live discovery document at `https://auth.ai.camer.digital`
advertises `authorization_code`, `refresh_token`, `client_credentials`,
`device_code` and `token-exchange`. Whether a client is *provisioned* for it is
a separate question and is not answered here — but the premise that the grant
does not exist is no longer true.

### 3. Grain → storage

Grain, not vendor, partitions storage. A new vendor adds a **mapper**; it must not add a
table.

| Grain | Tables | What a row means |
|---|---|---|
| request | `executions` · `model_calls` · `tool_calls` | one agent run, and the LLM/tool calls inside it |
| day-aggregate | `copilot_org_daily` · `copilot_user_daily` · `copilot_repo_daily` | one vendor-computed daily total |
| seat-snapshot | `copilot_seat_snapshot` | assignment state at a point in time |

The existing schema already honours this, which is why this design starts ahead of the audit
cited above. The day-grain tables are Copilot-named for historical reasons; onboarding
Anthropic, OpenAI, Cursor and M365 means **generalising those three tables by source**, not
adding twelve more.

A query must never sum across grains. One authoritative source per measure.

### 4. Authentication, keyed to direction

| | Pattern | Who authenticates | Mechanism | Principal risk |
|---|---|---|---|---|
| **A** | User-present push | the developer | authz authorization-code + PKCE; short-lived access token, refresh | token at rest in dotfiles; blast radius equals its scope |
| **B** | Hosted-agent push | the integration | authz machine-to-machine — **grant does not exist yet** | credential cannot rotate with a redeploy |
| **C** | Cron pull | **two** credentials: the vendor's, and the collector's own | vendor secret via ESO; collector authenticates to us | a long-lived third-party admin credential at rest |
| **D** | In-cluster | nobody | ClusterIP topology only | anyone who can reach it can fabricate billing rows |

**Pattern A is buildable today.** `lightbridge-authz` became a full IdP (its ADR-0019, 0021,
0023) and its live discovery document now advertises `authorization_endpoint`,
`device_authorization_endpoint`, `introspection_endpoint`, `revocation_endpoint` and the
`authorization_code` grant. `governance-auth` can therefore authenticate directly against it.

**Pattern B is blocked.** That same discovery document advertises no `client_credentials`
grant. ⚠️ **Superseded 2026-08-31 — see §2a, correction 4:** the live document now advertises
it. The text below is left intact per this repo's rule that superseded reasoning is marked,
not deleted, but do not plan against it. **Seven of the twelve rows depend on one** — pattern B's single hosted-agent row plus
all six of pattern C. (An eighth row, the gateway, has no user either, but is covered by
pattern D and needs no grant.) This is the single widest blocker in this RFC.

**Pattern C is the one to design carefully**, because it is the only pattern that holds a
long-lived third-party admin credential, and there are six of them. Every such
`secretKeyRef` must be non-optional: an optional ref lets a pod that beats ESO capture an
empty credential and fail auth until it is restarted by hand.

**Pattern D should not survive.** It is `lightbridge-authz`'s current ingest posture, and
their `docs/lightbridge-query-api.md` states plainly that anyone who can reach that listener
can write fabricated usage records for any account or project. It is listed here to be
retired, not copied.

### 5. Cost-unit normalisation

Units differ per source, so the conversion belongs in **the per-source mapper**, declared in
the matrix row — never in a shared helper that has to guess.

`model_pricing` (`input_per_million_micro_usd` / `output_per_million_micro_usd` /
`effective_from`) already exists, which permits a stronger posture than trusting the emitter:
**recompute** cost from token counts and treat the emitter's own figure as a cross-check that
raises an alert on divergence. That is a design choice this RFC surfaces rather than settles
— see Open question 3.

Per ADR-0008 all money is integer micro-USD. A `NULL` cost means *unknown*; it must never be
stored as `0`, because `0` is a legitimate value and the distinction is load-bearing
downstream.

### 6. The store boundary

> **⚠️ Amendment note (2026-08-31, [ADR-0014](../adr/0014-usage-telemetry-consolidates-into-the-authz-usage-store.md)).
> This entire section is superseded.** The boundary this section proposes — gateway telemetry in
> `lightbridge-authz`, everything else here — was never decided, and the contradiction it flags
> below (lightbridge-authz#491) has now been resolved the other way: by **consolidation**, not by
> a split. Per ADR-0014 and
> [lightbridge-authz ADR-0027](https://github.com/ADORSYS-GIS/lightbridge-authz/blob/main/docs/adr/0027-one-usage-store-partitioned-by-grain.md),
> **all telemetry storage and its query APIs live in the `lightbridge-authz` usage store**, one
> hypertable family per grain with `source` as a dimension column. This repository keeps the
> collectors (`governance-ctl`, `governance-auth`, `redact-extproc`, the `aiCliOtel` chart) and
> they become **clients** of that store's ingest surface; the store, migrations, and query surface
> planned here are decommissioned. The "two stores are not redundant" argument below is answered
> by the consolidated store's execution/tool grains, not by a second store. The text is left
> intact per this repo's rule that superseded reasoning is marked, not deleted.

The gateway's own request telemetry goes to `lightbridge-authz`'s usage store; everything in
this matrix except that row lands here. That boundary was previously *inferred* — the audit
cited above raises it as their open question Q8, noting it is written down nowhere. This RFC
writes it down, and proposes an ADR in each repository so it survives a refactor.

The two stores are not redundant. `usage_events` has no execution grouping key, no tool grain
and no trace correlation, so it cannot answer "executions per developer", "tool-call count per
execution" or "P95 tool latency" — which is what this product sells.

⚠️ **This boundary is currently contradicted by an open epic next door.**
`lightbridge-authz` [#491](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/491) is
titled *"GenAI observability ingestion — usage DB serves the governance KPIs"*, which asserts
the opposite allocation: their store serving this product's KPIs. Both positions are
defensible and only one can be built. This is not a documentation inconsistency to reconcile
in prose — it is a decision that needs making once, in an ADR in each repository, before
either side builds against its own assumption. Until it is made, every estimate on both sides
is conditional.

### 7. Model capability is a shared surface, not per-plugin data

A first-party IDE plugin ([#105](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/105))
needs real model metadata: context window, vision support, tool support. So does Claude Code
([#151](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/151), where the
"unrecognised model" warning is really an assumed-200k-window warning), and so does the
OpenCode toolbeit. Three consumers, one gap.

That data must not live in any of the three. The gateway already serves a model catalogue
(`/anthropic/v1/models`, `/models/info`) sourced from where the model list already lives.
Extending **that** with context window and modality flags gives all three clients one source
of truth; a capability table compiled into a plugin rots silently the first time a model
changes.

### 8. Related work already in flight

This RFC does not start from nothing. Both adjacent repositories have open epics covering
parts of the same surface, and several of them make decisions this document depends on.

**`lightbridge-authz`**

| Issue | Bearing on this RFC |
|---|---|
| [#491](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/491) — GenAI observability ingestion epic | **Contradicts §6.** Asserts their usage DB serves this product's KPIs. |
| [#489](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/489) — P0: `usage_events` is not a hypertable | The F2 finding, filed. Confirms the trap in *Risks*. |
| [#510](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/510) — Epic: one trust root for the platform | The natural home for pattern B's missing grant, though it is not scoped there today. |
| [#508](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/508) — Epic: usage graphs end to end | Overlaps the consumption layer; needs the §6 boundary settled first. |
| [#430](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/430) / PR [#454](https://github.com/ADORSYS-GIS/lightbridge-authz/pull/454) — remove `allowed_models`/`model_policy`/`quota_tier` claims | **Settles a question this repo had open.** Those claims are being deleted, not enforced. Any design here that reads them is building on something being removed. |
| [#421](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/421) — `api_key_id` reused as token-exchange session id causes gateway 403 | Still open. Affects pattern A attribution — the minted token names a principal that does not resolve on introspection. |
| [#427](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/427) — cut OpenCode to the device grant | Pattern A for the OpenCode row. |
| [#481](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/481) — OAuth client registry moves to a cratestack model | Where a per-collector client would be registered under patterns B and C. |

**`converse-frontends`**

| Issue | Bearing on this RFC |
|---|---|
| [#298](https://github.com/ADORSYS-GIS/converse-frontends/issues/298) — Epic: complete the console feature set | The consumption surface these sources feed. |
| [#300](https://github.com/ADORSYS-GIS/converse-frontends/issues/300) — usage dashboards on live data | Blocked on whichever store §6 selects. |
| [#294](https://github.com/ADORSYS-GIS/converse-frontends/issues/294) — usage API: expose latency/percentile fields | Percentile aggregates need `timescaledb_toolkit`, which ties this to the hypertable decision. |
| [#311](https://github.com/ADORSYS-GIS/converse-frontends/issues/311) — model filter from `listModelCatalog` | §7 already has a UI consumer; the capability surface has more than three callers. |
| [#291](https://github.com/ADORSYS-GIS/converse-frontends/issues/291) — Epic: cross-team dependencies for the console | Where asks arising from this RFC should land. |

Two gaps this comparison exposes:

- **Pattern B's machine-to-machine grant has no ticket in any repository.** It blocks seven of
  the twelve rows here and is assumed by nothing upstream. It needs filing before it is planned
  around.
- **Microsoft Copilot (M365) has no ticket either**, in this repo or any other.

### 9. The declaration gate

**A new source declares its matrix row before its implementation is written.** If Direction,
Grain, Identity origin, Auth pattern and Cost units cannot all be filled in, the integration
is not ready to build — the unanswerable column is the design question that was about to be
answered by accident.

This is the enforcement mechanism for everything above, and it is proposed as binding in
ADR-0013.

## Verification

Not "tests pass". The observable outcomes that would show this taxonomy is real:

1. **A source with no row cannot merge.** The gate is visible in review: a PR adding a
   connector without a matrix row is sent back.
2. **Grain separation is provable by query.** For any KPI, exactly one table is authoritative,
   and a query that sums across grains is a reviewable defect rather than an opinion.
3. **Reprocessing is a no-op.** Re-running any pull connector for a day it already ingested
   changes zero row counts. This is the specific property to test — not coverage.
4. **Identity mismatch alerts rather than overwrites.** A payload asserting a different user
   than the credential was issued to raises an alert and keeps the issuance-bound value.
5. **Each auth pattern has a refusal test.** For each of A, B and C, a test asserts that an
   unauthenticated or wrongly-audienced push is *refused* — the unavailable branch must not be
   the permissive branch.
6. **No content, at any grain.** Asserted by a test over the stored columns, not by convention.

## Risks and unknowns

- **Pattern B has no grant.** Six rows depend on a machine-to-machine grant that
  `lightbridge-authz` does not currently advertise. If it is not added, those sources need a
  different mechanism and §4 needs revision.
- **Generalising the day-grain tables is a migration**, and they are Copilot-shaped today.
  Doing it while Copilot is the only live pull source is far cheaper than doing it later.
- **Vendor APIs are not equivalent.** "Pull cron / day / org-scoped" describes the *shape*;
  Anthropic, OpenAI, Cursor and M365 differ in rate limits, backfill windows, and whether they
  expose per-user detail at all. Each needs a spike before its row is treated as settled.
- **Hypertable partitioning interacts with grain.** `create_hypertable()` refuses a table
  whose primary key omits the partition column, and the audit cited above records that
  refusal being swallowed by an `EXCEPTION WHEN OTHERS ... RAISE NOTICE`, leaving a schema
  that had neither chunking nor retention while appearing to have both. Whatever this repo
  does here must assert the hypertable exists after creating it.
- **GitHub Copilot IDE telemetry carries no identity in its payload.** ⚠️ **Partly superseded
  2026-08-31 — see §2a, correction 1:** `github.copilot.chat.otel.headers` *does* exist, so the
  claim that only an environment variable can supply one is wrong. The conclusion survives for a
  different reason: the header is static and `settings.json` syncs off-machine. Attribution for
  that row is still weaker than for any other push source — and §7's plugin, now built, does not
  fix it, because it is a separate row rather than a credential for this one.

## Open questions

1. **What machine-to-machine grant will `lightbridge-authz` offer?** `client_credentials`, or
   per-collector device-code enrolment with a stored refresh token? This blocks patterns B
   and C — seven of the twelve rows. **No issue exists for it in any repository** (§8); the
   nearest home is their [#510](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/510),
   which does not scope it today.

   1a. **Which store does this product's KPIs come from?** §6 and `lightbridge-authz`
   [#491](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/491) currently assert
   opposite answers. Everything in §2 downstream of "where does it land" is conditional on
   this.
2. **Do the day-grain tables get generalised now or per-source later?** Now is cheaper; later
   is less disruptive to the one connector currently in flight.
3. **Is cost recomputed from `model_pricing`, or trusted from the emitter?** Recomputing makes
   the emitter's figure a cross-check and makes cost comparable across vendors; trusting is
   simpler and keeps the vendor's own number authoritative for invoice reconciliation.
4. **Does the GitHub Copilot IDE row stay?** ⚠️ **Re-framed 2026-08-31 — see §2a.** The plugin
   exists now and does *not* secure this row; it is a separate row with its own credential. What
   the measurements changed: this row does not double-count against the provider row (correction
   2), and it carries no acceptance signal (correction 3) — so its original justification, the
   editor-native detail in #105, is not something it delivers. The question is no longer "secure
   it or drop it" but **"what is it for, now that we know what it contains?"**
5. **Is Microsoft Copilot (M365) in scope?** It has no ticket. Its Graph API surface needs
   verification before a row is treated as settled.
6. **Does `IdentityMap` cover cross-vendor identity resolution** for a developer who appears as
   a GitHub login, an Anthropic org member and an OIDC subject — or does that need its own
   design?

## Decisions produced

- ADR-0013 — the six ingest invariants and the declaration gate. Proposed alongside this RFC.
- An ADR in this repository and one in `lightbridge-authz` recording the store boundary
  (§6). Not yet drafted.
