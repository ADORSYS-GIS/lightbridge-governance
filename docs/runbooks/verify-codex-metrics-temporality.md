# Verify (or refute) Codex's OTLP metrics temporality, live

**Status: handoff runbook, not yet executed against a real interactive
session.** Everything in this document is either (a) already confirmed --
marked as such, with the evidence -- or (b) a step for whoever runs this to
execute and record the result of. Do not treat an unexecuted step's expected
outcome as having happened.

## Why this exists

Claude Code's `claude_code.lines_of_code.count`/`.active_time.total`/etc.
metrics never appeared in Mimir until
[lightbridge-governance#335](https://github.com/ADORSYS-GIS/lightbridge-governance/pull/335)
set `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE=cumulative`. Root
cause: Claude Code's documented default is `delta`, and this org's collector
pipeline (`ai-cli-otel` collector, Alloy) has no `deltatocumulative`
processor anywhere -- Prometheus/Mimir's data model has no delta concept, so
a well-formed delta export simply never becomes a queryable series. See
`docs/integrations/claude-code-dashboard.md`'s "Lines of code, active time,
commits" section for the full account, including how it was confirmed
(a temporary, reverted daemon instrumentation that decoded the actual OTLP
protobuf bytes).

**Codex shows the identical symptom, checked live on 2026-09-15:**

- Mimir, both Codex job labels, full 7-day window: `job="codex-app-server"`
  (23 distinct resources) and `job="codex_cli_rs"` (3 resources) carry
  **only** `target_info` -- zero named `codex_*`/`codex.*` metric series,
  ever, in that window.
- Codex's own local telemetry log on this machine
  (`~/.codex/logs_2.sqlite`, table `logs`) has a real historical entry from
  2026-09-11 (`app.version=0.135.0`, an interactive `codex-tui` session):
  `PeriodicReaderMetricsCollected count=12` immediately followed by
  `HttpMetricsClient.ExportStarted` then `HttpMetricsClient.ExportSucceeded`
  -- Codex's own Rust OTel SDK (`telemetry.sdk.language=rust`,
  `telemetry.sdk.version=0.31.0`, confirmed via the matching Loki log lines
  for that session) believed it successfully POSTed 12 metric points to the
  daemon. That timestamp falls inside the 7-day Mimir window above.
- **What is NOT confirmed:** the actual wire-level `aggregation_temporality`
  value (0=UNSPECIFIED, 1=DELTA, 2=CUMULATIVE per the OTel proto spec) of
  those 12 points. The symptom is identical to Claude Code's pre-fix state,
  and delta-vs-cumulative is the only mechanism this investigation found
  anywhere that produces exactly this signature (every layer reports
  success, zero resulting series) -- but that is an inference from a
  matching fingerprint, not a byte-level confirmation. This runbook gets
  that confirmation.
- **A methodology trap already found, do not repeat it:** `codex exec`
  (the one-shot non-interactive CLI mode) writes **nothing** to
  `logs_2.sqlite`, ever -- confirmed by running real, multi-minute `codex
  exec` sessions (including with `--enable runtime_metrics` explicitly set)
  and finding zero new rows in the log table afterward. Only the
  interactive TUI / app-server path exercises the metrics pipeline. Use a
  real interactive session for this verification, not `codex exec`.
- Also confirmed, not load-bearing for the above but worth knowing: `codex
  features list` shows `runtime_metrics` as `under development`, `false`
  (disabled) by default in this build (`codex-cli 0.154.0`). Enabling it
  (`--enable runtime_metrics`, or persist with `codex features enable
  runtime_metrics`) did not change the `codex exec` result -- expected,
  since `codex exec` doesn't reach the metrics pipeline at all regardless of
  this flag. Whether the *baseline* metric catalog (the instruments visible
  in the 2026-09-11 log -- `codex.startup_phase`, `codex.thread.started`,
  `codex.shell_snapshot`, `codex.remote_models.*`) requires this flag at all
  is itself unconfirmed; that session's flag state at the time is unknown.
  Enable it for this verification anyway, to maximize the metric surface
  captured and remove it as a variable.

## What this runbook settles

1. The actual `aggregation_temporality` Codex's Rust SDK puts on the wire
   for its Sum/Histogram metrics, captured directly (not inferred).
2. Whether the standard, spec-defined
   `OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE` environment variable
   -- not a Claude-Code-specific setting; it is part of the
   [OpenTelemetry SDK environment variable
   specification](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/)
   -- is honored by Codex's Rust SDK at all. Codex's own docs never mention
   it, but that does not mean the underlying `opentelemetry` crate ignores
   it; many SDKs implement the spec's standard surface even when the CLI's
   own docs don't call it out. If it works, the fix is exactly as small as
   Claude Code's was. If Codex ignores it, the fix has to live elsewhere
   (a `deltatocumulative` processor in the collector/Alloy pipeline,
   discussed at the end).

## Prerequisites

- `kubectl` access to the cluster (`observability` namespace: Mimir, Alloy).
- This repo cloned, on a machine that also runs the `governance-auth-serve-otel`
  systemd user service (the loopback OTLP daemon every AI CLI client already
  points at -- confirm with `systemctl --user status
  governance-auth-serve-otel.service`).
- A real Codex CLI install pointed at that daemon (`~/.codex/config.toml`'s
  `[otel.metrics_exporter.otlp-http]` should already read
  `endpoint = "http://127.0.0.1:17457/v1/metrics"` -- `governance-auth
  configure` writes this; if it's missing, run `governance-auth configure`
  first).
- Willingness to have one real interactive Codex session run for a few
  minutes -- this uses real API calls/tokens, same as any normal Codex
  usage.

## Step 1 -- isolate the work

Everything below is a **temporary, reverted** change to a debug build of the
daemon. Do it in a separate git worktree so it never touches your actual
working branch:

```sh
cd /path/to/lightbridge-governance
git worktree add /tmp/codex-metrics-diag main
cd /tmp/codex-metrics-diag
```

## Step 2 -- add the temporary diagnostic

Open `app/governance-auth/src/otel_daemon/mod.rs`. Find `handle_request`,
specifically the line right after signal classification:

```rust
    let Some(signal) = classify::signal(&incoming.body, incoming.format, &incoming.path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let body = if signal == signal::Signal::Logs {
```

Insert this block between those two lines (before `let body = ...`):

```rust
    // TEMPORARY DIAGNOSTIC -- not for commit. Decodes only metric names,
    // data-point counts, and the wire-level aggregation_temporality/
    // is_monotonic fields -- never values or log/trace bodies.
    if signal == signal::Signal::Metrics && incoming.format == receive::WireFormat::Protobuf {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::metric::Data;
        use prost::Message;
        match ExportMetricsServiceRequest::decode(incoming.body.as_slice()) {
            Ok(req) => {
                for rm in &req.resource_metrics {
                    let service_name = rm
                        .resource
                        .as_ref()
                        .and_then(|r| r.attributes.iter().find(|kv| kv.key == "service.name"))
                        .and_then(|kv| kv.value.as_ref())
                        .and_then(|v| v.value.clone())
                        .map(|v| format!("{v:?}"))
                        .unwrap_or_default();
                    for sm in &rm.scope_metrics {
                        for m in &sm.metrics {
                            let (data_points, temporality, is_monotonic) = match &m.data {
                                Some(Data::Sum(d)) => (d.data_points.len(), Some(d.aggregation_temporality), Some(d.is_monotonic)),
                                Some(Data::Gauge(d)) => (d.data_points.len(), None, None),
                                Some(Data::Histogram(d)) => (d.data_points.len(), Some(d.aggregation_temporality), None),
                                _ => (0, None, None),
                            };
                            tracing::info!(service_name, name = %m.name, data_points, ?temporality, is_monotonic, "DIAG metric");
                        }
                    }
                }
            }
            Err(error) => tracing::info!(%error, "DIAG failed to decode ExportMetricsServiceRequest"),
        }
    }
```

`temporality` prints as `Some(1)` for DELTA, `Some(2)` for CUMULATIVE,
`Some(0)` for UNSPECIFIED, `None` for a Gauge (gauges have no temporality
concept). This is the exact instrumentation used to confirm Claude Code's
root cause; nothing here is Codex-specific except that it's generic across
any client hitting the daemon's `/v1/metrics`.

## Step 3 -- build and swap the binary

```sh
cargo build -p governance-auth --bin governance-auth

# Stop the daemon FIRST -- the binary is memory-mapped while running and
# `cp` over a running executable fails with "Text file busy".
systemctl --user stop governance-auth-serve-otel.service

# Back this up. You will restore it in Step 6 -- do not skip this.
cp ~/.local/bin/governance-auth ~/.local/bin/governance-auth.prod-backup

cp target/debug/governance-auth ~/.local/bin/governance-auth
systemctl --user start governance-auth-serve-otel.service
systemctl --user status governance-auth-serve-otel.service --no-pager | head -6
```

Confirm it's the debug build actually running (`Main PID` should be a fresh
process, started just now).

## Step 4 -- tail the log while running a REAL interactive session

```sh
tail -f ~/.local/state/governance-auth/logs/governance-auth.log | grep --line-buffered "DIAG"
```

In a **separate terminal**, run Codex interactively (not `codex exec` --
see the methodology trap above) for at least 2-3 minutes of real activity
-- ordinary usage is fine, the point is just enough wall-clock time for the
periodic metrics reader to tick at least once (Codex's own log showed a
~40-60s interval). Two runs are worth doing:

1. **Baseline**, your normal environment, `runtime_metrics` enabled for
   maximum coverage:
   ```sh
   codex --enable runtime_metrics
   ```
   (or `codex features enable runtime_metrics` once, if you'd rather not
   pass the flag every time -- remember to `codex features disable
   runtime_metrics` afterward if you do, since it's `under development`).

2. **With the standard OTel env var set**, to test whether Codex's Rust SDK
   honors it despite Codex's own docs never mentioning it:
   ```sh
   OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE=cumulative codex --enable runtime_metrics
   ```

Do some real work in each session (ask it to read a file, make an edit,
run a command -- anything that exercises `tool.call`/`api_request`-shaped
activity) so there's something for the metric instruments to actually
record, then let the session sit for the remainder of the 2-3 minutes
before exiting.

## Step 5 -- read the result

For each of the two runs, record every `DIAG metric` line: `name`,
`data_points`, `temporality`, `is_monotonic`. Then:

- **If `temporality` is ever `Some(1)` (DELTA) in the baseline run**: root
  cause confirmed, matching Claude Code exactly.
  - **If the env-var run shows `Some(2)` (CUMULATIVE) instead**: the fix is
    exactly as small as Claude Code's -- get this environment variable into
    Codex's process environment. Check whether `~/.codex/config.toml`
    supports an env-var-injection mechanism for its own child process
    (`shell_environment_policy` governs the *shell tool's* environment, not
    necessarily Codex's own -- verify which one, if either, actually
    affects Codex's own OTel SDK init before assuming either one works).
    If neither does, this may need to go into `governance-auth
    configure_codex`'s own process-launch wrapper, or Codex may need to be
    launched via a shell function/wrapper script that exports it -- check
    against this repo's own rule in `app/governance-auth/src/otel/client_scope_tests.rs`
    (`shell_exports_carry_no_otel_key`) before reaching for the shared
    shell rc files: a client-specific OTLP setting was deliberately kept out
    of the machine-global shell after a real incident (one client's OTLP
    endpoint leaking into another's), and a temporality preference is the
    same category of per-client setting.
  - **If the env-var run ALSO shows `Some(1)` (DELTA)**: Codex's Rust SDK
    does not honor the standard env var (or something else overrides it).
    The fix has to live downstream instead: add a `deltatocumulative`
    processor to the `ai-cli-otel` collector's or Alloy's metrics pipeline
    (see `docs/integrations/claude-code-dashboard.md`'s note on this being
    the more centralized, higher-blast-radius alternative -- it protects
    every future client, not just Codex, but touches shared cluster config
    in `ai-helm-values`, not just this repo).
- **If `temporality` is consistently `Some(2)` (CUMULATIVE) already**: the
  delta/cumulative hypothesis is wrong for Codex. In that case, re-open the
  investigation from the symptom itself (target_info-only, zero named
  series) -- check the collector/Alloy self-metrics the same way the Claude
  Code investigation did (`otelcol_receiver_accepted_metric_points_total`
  vs `otelcol_exporter_sent_metric_points_total` on the `ai-cli-otel`
  collector; `otelcol_receiver_accepted_metric_points_total` and
  `prometheus_forwarded_samples_total{component_id="otelcol.exporter.prometheus.default"}`
  on each Alloy pod) to find where else in the pipeline it could be
  disappearing.
- **If zero `DIAG metric` lines appear at all**, even after 2-3 minutes of
  real activity: something more basic is blocking Codex's metrics export
  from ever reaching the daemon (a connectivity issue, an auth failure
  specific to the metrics endpoint, or the interactive/app-server path
  behaving differently on this machine than the 2026-09-11 session did).
  Check `journalctl --user -u governance-auth-serve-otel.service` for any
  `WARN`/`ERROR` in the same window, and confirm
  `~/.codex/config.toml`'s `[otel.metrics_exporter.otlp-http]` block is
  actually present and points at the daemon.

## Step 6 -- revert, every time, even if you didn't finish

```sh
systemctl --user stop governance-auth-serve-otel.service
cp ~/.local/bin/governance-auth.prod-backup ~/.local/bin/governance-auth
systemctl --user start governance-auth-serve-otel.service
rm ~/.local/bin/governance-auth.prod-backup

cd /path/to/lightbridge-governance
git worktree remove /tmp/codex-metrics-diag --force
```

Confirm the restored daemon is the real one: `governance-auth --version`
should print the same version it did before Step 3, and
`journalctl --user -u governance-auth-serve-otel.service -n 5` should show
a normal restart with no diagnostic output.

## After this runbook

Update this file (or fold its result into
`docs/integrations/claude-code-dashboard.md`-style documentation for
Codex, if one gets written) with what was actually observed -- the
"What is NOT confirmed" section above should either be resolved to a
confirmed root cause and fix, or replaced with whatever the real
mechanism turned out to be. Don't leave this document's speculative parts
sitting next to confirmed ones without saying which is which, the same
discipline the Claude Code investigation held throughout.
