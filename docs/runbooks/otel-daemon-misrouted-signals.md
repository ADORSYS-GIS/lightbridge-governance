# Incident: Codex metrics routed as logs, and checks for Claude Code and Copilot

Incident date: 2026-09-10. This documents an observed Codex incident; it does
not claim that Claude Code or Copilot reproduced it. Their sections distinguish
our generated configuration from behavior that still needs a per-client check.

## Two separate failures

The first failure was local spool recovery: the checkpoint contained a different
device identifier from the live file. Recovery reset the offset but retained the
stale identity, looping at 100% CPU before forwarding anything. Commit
[`4a6b77a`](https://github.com/ADORSYS-GIS/lightbridge-governance/commit/4a6b77a)
fixed that loop and shipped in v2.5.1. Installing the binary did not replace the
running process; after a service restart, CPU fell to 0% and the checkpoint moved.

That exposed the second failure: repeated HTTP 400 responses while the checkpoint
advanced slowly and discard counts increased. A running process and a moving
checkpoint were not sufficient proof of successful delivery of every signal.

## Evidence and cause

On 2026-09-10, an explicitly approved replay of one queued Codex payload
returned HTTP 400 from `/v1/logs` (`wrong wireType = 2 for field TimeUnixNano`)
and HTTP 200 from `/v1/metrics`, with identical bytes. The production collector
had logs, metrics and traces pipelines enabled; both replicas had zero restarts.

Codex's binary exporters used the configured root URL verbatim. The daemon
recognized `/v1/metrics` but defaulted every other binary request to logs.
A metric's field 1 is its string name; a log record's field 1 is a fixed64
timestamp. Decoding metrics as logs therefore produces that wire-type error.
The bytes are not necessarily corrupt, even when several consecutive exports fail.

## Correct behavior

The daemon supports OTLP/HTTP logs, metrics and traces in JSON and protobuf:

| Signal | Endpoint | JSON envelope |
|---|---|---|
| Logs | `/v1/logs` | `resourceLogs` |
| Metrics | `/v1/metrics` | `resourceMetrics` |
| Traces | `/v1/traces` | `resourceSpans` |

`configure` writes explicit logs and metrics URLs for Codex. Other clients
using standard OTLP base-URL settings still append their signal paths themselves.
This does not enable an additional exporter in clients that were not exporting it.

For existing root-URL binary exporters, the daemon uses the official OTLP
message schemas and requires exactly one recognizable signal. Empty or
ambiguous binary exports need an explicit signal path. Unknown JSON signals,
mixed JSON envelopes and detected path/body conflicts return HTTP 400 before
admission. Unsupported signals are not relabeled as logs. This receiver is
OTLP/HTTP; it does not add a gRPC listener or a profiles pipeline.

## Endpoint rules: base URL versus signal URL

The distinction is the configuration field, not the vendor name. The
[OTLP exporter specification](https://opentelemetry.io/docs/specs/otel/protocol/exporter/#endpoint-urls-for-otlphttp)
defines a shared base endpoint to which signal paths are added, while
signal-specific endpoints are used verbatim. A root URL in a signal-specific
field therefore sends to `/`. Do not append `/v1/logs` to a shared base setting:
that can also redirect metrics and traces incorrectly or double the suffix.

### Codex: fix each configured HTTP exporter

The incident workstation had `protocol = "binary"` and the same root URL in
both exporter tables. The updated [writer](../../app/governance-auth/src/otel.rs)
produces this telemetry fragment:

```toml
[otel]
log_user_prompt = false

[otel.exporter.otlp-http]
endpoint = "http://127.0.0.1:17457/v1/logs"
protocol = "binary"

[otel.metrics_exporter.otlp-http]
endpoint = "http://127.0.0.1:17457/v1/metrics"
protocol = "binary"
```

Merge through `governance-auth configure --profile daemon`; do not replace a
user's whole config with this fragment. Restart Codex to load exporter changes.
The [official Codex example](https://learn.chatgpt.com/docs/config-file/config-advanced#observability-and-telemetry)
also uses a complete `/v1/logs` URL. The metrics behavior here was verified from
this workstation's configuration and the approved identical-byte replay.
This patch does not automatically enable an additional Codex trace exporter.
If one is configured in a client version that supports it, verify its endpoint
contract and route its OTLP/HTTP traffic to `/v1/traces`.

### Claude Code: preserve the shared endpoint unless evidence says otherwise

Our [Claude Code writer](../../app/governance-auth/src/otel.rs) already uses the
shared OTLP endpoint and HTTP/protobuf. Its relevant `settings.json` fragment is:

```json
{
  "env": {
    "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
    "OTEL_METRICS_EXPORTER": "otlp",
    "OTEL_LOGS_EXPORTER": "otlp",
    "OTEL_EXPORTER_OTLP_PROTOCOL": "http/protobuf",
    "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:17457"
  }
}
```

Do not change that shared endpoint to `/v1/logs` as a blanket Codex workaround.
Check effective settings, environment and managed-policy overrides, then observe
the request path for each enabled signal. When signal-specific HTTP endpoints
are needed, use complete URLs ending in `/v1/logs`, `/v1/metrics`, or `/v1/traces`.
Managed settings may override developer settings; make the change at the owning
configuration layer. See [Claude Code monitoring](https://code.claude.com/docs/en/monitoring-usage).

Our generator enables logs and metrics, not enhanced trace/content capture.
If the installed Claude Code version has an enabled trace exporter, verify it
separately. Daemon trace support does not prove a client emits traces, and
troubleshooting should not turn on prompt or tool-content capture.

### Copilot: check the VS Code exporter and effective overrides

For **VS Code Copilot Chat**, our [daemon-profile writer](../../app/governance-auth/src/vscode/entries.rs)
produces:

```json
{
  "github.copilot.chat.otel.enabled": true,
  "github.copilot.chat.otel.exporterType": "otlp-http",
  "github.copilot.chat.otel.otlpEndpoint": "http://127.0.0.1:17457",
  "github.copilot.chat.otel.captureContent": false
}
```

Keep this collector base URL unless a check of the installed exporter proves
otherwise. VS Code documents logs/events, metrics and traces, a shared endpoint,
and environment/managed settings that can supersede user settings. Inspect
those overrides before changing the generated file. Restart the relevant client
process and verify each emitted signal. See [VS Code monitoring](https://code.visualstudio.com/docs/agents/guides/monitoring-agents).

Do not assume these VS Code keys configure standalone Copilot CLI, JetBrains,
or every embedded agent runtime; check each runtime's own export settings.
The `manual` profile's Copilot file drain is a separate path and remains a
logs/metrics drain. Choose the daemon profile for direct OTLP signal forwarding.

## Repeatable diagnosis for any client

1. Record the client version, daemon version **in the running process**, profile,
   effective exporter settings and protocol. Keep credentials out of reports.
2. Generate a small ordinary activity in one client. Inspect only HTTP method,
   destination path, content type, resource service name and record counts.
   Verify logs, metrics and traces individually where the client emits them.
3. Compare schema with destination. Decode privately using the official OTLP
   schemas; an envelope labeled `Logs` by an old daemon is not reliable evidence
   of the payload's signal. Do not dump bodies, prompts or tool arguments.
4. Check collector readiness, authentication and the corresponding signal
   pipeline. Preserve the same bytes for a controlled replay if necessary.
   Obtain approval before exporting captured telemetry for diagnosis; report
   status codes and bounded decoder errors, never tokens or response bodies.
5. Change the client-specific URL only when its actual endpoint semantics require
   it. Add a regression fixture with synthetic data for that signal, protocol and
   path, and prove it fails before the fix. Never infer fleet-wide health from
   a single successful log export.
6. Confirm collector acceptance and downstream visibility, not only local HTTP
   200. Local success means durable admission; an upstream 200 does not itself
   prove persistence or dashboard visibility in a later stage.

Use an isolated test environment for tests binding port 17457. Merely stopping
the production daemon does not stop IDE exporters: they can send real telemetry
to a test receiver that acknowledges it without delivering it to production.
Prefer ephemeral-port checks where possible. A mixed-format assertion failed
once in the fixed-port suite and passed on a diagnostic run; live-traffic
contamination is a possible explanation, not a confirmed root cause. The added
isolated sender test checks all 50 requests across repeated format transitions.

## Upgrade and recovery

Restart the service after installing the patched binary; replacing the executable
does not replace an already-running process. Re-run `configure` to update client
URLs, and restart clients when their exporter configuration is read only at startup.

The drain recognizes old `Logs` envelopes containing identifiable metrics or
traces while reading them. It preserves their payload bytes, original retry key,
file identity and byte offsets. Refusal evidence is scoped to the corrected
destination, so old logs rejections cannot justify discarding recovered metrics.
No spool rewrite or checkpoint reset is needed
for pending records. JSON traces receive the same identity and retry-key stamping
as JSON logs and metrics. Protobuf bytes retain the existing pass-through behavior.

Records already counted as discarded are behind the checkpoint. This fix does
not rewind it: doing so would replay successful deliveries too. Preserve a private
snapshot before spool reclamation if those records need separate recovery, and
verify the true signal and downstream duplicate behavior before replaying them.

Check for advancing offsets, decreasing pending counts, and collector acceptance.
An increasing discard count with HTTP 400 is not sufficient evidence of malformed
client output; first rule out an incorrect destination.

## Verification recorded for this change

- The root-URL binary metrics regression failed as `Logs` before the fix and
  passed as `Metrics` afterward. The refusal-history regression likewise failed
  before destination scoping and passed afterward.
- Final unit run: 398 passed, two fixed-port tests excluded to avoid the live
  daemon. Those port tests passed during an earlier paused-service run.
- Existing daemon integration cases passed; the new signal integration case
  passed on its diagnostic run, with the transient failure noted above.
- Nightly formatting, Clippy, type checks and the changed-file size check passed.
- The broader suite stopped at two browser-launch failures. `cargo-deny` was not
  installed locally. Neither is represented as a passing check.
- No live Claude Code or Copilot replay was performed in this incident. The
  production replay proved the sampled Codex metrics payload only.

Upstream documentation was consulted on 2026-09-10. Recheck version-specific
client behavior when an exporter changes; the daemon's contract remains explicit
signal routing, byte-preserving protobuf forwarding and refusal of ambiguity.

## Follow-up: login succeeded but the macOS daemon stopped

On 2026-09-10, v2.5.2 forwarded logs and metrics successfully, then stopped
forwarding after its access token expired. A separate credential-helper call
confirmed HTTP 400 `invalid_grant` during refresh. That response does not establish
why the refresh grant became invalid. The daemon retained pending records.

A fresh login succeeded, but service installation unloaded the existing launchd
job and its immediate `bootstrap` failed with error 5. Inspection subsequently
found no registered daemon. Registering the same plist later succeeded and the
backlog drained without increasing the historical discard count of 122. The exact
launchd rejection cause has not been established; the timing is consistent with a
reload race, not proof of one.

Shutdown also exposed a separate programming error: the spool task join handler
called `JoinError::into_panic()` for a cancelled task. Cancellation is now returned
as an operation error, while genuine panic payloads still propagate. Shutdown waits
for the aborted drain task before returning. Repeat installation with an unchanged
plist uses `kickstart -k` instead of unregistering the service; a missing service or
changed plist still takes the registration path. This avoids the unnecessary
unload/register sequence on repeated logins, rather than claiming to explain every
possible launchd bootstrap failure.

Regression tests reproduce the cancellation panic and the previous repeat-login
command sequence before the fixes. Scheduler tests use injected commands and never
unload the workstation's daemon. These source changes require a subsequent binary
release; restoring the installed v2.5.2 service does not install them.
