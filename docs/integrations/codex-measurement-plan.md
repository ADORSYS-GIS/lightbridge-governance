# Codex: measuring the missing user/session fields

Status: initial implementation, 2026-09-12. The bounded metadata exporter and
native-response estimate annotations are implemented. The operational contract,
supported version/model and limitations are in [codex-dashboard.md](codex-dashboard.md).
The broader measurement targets below remain the design for subsequent coverage.

## Evidence

The existing dashboard reads forwarded OTLP logs. An allowlisted inspection of
the local development conversation found additional metadata in two Codex Desktop
session files produced by engine `0.154.0-alpha.6.2`:

- `session_meta.git.repository_url`, launch commit and branch; `turn_context.cwd`
  supplies the working directory at individual turns.
- Terminal `task_complete` and `turn_aborted` records have `turn_id`, `started_at`,
  `completed_at`, and `duration_ms`. At inspection there were 19 distinct terminal
  turns, no conflicting terminal records, and 2,191,320 ms of summed turn duration.
  The union of the second-resolution start/end intervals was 2,193 seconds.
  These differ because the source timestamps and durations have different precision.
  The current unfinished turn is excluded; this is a sample, not a session total.
- Completed `FileChange` records exist, but only one was observed in this sample,
  covering one added file. Numerous other edits used command execution. Structured
  edits alone therefore cannot represent all AI changes.
- Request usage records distinguish input, cached input, output, reasoning output,
  cache writes, and total tokens. They also carry response, thread and turn IDs,
  alongside cumulative turn/thread counters. Never sum all three counter grains.

The inspection emitted metadata and counts, not prompts, responses, commands,
source code, tool results, or copies of transcripts. No session files belong in
fixtures. Synthetic fixtures must exercise the same shapes.

The [Codex hooks documentation](https://learn.chatgpt.com/docs/hooks) describes
turn-scoped hooks and their session/turn context. It explicitly says transcript
format is unstable. Hooks require trust and tool coverage is incomplete. A Stop
hook is not necessarily the final end of a turn.
The [app-server reference](https://learn.chatgpt.com/docs/app-server) describes
turn lifecycle notifications, completed file changes and aggregated turn diffs.
A separate app-server process must not be assumed to observe the running desktop
session. Attachment and delivery coverage need a live proof before selecting it.

## Measurement and display contract

| Field | Measurement | Display and limitations |
|---|---|---|
| Repository | Resolve the repository from the event/turn working directory locally. Normalize its remote to host/owner/repository, stripping credentials, query and fragment. Use an explicit project mapping when there is no safe canonical remote. | Repository column in sessions; one repository table for sessions, tokens and attributable cost. Unknown stays visible. A session may touch several repositories; never allocate its entire cost to every repository it touched. |
| Agent turn time | Record terminal turn duration with its status. Preserve intervals to calculate unions within each user/session and clip to the selected time window. | Time column per session and a selected-session turn table. Label as elapsed agent turn time: approval, tool and other waits can be included. It is not human active time or CPU time. |
| Proposed lines | Count additions/deletions from an identifiable structured edit proposal. An updated aggregated diff replaces its preceding snapshot. | Separate numbers for proposed additions and deletions, with structured-edit coverage. Shell edits with no proposal are unavailable at this stage. |
| Applied lines | Confirm a successful edit and its observed before/after delta. Count only changes attributable to the agent operation; exclude ambiguous concurrent edits. | Separate applied additions/deletions. Applied is not evidence of human acceptance. Repeated rewrites are edit volume, not unique retained contribution. |
| Accepted lines | Explicit human acceptance tied to the same edit identity, if the client exposes it. | Separate number only for covered clients/events. Permission to execute a tool, automated approval and absence of an undo are not acceptance. |
| Retained lines | Follow attributable added lines to a defined checkpoint, initially the first commit containing them. Later survival at a fixed interval is a separate cohort. | Retained additions at commit, with coverage and observation age. Do not subtract unrelated deletions from additions or interpret pending cohorts as zero. |
| Actual cost | An authoritative billing source at its available attribution grain. Reconcile reporting period, identity and billing mode. | Currency total only at supported grain. Workspace/day billing cannot silently become per-session cost. Subscription allocation is a separate accounting policy. |
| Estimated cost | Deduplicated per-response token categories multiplied by a versioned, effective-dated rate card for the exact model/tier/context policy. | Explicitly labelled API-equivalent estimate, separate from actual charges. Unknown rate/model/category leaves cost unknown. Use integer micro-USD and checked arithmetic; retain rate provenance. |

Cached input is a subset of input and reasoning output must not be added to
inclusive output again. Cache-write semantics require source-specific verification.
An API-equivalent estimate for ChatGPT-authenticated Codex is not its actual bill.
The [Codex Analytics API overview](https://learn.chatgpt.com/docs/enterprise/analytics-api)
describes workspace reporting and organization-scoped access. The exact accessible
schema and reporting grain must be verified before promising per-user credits or
dollars. This investigation did not obtain admin reporting access or verify a
billing endpoint.

## Collection boundary

Implement the adapter in Rust within `governance-auth`; use the existing daemon's
durable, authenticated forwarding. Do not introduce a second production toolchain,
ship transcripts, or add telemetry tables to the governance registry database.
The usage system of record belongs to `lightbridge-authz` (ADR-0014).

Local inspection must minimize content access. Diff parsing, where necessary,
emits counts only; never spool diffs, prompts, commands, completions, or content
hashes. A retention tracker requiring persisted source-content fingerprints is
not compatible with ADR-0013's no-content rule as written. Prefer source-native
provenance references or transient comparisons against existing Git history;
prove that these can preserve attribution through edits before shipping retention.

For repository and turn timing, the candidate declaration is: push through the
daemon; execution/turn grain; principal bound to the forwarding credential;
pattern A via daemon; no emitted cost (unknown, not zero). Add a dedicated row to
RFC-0003 before implementing this source, as required by ADR-0013. Edit lifecycle
and billing sources need their own declared grain and authoritative measures.

Use source session/turn/edit IDs as opaque identifiers. Combine them with the
tenant, credential principal, source and event kind for deterministic natural keys.
Never mint replacements for external IDs. A lifecycle update upserts the same
fact; resuming, compacting or replaying a session must not increase totals.
Conflicting records must be surfaced, not silently replaced by whichever arrived
last. Transport spool retry keys do not independently deduplicate source events.

## Implementation sequence and acceptance evidence

1. **Repository and turn-time adapter.** Select a supported lifecycle feed after
   exercising it in the desktop and CLI. If a transcript adapter is necessary,
   explicitly version its compatibility and report unsupported or incomplete
   records. Test restart, resume/compaction, duplicate events, aborts, unfinished
   turns, overlapping subagents, changed working directories, multiple repos,
   malformed records and credential-bearing remotes. Export a coverage measure.
2. **Read path and dashboard.** Verify idempotent ingest before aggregation; use
   interval unions for elapsed time. Add repository/time columns and the one
   repository breakdown only after live emitted facts agree with local metadata.
   Preserve missing values and selected-range semantics. Recheck Grafana layout
   and image export along with generator/chart checks.
3. **Cost adapter.** Choose actual reporting, estimates, or both; verify the
   accessible schema/rates. Test rounding boundaries, overflow, cached-token
   inclusion, unknown pricing, duplicate responses and billing/estimate separation.
4. **Contribution pilot.** Exercise structured edits, shell-written files,
   rejected/failed changes, reversals, renames, concurrent human edits and commits.
   Measure coverage before showing counts. Add proposed/applied counts first if
   defensible; acceptance and retention remain unavailable until independently
   observed. Do not label the same counter three different ways.

New regression tests must fail under a deliberate mutation and then pass after
restoration. The initial duration display is explicitly a sum of terminal turns,
not the interval-union target above. Actual billing, code contribution and broader
client/model coverage are not claimed as implemented.
