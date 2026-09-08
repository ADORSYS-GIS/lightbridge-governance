#!/usr/bin/env bash
# Asserts the real client address, not the Traefik pod IP, ends up on
# telemetry that gets PAST auth (lightbridge-governance#284 AC1's reachable
# half -- see the ⚠️ comment above `include_metadata: true` in _helpers.tpl
# for why the OTHER half, the refusal log's `client_ip`, is explicitly out of
# scope: verified against the pinned `oidcauthextension`/confighttp source
# that no version of either reads X-Forwarded-For for that field).
#
# Two collectors carry this (`aiCliOtel`, `opencodeOtel` -- both behind
# Traefik); the `copilotOtel` collector does not (its CiliumNetworkPolicy
# admits only the CronJobs + Alloy, never Traefik) and is untouched by this
# change, on purpose.
#
# Structural checks alone would not be enough here, unlike
# assert-oidc-auth.sh: THAT script only pins config keys because the
# enforcement logic lives inside oidcauthextension, Go this repo does not
# own. The XFF-parsing logic below (Split/Trim/Len-index to the rightmost
# entry) is OTTL *this chart authors*, so a copy-paste or off-by-one bug in
# it would still pass a string-match test. This script instead renders the
# chart, strips only the OIDC gate (already covered by assert-oidc-auth.sh)
# so the pipeline can run unauthenticated locally, and runs the exact
# rendered `processors`/`receivers` config under the SAME pinned image
# (otel/opentelemetry-collector-contrib:0.158.0) production runs, via
# `docker run` -- then pushes real HTTP requests at it and reads back what
# actually landed.
#
# Sabotage-checked: reverting the `transform/client_address_from_xff`
# processor (or the `resource` processor's `client.address.xff_raw` action)
# makes the spoofed-header assertion below fail by finding `client.address`
# absent rather than equal to the trustworthy entry.
set -euo pipefail

CHART="${1:-charts/lightbridge-governance}"
YQ="${YQ_BIN:-yq}"
IMAGE="otel/opentelemetry-collector-contrib:0.158.0"
FRAGMENT="ai-cli-otel" # opencodeOtel shares the same body verbatim -- see below.
PORT=$(( (RANDOM % 20000) + 20000 ))

WORKDIR="${TMPDIR:-/tmp}/assert-client-address-xff.$$"
# `mkdir -p` here, deliberately NOT `mktemp -d`: on at least one real Docker
# setup this was tested against, a `mktemp -d` workdir bind-mounted as /out
# produced a silently empty file in the container -- HTTP 200 accepted, zero
# lines in `docker logs`, nothing ever written, no matter how long this
# waited -- while an otherwise byte-identical `mkdir -p` directory worked on
# the first push every time. Whatever that Docker setup's bind-mount
# resolution does differently for a `mktemp`-created path, this sidesteps it
# entirely rather than chasing it further.
rm -rf "${WORKDIR}"
mkdir -p "${WORKDIR}"
trap 'docker rm -f otel-xff-assert >/dev/null 2>&1 || true; rm -rf "${WORKDIR}"' EXIT

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

  im="$(printf '%s\n' "${cfg}" | "${YQ}" '.receivers.otlp.protocols.http.include_metadata')"
  [ "${im}" = "true" ] || fail "${fragment}: receivers.otlp.protocols.http.include_metadata != true (got: ${im})"

  staged="$(printf '%s\n' "${cfg}" | "${YQ}" '.processors.resource.attributes[] | select(.key == "client.address.xff_raw") | .from_context')"
  [ "${staged}" = "metadata.x-forwarded-for" ] || fail "${fragment}: resource processor has no client.address.xff_raw action reading metadata.x-forwarded-for"

  for stmts in log_statements trace_statements metric_statements; do
    ctx="$(printf '%s\n' "${cfg}" | "${YQ}" ".processors.[\"transform/client_address_from_xff\"].${stmts}[0].context")"
    [ "${ctx}" = "resource" ] || fail "${fragment}: transform/client_address_from_xff.${stmts}[0].context != resource (got: ${ctx})"
  done

  for pipeline in traces metrics logs; do
    procs="$(printf '%s\n' "${cfg}" | "${YQ}" ".service.pipelines.${pipeline}.processors | join(\",\")")"
    case "${procs}" in
      *"resource,transform/client_address_from_xff,batch"*) ;;
      *) fail "${fragment}/${pipeline}: transform/client_address_from_xff is not positioned between resource and batch (got: ${procs})" ;;
    esac
  done

  echo "Structural: ${fragment} stages+resolves client.address from X-Forwarded-For in all three pipelines."
done

# --- Runtime assertion, against the real pinned image -------------------
# Only one collector needs the live probe: both public collectors render the
# identical body (proven by the structural loop above finding both correct),
# so a runtime bug in the shared OTTL would reproduce on either.
cfg="$(get_config "${FRAGMENT}")"

# Strip the OIDC gate so this probe can run unauthenticated -- that gate is
# assert-oidc-auth.sh's job, not this script's. Redirect to a `file` exporter
# so the result is inspectable without a real Alloy endpoint.
printf '%s\n' "${cfg}" \
  | "${YQ}" 'del(.extensions) | del(.receivers.otlp.protocols.http.auth) | del(.service.extensions)' \
  | "${YQ}" '.exporters = {"file": {"path": "/out/telemetry.json"}}' \
  | "${YQ}" '(.service.pipelines.[].exporters) = ["file"]' \
  > "${WORKDIR}/collector-config.yaml"

mkdir -p "${WORKDIR}/out"
chmod 777 "${WORKDIR}/out"

docker rm -f otel-xff-assert >/dev/null 2>&1 || true
docker run -d --name otel-xff-assert \
  -p "${PORT}:4318" \
  -v "${WORKDIR}/collector-config.yaml:/etc/otelcol-contrib/config.yaml:ro" \
  -v "${WORKDIR}/out:/out" \
  "${IMAGE}" >/dev/null

for _ in $(seq 1 30); do
  docker logs otel-xff-assert 2>&1 | grep -q "Everything is ready" && break
  sleep 1
done
docker logs otel-xff-assert 2>&1 | grep -q "Everything is ready" \
  || fail "otel collector never became ready -- see docker logs otel-xff-assert"

trace_body() {
  local trace_id="$1"
  cat <<EOF
{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"assert-client-address-xff"}}]},"scopeSpans":[{"scope":{},"spans":[{"traceId":"${trace_id}","spanId":"EEE19B7EC3C1B174","name":"probe","startTimeUnixNano":"1700000000000000000","endTimeUnixNano":"1700000000100000000"}]}]}]}
EOF
}

# `file` exporter opens telemetry.json ONCE and keeps that fd for the
# collector's life (confirmed live: deleting the path mid-run doesn't make it
# reappear -- the exporter keeps "succeeding" into the now-unlinked inode,
# invisible to any later `ls`/`cat`). So this NEVER deletes the file between
# assertions. Each push instead gets its own trace ID, and every read below
# greps/filters for that specific ID -- the file just keeps growing as JSONL
# (one export per line, confirmed live), which this treats as the log it is.
wait_for_trace() {
  local trace_id="$1"
  for _ in $(seq 1 15); do
    grep -qi "${trace_id}" "${WORKDIR}/out/telemetry.json" 2>/dev/null && return 0
    sleep 1
  done
  return 1
}

resource_attrs_for_trace() {
  local trace_id="$1"
  jq -c --arg tid "${trace_id}" '
    select(.resourceSpans[0].scopeSpans[0].spans[0].traceId == $tid)
    | .resourceSpans[0].resource.attributes
  ' "${WORKDIR}/out/telemetry.json" | tail -n1
}

# 1. Spoofed leading entry: the caller's own claimed address must NOT survive.
#    Only what Traefik itself appended (the rightmost entry) may.
TRACE_SPOOFED="5b8efff798038103d269b633813fc60c"
curl -sf -X POST "http://localhost:${PORT}/v1/traces" -H "Content-Type: application/json" \
  -H "X-Forwarded-For: 6.6.6.6, 203.0.113.42" \
  -d "$(trace_body "${TRACE_SPOOFED}")" -o /dev/null \
  || fail "push with spoofed X-Forwarded-For was rejected -- probe config is broken, not just the feature under test"
wait_for_trace "${TRACE_SPOOFED}" || fail "no telemetry flushed after spoofed-XFF push (batch never fired -- pipeline is broken)"

attrs="$(resource_attrs_for_trace "${TRACE_SPOOFED}")"
got="$(jq -r '.[] | select(.key=="client.address") | .value.stringValue // empty' <<<"${attrs}")"
[ "${got}" = "203.0.113.42" ] || fail "spoofed-XFF push: client.address = \"${got}\", want \"203.0.113.42\" (the spoofed leading entry must be discarded, not trusted)"

leaked="$(jq -r '.[] | select(.key=="client.address.xff_raw") | .key // empty' <<<"${attrs}")"
[ -z "${leaked}" ] || fail "scratch key client.address.xff_raw leaked into exported telemetry"

echo "Runtime: spoofed X-Forwarded-For (6.6.6.6, 203.0.113.42) resolved to client.address=203.0.113.42, not the spoofed entry."

# 2. No X-Forwarded-For header at all: must not crash, must not fabricate a value.
TRACE_NOHEADER="5b8efff798038103d269b633813fc60d"
curl -sf -X POST "http://localhost:${PORT}/v1/traces" -H "Content-Type: application/json" \
  -d "$(trace_body "${TRACE_NOHEADER}")" -o /dev/null \
  || fail "push with no X-Forwarded-For header was rejected"
wait_for_trace "${TRACE_NOHEADER}" || fail "no telemetry flushed after header-less push"

attrs="$(resource_attrs_for_trace "${TRACE_NOHEADER}")"
got="$(jq -r '.[] | select(.key=="client.address") | .value.stringValue // empty' <<<"${attrs}")"
[ -z "${got}" ] || fail "header-less push: client.address = \"${got}\", want absent (no header means no attribute, not a fabricated one)"

echo "Runtime: no X-Forwarded-For header -> no client.address attribute (no crash, nothing fabricated)."
