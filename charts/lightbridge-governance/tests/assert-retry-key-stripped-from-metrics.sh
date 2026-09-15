#!/usr/bin/env bash
# Asserts `governance.retry_key` -- the per-record idempotency key
# `governance-auth`'s daemon stamps on JSON-encoded telemetry (otel_daemon/
# normalize/mod.rs) -- never reaches a Prometheus/Mimir series, while
# continuing to reach the logs pipeline unchanged.
#
# Why this exists: found 2026-09-15 investigating VS Code Copilot's
# dashboard. Protobuf-encoded clients (Claude Code, Codex) never see this key
# at all (the daemon's stamp() only touches JSON bodies), but VS Code Copilot
# Chat sends JSON, so every one of its records gets a distinct key -- and
# because a metrics exporter's `target_info` series is built from resource
# attributes unconditionally, every record produced a brand-new `target_info`
# series, forever: confirmed live, 5,094 distinct `governance_retry_key`
# values across 5,105 target_info entries for job="copilot-chat" in a single
# 7-day window (13 real sessions, 4 real users -- the key is not per-session,
# it is per-record). See _helpers.tpl's own doc on
# `transform/strip_retry_key_from_metrics` for the full account, including
# why this is scoped to metrics only: the key may still be exactly what a
# future usage-ingest dedup path needs on the signal that actually carries
# billing-relevant events (logs) -- checked live, lightbridge-authz's
# ingest.rs has zero references to it today, but "unused today" is not the
# same claim as "safe to delete everywhere".
#
# Structural checks alone would not catch a typo in the OTTL statement or a
# processor wired into the wrong pipeline -- same reasoning
# assert-client-address-xff.sh gives for its own runtime probe. This follows
# that exact harness: render the chart, strip the OIDC gate, redirect every
# pipeline to a `file` exporter, run the real pinned image via `docker run`,
# push synthetic OTLP, and read back what actually left the pipeline.
#
# Sabotage-checked: commenting out the `delete_key` statement (or removing
# `transform/strip_retry_key_from_metrics` from the metrics pipeline's
# processor list) makes the metrics assertion below fail by finding the key
# present rather than absent. Removing the processor definition from the
# LOGS pipeline's list -- i.e. accidentally scoping the strip to logs too --
# makes the logs assertion fail by finding the key absent rather than
# present, catching the opposite mistake just as directly.
set -euo pipefail

CHART="${1:-charts/lightbridge-governance}"
YQ="${YQ_BIN:-yq}"
IMAGE="otel/opentelemetry-collector-contrib:0.160.0"
FRAGMENT="ai-cli-otel" # opencodeOtel shares the same body verbatim.
PORT=$(( (RANDOM % 20000) + 20000 ))

WORKDIR="${TMPDIR:-/tmp}/assert-retry-key.$$"
# `mkdir -p`, not `mktemp -d` -- see assert-client-address-xff.sh's own
# comment on this exact same line for why (a real Docker setup observed
# during that script's own development silently produced an empty bind-mount
# under a `mktemp -d` path).
rm -rf "${WORKDIR}"
mkdir -p "${WORKDIR}"
trap 'docker rm -f otel-retry-key-assert >/dev/null 2>&1 || true; rm -rf "${WORKDIR}"' EXIT

rendered="$(
  helm template ci "${CHART}" \
    --set aiCliOtel.enabled=true \
    --set opencodeOtel.enabled=true
)"
printf '%s\n' "${rendered}" > "${WORKDIR}/rendered.yaml"

fail() {
  echo "::error::${1}" >&2
  exit 1
}

get_config() {
  local fragment="$1"
  "${YQ}" eval-all "select(.kind == \"OpenTelemetryCollector\") | select(.metadata.name | contains(\"${fragment}\")) | .spec.config" "${WORKDIR}/rendered.yaml"
}

# --- Structural assertions, on BOTH public collectors ------------------
for fragment in ai-cli-otel opencode-otel; do
  cfg="$(get_config "${fragment}")"
  [ -n "${cfg// /}" ] || fail "No OpenTelemetryCollector matching \"${fragment}\" rendered."

  del="$(printf '%s\n' "${cfg}" | "${YQ}" '.processors.["transform/strip_retry_key_from_metrics"].metric_statements[0].statements[0]')"
  case "${del}" in
    *'delete_key(attributes, "governance.retry_key")'*) ;;
    *) fail "${fragment}: transform/strip_retry_key_from_metrics.metric_statements[0] does not delete governance.retry_key (got: ${del})" ;;
  esac

  ctx="$(printf '%s\n' "${cfg}" | "${YQ}" '.processors.["transform/strip_retry_key_from_metrics"].metric_statements[0].context')"
  [ "${ctx}" = "resource" ] || fail "${fragment}: transform/strip_retry_key_from_metrics.metric_statements[0].context != resource (got: ${ctx})"

  # A transform processor with ONLY a metric_statements key is structurally a
  # no-op for logs/traces -- not "also present but empty", genuinely absent.
  for other in log_statements trace_statements; do
    present="$(printf '%s\n' "${cfg}" | "${YQ}" ".processors.[\"transform/strip_retry_key_from_metrics\"] | has(\"${other}\")")"
    [ "${present}" = "false" ] || fail "${fragment}: transform/strip_retry_key_from_metrics has a ${other} block -- it must be metrics-only"
  done

  metrics_procs="$(printf '%s\n' "${cfg}" | "${YQ}" '.service.pipelines.metrics.processors | join(",")')"
  case "${metrics_procs}" in
    *"transform/strip_retry_key_from_metrics"*) ;;
    *) fail "${fragment}: metrics pipeline does not include transform/strip_retry_key_from_metrics (got: ${metrics_procs})" ;;
  esac

  for pipeline in logs traces; do
    procs="$(printf '%s\n' "${cfg}" | "${YQ}" ".service.pipelines.${pipeline}.processors | join(\",\")")"
    case "${procs}" in
      *"transform/strip_retry_key_from_metrics"*) fail "${fragment}/${pipeline}: transform/strip_retry_key_from_metrics must not appear outside the metrics pipeline (got: ${procs})" ;;
      *) ;;
    esac
  done

  echo "Structural: ${fragment} strips governance.retry_key in metrics only, never logs/traces."
done

# --- Runtime assertion, against the real pinned image -------------------
# One collector needs the live probe: both render the identical body (proven
# by the structural loop above), so a runtime bug in the shared OTTL would
# reproduce on either.
cfg="$(get_config "${FRAGMENT}")"

printf '%s\n' "${cfg}" \
  | "${YQ}" 'del(.extensions) | del(.receivers.otlp.protocols.http.auth) | del(.service.extensions)' \
  | "${YQ}" '.exporters = {"file": {"path": "/out/telemetry.json"}}' \
  | "${YQ}" '(.service.pipelines.[].exporters) = ["file"]' \
  > "${WORKDIR}/collector-config.yaml"

mkdir -p "${WORKDIR}/out" "${WORKDIR}/refusals"
chmod 777 "${WORKDIR}/out" "${WORKDIR}/refusals"

docker rm -f otel-retry-key-assert >/dev/null 2>&1 || true
docker run -d --name otel-retry-key-assert \
  -p "${PORT}:4318" \
  -v "${WORKDIR}/collector-config.yaml:/etc/otelcol-contrib/config.yaml:ro" \
  -v "${WORKDIR}/out:/out" \
  -v "${WORKDIR}/refusals:/var/log/collector" \
  "${IMAGE}" >/dev/null

for _ in $(seq 1 30); do
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 \
    -X POST "http://localhost:${PORT}/v1/logs" \
    -H "Content-Type: application/json" -d '{"resourceLogs":[]}' 2>/dev/null || true)"
  [ -n "${code}" ] && [ "${code}" != "000" ] && break
  sleep 1
done
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 \
  -X POST "http://localhost:${PORT}/v1/logs" \
  -H "Content-Type: application/json" -d '{"resourceLogs":[]}' 2>/dev/null || true)"
[ -n "${code}" ] && [ "${code}" != "000" ] \
  || fail "otel collector never became ready (endpoint did not respond) -- see docker logs otel-retry-key-assert"

# `file` exporter opens telemetry.json once and keeps the fd for the
# collector's life -- see assert-client-address-xff.sh's own comment on this
# same behavior. Never delete the file between pushes; each gets its own ID
# and every read filters for that specific one.
wait_for_marker() {
  local marker="$1"
  for _ in $(seq 1 15); do
    grep -qi "${marker}" "${WORKDIR}/out/telemetry.json" 2>/dev/null && return 0
    sleep 1
  done
  return 1
}

# 1. Metrics: push a Sum metric carrying governance.retry_key on the
#    resource, exactly as the daemon's stamp() would produce for a
#    JSON-encoded client. Must be ABSENT in what left the pipeline.
METRIC_MARKER="assert-retry-key-metric-probe"
RETRY_KEY_METRICS="rk-metrics-c3b6c503319b48381"
metrics_body() {
  cat <<EOF
{"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"${METRIC_MARKER}"}},{"key":"governance.retry_key","value":{"stringValue":"${RETRY_KEY_METRICS}"}}]},"scopeMetrics":[{"scope":{},"metrics":[{"name":"probe.count","sum":{"dataPoints":[{"timeUnixNano":"1700000000000000000","asInt":"1"}],"aggregationTemporality":2,"isMonotonic":true}}]}]}]}
EOF
}
curl -sf -X POST "http://localhost:${PORT}/v1/metrics" -H "Content-Type: application/json" \
  -d "$(metrics_body)" -o /dev/null \
  || fail "metrics push was rejected -- probe config is broken, not just the feature under test"
wait_for_marker "${METRIC_MARKER}" || fail "no telemetry flushed after metrics push (batch never fired -- pipeline is broken)"

metric_attrs="$(jq -c --arg m "${METRIC_MARKER}" '
  select(.resourceMetrics[0].resource.attributes[]? | .value.stringValue? == $m)
  | .resourceMetrics[0].resource.attributes
' "${WORKDIR}/out/telemetry.json" | tail -n1)"
leaked="$(jq -r '.[] | select(.key=="governance.retry_key") | .value.stringValue // empty' <<<"${metric_attrs}")"
[ -z "${leaked}" ] || fail "metrics: governance.retry_key = \"${leaked}\", want absent -- the metrics-only strip did not run"

echo "Runtime: governance.retry_key stripped from a real metrics export."

# 2. Logs: push the SAME attribute via /v1/logs. Must SURVIVE -- proving the
#    strip is scoped to metrics only, not a blanket removal that would also
#    (silently) take away whatever a future ingest-dedup path might need.
LOG_MARKER="assert-retry-key-log-probe"
RETRY_KEY_LOGS="rk-logs-c3b6c503319b48382"
logs_body() {
  cat <<EOF
{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"${LOG_MARKER}"}},{"key":"governance.retry_key","value":{"stringValue":"${RETRY_KEY_LOGS}"}}]},"scopeLogs":[{"scope":{},"logRecords":[{"timeUnixNano":"1700000000000000000","body":{"stringValue":"probe"}}]}]}]}
EOF
}
curl -sf -X POST "http://localhost:${PORT}/v1/logs" -H "Content-Type: application/json" \
  -d "$(logs_body)" -o /dev/null \
  || fail "logs push was rejected -- probe config is broken, not just the feature under test"
wait_for_marker "${LOG_MARKER}" || fail "no telemetry flushed after logs push (batch never fired -- pipeline is broken)"

log_attrs="$(jq -c --arg m "${LOG_MARKER}" '
  select(.resourceLogs[0].resource.attributes[]? | .value.stringValue? == $m)
  | .resourceLogs[0].resource.attributes
' "${WORKDIR}/out/telemetry.json" | tail -n1)"
survived="$(jq -r '.[] | select(.key=="governance.retry_key") | .value.stringValue // empty' <<<"${log_attrs}")"
[ "${survived}" = "${RETRY_KEY_LOGS}" ] || fail "logs: governance.retry_key = \"${survived}\", want \"${RETRY_KEY_LOGS}\" -- the metrics-only strip leaked into logs"

echo "Runtime: governance.retry_key preserved on a real logs export (metrics-only scoping confirmed)."
