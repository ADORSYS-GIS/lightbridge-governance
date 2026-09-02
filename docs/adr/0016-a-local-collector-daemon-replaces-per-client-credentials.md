# ADR-0016: A local collector daemon replaces per-client OTLP credentials

- Status: Accepted
- Date: 2026-09-02
- Decision owners: @stephane-segning

## Context

`governance-auth` points three AI clients at a governed collector, and each one solves the
credential problem differently — because each one *forced* a different answer:

| Client | How it authenticates today | Refreshes? |
|---|---|---|
| Claude Code | `otelHeadersHelper` re-runs `governance-auth otel-headers` on a debounce | ✅ yes |
| VS Code Copilot | writes to a **file**; `copilot-push` drains it with a bearer minted per wake | ✅ yes, indirectly |
| Codex | a **static** `Authorization` string in `~/.codex/config.toml`, read once at start | ❌ **no** |

Codex is not an oversight, it is a dead end. Measured against codex-cli 0.149.0 and upstream `main`
on 2026-09-02:

- **There is no file exporter.** `otel.exporter` is a four-variant enum and the binary rejects
  anything else: `unknown variant 'file', expected one of 'none', 'statsig', 'otlp-http',
  'otlp-grpc'`. So the Copilot pattern cannot be transferred.
- **There is no credential indirection.** No `headers_command`, no `bearer_token_env_var`, no
  `${VAR}` interpolation under `[otel]` — all three exist for MCP servers and none for telemetry.
  `headers` is a plain `HashMap<String, String>`.
- `OTEL_EXPORTER_OTLP_HEADERS` *is* honoured and overrides the config value, but it is read once at
  process start, so it moves the secret off disk without making it refreshable.

Meanwhile the Copilot path pays for its own workaround: a spool that grew to **12 MB in a few
hours** before [#230](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/230) bounded it, a
parser for the OpenTelemetry **JS SDK's private object graph** rather than OTLP, and a drain that
degrades whenever that private shape moves.

Three clients, three mechanisms, one of them structurally unfixable. That is the situation forcing a
decision.

## Decision

**Adopt a local collector daemon as the default telemetry path, and keep today's direct wiring as an
explicitly selected alternative.**

`governance-auth serve --otel` runs an OTLP/HTTP receiver bound to **loopback only**, on a fixed
port. Every client exports to it over plain HTTP with no credential. The daemon spools to disk,
attaches a freshly minted bearer, and forwards to the governed collector.

**Two profiles, selected at `configure` time and recorded in the config file:**

- **`daemon`** (default) — clients point at `http://127.0.0.1:<port>`; the daemon forwards.
- **`manual`** — today's behaviour: direct exporters, a static `--otel-token` where a client needs
  one, and the `copilot-push` timer. Selected with `configure --profile manual`.

`manual` is not a deprecation shim. It is the correct profile for a machine where a long-running
user service is unwanted or impossible — a locked-down build agent, a container, a shared host —
and it is the profile that keeps working if the daemon is stopped.

**The spool does not disappear; it moves.** One spool inside the daemon replaces per-client
workarounds, and it keeps the property that makes the current design survivable: bytes reaching disk
before they reach the network, so a sleeping laptop, an unreachable collector or an expired token
costs latency rather than data.

**The port is fixed, not ephemeral**, for the reason ADR-0015 already fixes the callback ports: a
value drawn from the ephemeral range can be handed to an unrelated process at any time, and a
client's config is written once and read at process start — it cannot renegotiate. It sits in the
same reserved block as the callback ports so one range covers everything this binary listens on.

## Consequences

**Positive**

- **Codex becomes self-renewing**, which nothing else in its configuration surface can achieve. The
  last long-lived OTLP credential on disk is removed.
- **Copilot stops needing the file exporter** and the JS-SDK object-graph parser. Its telemetry
  arrives as OTLP, from Copilot's own OTLP exporter, over loopback.
- **One credential path instead of three**, each of which currently has its own failure mode and its
  own row in the support matrix.
- **One spool instead of per-source workarounds**, with one checkpoint, one rotation policy and one
  place to reason about conservation.
- Signal-path handling moves into code we own — which is where the Codex endpoint-path defect
  (below) stops mattering, because the daemon accepts whatever path a client posts to.

**Negative**

- **A daemon is a bigger claim on a developer's machine than a timer.** Mitigated by the fact that
  we already install and manage a launchd agent / systemd user unit
  ([`crate::schedule`](../../app/governance-auth/src/schedule/)); this converts a five-minute
  oneshot into a long-running service rather than introducing a new deployment concept.
- **A loopback port is reachable by every process running as any user on the machine**, where a
  `0600` spool file is not. See the threat model below — this is a real widening, accepted
  deliberately.
- **A stopped daemon is a silent hole.** A client that fires-and-forgets at a dead port loses its
  telemetry with no error anyone reads. `status` must report the daemon's health as loudly as it
  reports the drain's today, and that requirement is part of the epic rather than a nicety.
- More moving parts on the client machine, and a new failure mode (daemon up, collector down) that
  the spool must absorb.

**Neutral / follow-ups**

- The `copilot-push` machinery is **not** deleted. `manual` still uses it, and roughly three
  quarters of it — the offset tailer, file-identity detection, quarantine, the OTLP push with its
  401/404/413/429 taxonomy, the checkpoint and journal — is source-agnostic and becomes the
  daemon's spool layer.
- The support matrix and `default-flow.md` gain a profile axis; every per-client credential row
  becomes profile-dependent.

## Threat model for the loopback listener

Stated explicitly because "it's only localhost" is where this kind of decision usually stops.

**What an unprivileged local process can do:** POST arbitrary OTLP to the daemon, which will forward
it to the governed collector stamped with *this developer's* identity attributes. That is telemetry
forgery, not credential theft — the bearer is never handed to the client, only used by the daemon on
its own outbound request.

**What it cannot do:** read the bearer, read other telemetry, or reach anything but this daemon.
Binding is `127.0.0.1` explicitly, never `0.0.0.0`.

**Why no local authentication:** every mechanism available would be worse. A shared secret has to be
written into each client's config — and for Copilot that config is covered by **Settings Sync**,
which is the exact reason the current design refuses to put a bearer there. A secret readable by the
client is readable by anything running as the developer, so it authenticates nothing a filesystem
permission does not already imply.

**The residual risk is accepted**: forged telemetry from a machine the developer already controls,
attributable to that developer. If that ever becomes material, the answer is signing at the daemon,
not a shared secret at the client.

## Alternatives considered

- **Leave Codex on a static token.** Rejected: it is the last long-lived OTLP credential on disk,
  and today's investigation established that no configuration change can make it refresh. Accepting
  it means accepting it permanently.
- **A launcher shim that `exec`s Codex with `OTEL_EXPORTER_OTLP_HEADERS` set.** Measured to work,
  and cheap. Rejected as the *answer* because it is still read once at process start: a 300-second
  token dies mid-session, and a developer who starts Codex any other way gets nothing. Worth keeping
  as a `manual`-profile improvement, not as the architecture.
- **Run the upstream OpenTelemetry Collector locally.** Rejected: it solves receiving, not
  authenticating — something still has to mint and rotate the bearer — and it adds a second
  deployment artifact, a second config language and a second upgrade path to every laptop, for
  machinery we would still have to wrap.
- **Extend the file-spool pattern to every client.** Rejected on evidence: Codex has no file
  exporter, so it is impossible there, and Copilot's file exporter writes a private JS-SDK shape
  that has already cost a bespoke parser.
- **Ephemeral port with discovery through a state file.** Rejected for ADR-0015's reason: clients
  read their configuration once at start and cannot renegotiate, so a port that moves is a port that
  breaks after a restart.
- **`rusqlite` for daemon state now.** Deferred, not rejected — see below.

## Open question: structured local state

The daemon's state is plausibly richer than the current single JSON checkpoint: per-source offsets,
a quarantine table with a TTL, delivery counters, maybe a spool index. `rusqlite` was raised as the
structured alternative to JSON files.

**Deferred deliberately.** It is a real dependency decision, not a detail: `bundled` compiles SQLite
from source, which every release target — including two musl builds — must keep working, and it
introduces schema migrations to a binary whose whole on-disk story is currently "files you can read
with `cat`" (ADR-0012 §1, and its refusal of an OS keychain for the same reason).

Revisit when at least one is true: more than two telemetry sources share one daemon; state needs a
query rather than a full read; or concurrent writers need transactional semantics that the existing
`FileLock` cannot express. Until then the daemon uses the file-based checkpoint that already exists
and is already tested.

## Related

- ADR-0009 — no second persistence path (the server-side rule this deliberately does not extend to
  the CLI's own local state)
- ADR-0012 §1 — on-disk layout, and §4's installer conventions
- ADR-0015 — the loopback callback port block this reuses the reasoning of
- RFC-0003 §2a — the telemetry source taxonomy these profiles cut across
- Issues [#230](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/230),
  [#241](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/241) — spool growth, the cost
  of the current per-client workaround
- `docs/governance-auth/default-flow.md` — the runbook this makes profile-dependent
