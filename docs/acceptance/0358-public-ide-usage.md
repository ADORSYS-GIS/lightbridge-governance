# Public IDE collector → usage-store acceptance (#358)

Source of truth: [governance#358](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/358),
[authz#581](https://github.com/ADORSYS-GIS/lightbridge-authz/issues/581), governance
ADR-0013/0014/0016 and authz ADR-0028 D8. This is an acceptance record, not a declaration of completion.

## Reverified baseline — 2026-09-22

Read-only Kubernetes discovery and a PostgreSQL `BEGIN READ ONLY` transaction found:

- Public AI-CLI and OpenCode collectors: two ready replicas each, contrib `0.160.0`;
  their logs/traces/metrics pipelines export only to `otlp/alloy` and `awss3`.
- Alloy routes traces to Tempo, logs to Loki, and metrics through identity promotion to
  Prometheus. There is no usage exporter in its signal-routing graph.
- Usage service: two ready replicas, image
  `ghcr.io/adorsys-gis/lightbridge-authz-usage:sha-4ed92a655385b4ea85e298290b6fe2c5e30a63ec`.
- `usage_events` in the preceding 24 hours: **20,215**, all `source=eaig`.
- Exact counts: `usage_executions`, `usage_model_calls`, `usage_tool_calls`,
  `usage_day_facts`, `usage_seat_snapshots`, `usage_identities`: **0 each**.
- Usage Service is ClusterIP, ingest port 3000 and query port 3006.

Counts are a point-in-time observation. No tokens, personal identifiers or payload bodies were
selected for this evidence. These checks do not prove a working native IDE path.

A second isolated wire probe returned HTTP 503 twice and HTTP 202 on its third attempt.
Contrib `0.160.0` retried and returned HTTP 200 to the synthetic upstream sender. This exercises
exporter recovery only; it is not an end-to-end production outage rehearsal. The existing Helm
`assert-oidc-auth.sh` passed for both public collectors with their expected audiences.

## Source matrix

| Client | Direction / grain | Canonical source | Identity / authentication | Cost input | Current acceptance |
|---|---|---|---|---|---|
| Claude Code | Push; `claude_code.api_request` logs are request grain | `claude-code` | Authz bearer verified by public collector; stored account/project/user must be derived from verified claims | Prefer integer `cost_usd_micros`; otherwise decimal USD converted to integer micro-USD; absent remains NULL | Blocked: source dispatch, trusted identity, dedup, authoritative measure selection, deployment and real scoped query |
| Codex | Push (via `governance-auth`'s local collector daemon, ADR-0016, the default profile); supported native traces are execution grain; logs require an explicitly tested mapping | `codex` | No separate collector needed: the local daemon forwards Codex's telemetry to `aiCliOtel` under the SAME bearer (`aud: governance-auth-cli`) Claude Code uses, and `otel_daemon/source_stamp.rs` derives a trustworthy per-resource `governance.source` from `event.name` (`codex.*`) before forwarding | Source-specific token units; missing price remains NULL | Daemon-side source stamping implemented and tested; blocked on the authz-side per-resource preference (see "Trusted source decision" below) before `aiCliOtel.usageExport` can safely enable this leg; no real session or scoped-query proof yet; do not substitute gateway `/responses` events |
| OpenCode | Push; traces, execution grain | `opencode` | Dedicated OIDC audience/fleet; verified credential claims bind identity | Source mapper defines units; missing cost remains NULL | No real session or scoped query verified |
| Native VS Code / GitHub Copilot | Push; native spans must have an explicit execution mapper | Unresolved: `github-copilot` currently dispatches logs to the vendor day/seat decoder | Shared AI-CLI edge audience today; native identity must come from verified claims | Native contract needs fixture verification; do not infer cost from gateway rows | Unsupported by the existing day/seat mapper; do not relabel native spans as vendor daily reports |

Productivity counters and a new productivity grain are out of scope. Metrics retain their Alloy
and archive paths and must not be inserted as request observations. The `ai-cli` fleet stamp is
not a canonical source and must never become `X-Source`.

## Blockers verified in the deployed receiver's source

1. **Fixed.** `handlers/ingest.rs::merge_attr_maps` merged record (payload) attributes over
   resource attributes at every signal level, so a forged `user_id`/`account_id` in a single log
   record, span or metric point silently overrode the verified identity `governance-auth` stamps
   at the resource level (from the claims of the same bearer token the collector's OIDC extension
   already validated). All seven call sites now merge the other way -- the trusted resource side
   is applied last and wins on conflict, with a record's own value used only when resource has
   nothing to say. `check_identity_mismatch`/`payload_identity.rs` is a *different*, already-sound
   mechanism (it only ever covers the collector-fleet `source`, never account/user), so it did not
   need to change. Proven with a spoofing test (sabotage-verified: fails with the attacker's value
   when the merge order is reverted) and a fallback test (an internal/legacy sender with no
   resource-level identity still gets its record-level value).
2. Unchanged from the prepared authz prerequisites below (dedup key, additive migration).
3. Unchanged from the prepared authz prerequisites below (replay `X-Source`).
4. **Fixed.** `spend_for_account` now restricts both the raw and rollup arms to
   `source IS NULL OR source = 'eaig'`. `NULL` is deliberately treated as EAIG, not excluded --
   every row written before `usage_events.source` existed, and every rollup row written before
   `usage_events_daily.source` existed (new migration `20260922000002`, additive + a replaced
   unique index), is `NULL` and is, in substance, 100% EAIG traffic; excluding `NULL` would have
   silently undercounted that legacy spend, which a naive `source = 'eaig'` filter would have done.
   The daily rollup itself (`ROLLUP_AND_PURGE_SQL`) now carries `source` through its
   `SELECT`/`GROUP BY`/`ON CONFLICT`, so two sources on the same account/day/model land in two
   separate rollup rows instead of merging. Proven with three new tests: raw-row exclusion,
   rollup-row exclusion, and NULL-as-EAIG backward compatibility, plus a rollup test asserting
   distinct rows per source and a migration-replay test.
5. **Wire probe passed:** contrib `0.160.0` `otlp_http` sent gzip-compressed
   `application/x-protobuf` to an explicit `/v1/otel/logs` endpoint and accepted a synthetic
   HTTP 202 `application/json` response containing `accepted_events`. The local upstream
   response was HTTP 200 with `partialSuccess: {}`. This proves exporter response compatibility,
   not real-client normalization, TLS or database persistence.
6. Native VS Code telemetry is not proven by `github-copilot` being a registry entry: its current
   log dispatch is day/seat-specific. Preserve that vendor-report contract when adding native spans.
   Still unresolved -- out of scope for this change, same as OpenCode's own real-session proof.

## Trusted source decision — resolved: source-specific paths, no second credential

Authz's own accepted ADR-0028 D8 ("Ingest authentication topology: the edge collector is the door.
Projected SA tokens are not.") already settles this in plain text: *"Source identity comes from
that credential or from the trusted collector processor, never from the payload... The
collector→usage hop carries no second credential... Adding a token on a hop that is already
topology-pinned buys an audit line and a rotation liability, not a boundary."* The governance#358
ticket's own instructions say the same thing directly: *"Follow ADR-0028's edge-authenticated
internal hop rather than adding an invented second credential."* Distinct source
audiences/credentials is therefore not an option to weigh -- it is the design D8 explicitly
retired.

**⚠️ Corrected mid-implementation, recorded here rather than silently fixed**: an earlier version
of this change built a third collector (`codexOtel`, a dedicated `aud: lightbridge-api-key`
audience) on the theory that Codex and VS Code Copilot are locked out of `aiCliOtel` entirely --
that theory came from stale comments in this chart and in `ai-helm-values` describing a `manual`,
direct-wiring mode that is no longer the default. **`governance-auth`'s local collector daemon
(ADR-0016) has been the default profile for a while**: Claude Code, Codex and VS Code Copilot all
export to `127.0.0.1:<fixed port>` with no credential, and the daemon itself mints ONE bearer
(`aud: governance-auth-cli`, minted through the same `oauth` path `governance-auth ... token`
uses) and forwards everything through it to `aiCliOtel` -- the collector this audience already
works against. Verified directly, not inferred: a real developer's own `~/.codex/config.toml`
already points its OTLP exporter at `127.0.0.1:17457`, not at `otel.ai.camer.digital`. The
`codexOtel` collector, its ingress, its CiliumNetworkPolicy and the `assert-*` script updates for
it have been reverted -- they solved a problem that mostly does not exist for the profile that
matters, and would have added chart surface for the `manual` profile alone (which ADR-0016 keeps
for locked-down/shared hosts, but is not the common case).

**What actually needed fixing, and is now implemented:** the daemon receives from every local
client on ONE port and previously stamped no per-tool `governance.source` at all -- everything
forwarded through it landed at `aiCliOtel` under that collector's own coarse, per-instance fleet
label (`ai-cli`). `app/governance-auth/src/otel_daemon/source_stamp.rs` (governance-auth, new
module) now derives a per-resource `governance.source` (`codex` / `claude-code`) from each
resource's own `event.name` before the daemon forwards -- `codex.*`-namespaced names for Codex,
Claude Code's documented bare event names (`api_request`, `user_prompt`, `tool_result`, `auth`,
`plugin_*`, ...) otherwise, `docs/rfc/sources/claude-codex-usage-investigation.md` is the source
for both taxonomies. **This is safe to derive from `event.name` here specifically, where it would
not be at a public collector**: ADR-0016's own threat model already accepts that any local process
can forge telemetry attributable to its own developer ("the residual risk is accepted"), so a
same-developer TOOL label from a signal the process already legitimately controls adds no new
exposure -- `codex_cost::enrich` already relies on the identical signal (`codex.sse_event`) for
cost estimation, at this same trust boundary and admission point. A client-supplied
`governance.source` is stripped before this module's own derived value is inserted, the same
"strip then set" shape `normalize::stamp` already uses for identity. Six unit tests cover the two
taxonomies, an unrecognised event leaving the payload untouched, a forged value being replaced not
layered, protobuf/JSON parity + re-enrichment idempotency, and a multi-resource batch stamping each
resource independently -- sabotage-verified (disabling the strip, or the Codex prefix check, both
made the expected tests fail for the predicted reason).

**Chart-side consequence, also implemented:** `publicOtelCollector`'s `resource` processor stamped
`governance.source` with `action: upsert`, which would have silently overwritten the daemon's
correct per-resource value with `aiCliOtel`'s own coarse `ai-cli` default on every single request.
Changed to `action: insert` (only fills the key when absent), so a daemon-stamped resource keeps
its real value, while a resource that reaches this collector without ever passing through the
daemon (a `manual`-profile client, or `opencodeOtel`'s traffic, which never goes through this
daemon at all) still gets a sane default. Re-validated against the real
`otel/opentelemetry-collector-contrib:0.160.0` binary's `validate` subcommand after this change.

**The fourth exporter leg (`otlphttp/usage`, parallel to `otlp/alloy`/`awss3`) is implemented, still
off by default, and now safe to enable on `aiCliOtel`.** It sends a STATIC `X-Source` header
(`$otel.usageExport.source`) on the whole outgoing HTTP request, which by itself is too coarse once
one forwarded batch can legitimately mix `claude-code` and `codex` resources (the collector's own
`batch` processor can combine multiple daemon-forwarded requests within its 5s window). **Fixed**:
`lightbridge-authz`'s `extract_log_events`/`extract_trace_events`/`extract_metric_events` now
resolve source PER RESOURCE via a new `resolve_event_source` helper -- a resource's own
`governance.source` attribute wins when present and inside the closed source registry, falling
back to the collector-level `X-Source` header only when a resource carries none of its own (or an
unrecognised one). This is a refinement of the already-authenticated channel's source, never an
escape from it: nothing here accepts a value `resolve_source` would have refused for the request as
a whole. Covered by a new test exercising all four cases in one multi-resource batch (Codex
override, Claude Code override, no override falls back to the header, an out-of-registry claim
falls back to the header) -- sabotage-verified. `opencodeOtel`'s traffic is unaffected (no
resource-level override ever present, so it always falls through to its own correct default).

**Still genuinely open, unrelated to the above correction:** VS Code Copilot's telemetry is
day/seat-grain (a different normalizer, a different table) and stays its own unresolved matrix row
below -- it is unaffected by the daemon/source-stamp work either way, since `source_stamp.rs`
leaves an unrecognised `event.name` untouched rather than guessing.

## Production acceptance procedure

After the reviewed chart, receiver, identity/source contract and network policy are deployed:

1. Record chart/config commit and receiver/collector image versions; assert ready replicas.
2. Run one small real Claude Code session. Capture only aggregate row deltas, source, timestamp
   interval, token totals and known/unknown cost counts. Record which signal was unsupported.
3. Query through the normal authenticated query surface for the owning user/account. Verify
   refusal for another account and for missing/invalid credentials; test claim dependency outage.
4. Inject conflicting identity/source attributes in isolated test traffic and verify they cannot
   change attribution. Never publish the conflicting identifiers or payload.
5. Exercise a temporary usage-service failure in an isolated environment, restore it, and resend
   the same request. Assert unchanged authoritative row count and spend after redelivery. Check
   Alloy/archive progress while the usage leg is unavailable and state queue exhaustion limits.
6. Repeat real-client and scoped-query proof for Codex/OpenCode; separately resolve and verify
   native VS Code/Copilot. No gateway event substitutes for native-client acceptance.
7. Attach this completed matrix and sanitized evidence to #358 and #581 before closing.

No production migration, deployment, replay or test session has been performed by this record.

## Prepared authz prerequisites (not deployed)

The isolated authz branch prepares an additive nullable request dedup key and a separate writer
change. Log requests with a natural request ID and stable event timestamp are deduplicated while
the raw row remains in storage. Legacy/no-key rows and replay after raw retention are explicitly
outside this guarantee. Schema and writer must ship in successive releases (authz ADR-0031).
Replay objects require an explicit canonical source; mixed fleet archive keys are not trusted
provenance. Receiver error bodies are no longer echoed by the replay transport. The trusted-identity
precedence fix (blocker #1) and the source-preserving spend/rollup fix (blocker #4) described above
are the same additive-migration discipline: both ship as nullable columns / non-destructive index
replacements, no backfill that invents data, and no writer deployed without its schema first.

Focused verification in that worktree: **191 passed, 0 failed, 0 ignored** (was 184; +7 from this
change's identity-precedence and spend/rollup tests):

| Test binary | Passed |
|---|---:|
| Usage library | 96 |
| Replay | 14 |
| Repository | 38 |
| Request-dedup migration over existing rows | 1 |
| Natural-key helpers | 2 |
| Real ingest-route retries | 3 |
| Retention (incl. per-source rollup separation) | 12 |
| Scope ownership | 15 |
| Spend queries (incl. EAIG-only exclusion + NULL fallback) | 9 |
| Rollup-source migration over existing rows | 1 |

The synthetic ingest-route retry retained **1 row / 15 tokens / 42 micro-USD** after two deliveries.
Removing source validation/header propagation produced **3 expected replay failures**; omitting
the inserted dedup key produced **2 rows where 1 was expected**; reverting the identity-precedence
merge order made the new spoofing test fail with the attacker's forged value, exactly as predicted.
Fixed code was restored and the full focused set above rerun. Strict Clippy for the usage crate's
all targets/features and `cargo deny check` passed. These are component/prerequisite results, not
real-client acceptance.

⚠️ **`cargo +nightly fmt --all` reformats ~250 files repo-wide** (pre-existing nightly-fmt drift
unrelated to this change, verified file-by-file: none of the files this change or the prior
dedup/replay change touches appear in a `cargo +nightly fmt -p lightbridge-authz-usage-rest --
--check` run). Only ran it scoped to files this change actually touched; the repo-wide drift is a
separate, unrelated cleanup for someone else to pick up deliberately, not folded into this diff.
