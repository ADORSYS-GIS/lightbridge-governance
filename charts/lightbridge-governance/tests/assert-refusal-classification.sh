#!/usr/bin/env bash
# Asserts `transform/classify_refusal` (lightbridge-governance#275) actually
# classifies REAL oidc-extension refusals correctly, against the real pinned
# `oidcauthextension` -- not just structurally, and not with OIDC stripped
# like assert-client-address-xff.sh does.
#
# Why this exists: every one of the 7 `refusal.reason` branches matches the
# upstream `oidcauthextension`'s exact warn-log text, verified only once,
# manually, against this pinned image (see the "Phase 1a spike" comment in
# _helpers.tpl). Nothing before this script re-ran that check, so a future
# `otelcol-contrib` bump that rewords any of those strings would silently
# degrade classification to "unknown" with zero build or CI signal -- a
# review finding (#321) left open when the rest of that review's findings
# were fixed.
#
# This boots the real, UNSTRIPPED oidc extension (unlike the xff script) and
# a throwaway local OIDC issuer (charts/lightbridge-governance/tests/support/
# mock_oidc_issuer.py) so cases requiring actual signature/expiry/audience
# verification -- not just malformed input -- are reachable without a real
# IDP. The pinned image reaches the issuer via `host.docker.internal`
# (`--add-host=host.docker.internal:host-gateway`, Docker 20.10+).
#
# Sabotage-checked against the actual risk this protects: mutating the
# `expired` statement's matched substring from "token is expired" to "token
# has expired" (simulating an upstream reword this chart hasn't caught up to)
# makes the `expired` assertion below fail -- the real go-oidc error text
# still says "is", so the refusal silently falls through to the generic
# `verification_failed` catch-all, exactly the "unknown"/wrong-bucket
# degradation this script exists to catch.
#
# NOT sabotage-checked (tried, and worth recording why): reordering
# `verification_failed` above `expired`/`wrong_audience` does NOT reproduce a
# failure here, despite review discussion suggesting it would. The transform
# processor runs every statement in order and each `set()` fires whenever ITS
# OWN `where` is true, independent of the record's current attribute value --
# `expired`/`wrong_audience` have no nil-guard of their own, so they
# unconditionally overwrite whatever an earlier `verification_failed` set,
# regardless of position. The `not IsMatch(...)` exclusions this PR added to
# `verification_failed` are still a real, worthwhile hardening (self-
# documenting, and correct if a future edit ever gives `expired`/
# `wrong_audience` their own nil-guard, which WOULD reintroduce real order
# dependence) -- just not a fix for a reachable bug in the code as it
# stands today. Recorded here rather than left as an unverified claim.
set -euo pipefail

CHART="${1:-charts/lightbridge-governance}"
YQ="${YQ_BIN:-yq}"
IMAGE="otel/opentelemetry-collector-contrib:0.160.0"
FRAGMENT="ai-cli-otel" # opencodeOtel shares the identical body -- see assert-oidc-auth.sh.
OTEL_PORT=$(( (RANDOM % 10000) + 20000 ))
ISSUER_PORT=$(( OTEL_PORT + 1 ))
AUDIENCE="test-audience"
ISSUER_URL="http://host.docker.internal:${ISSUER_PORT}"

WORKDIR="${TMPDIR:-/tmp}/assert-refusal-classification.$$"
# `mkdir -p`, not `mktemp -d` -- see assert-client-address-xff.sh's comment on
# this exact line for why (a real Docker bind-mount quirk with mktemp paths).
rm -rf "${WORKDIR}"
mkdir -p "${WORKDIR}/out" "${WORKDIR}/refusals"
chmod 777 "${WORKDIR}/out" "${WORKDIR}/refusals"

ISSUER_PID=""
trap 'docker rm -f otel-refusal-assert >/dev/null 2>&1 || true
      [ -n "${ISSUER_PID}" ] && kill "${ISSUER_PID}" >/dev/null 2>&1 || true
      rm -rf "${WORKDIR}"' EXIT

fail() {
  echo "::error::${1}" >&2
  exit 1
}

# --- Start the throwaway OIDC issuer, pointed at the port the collector
#     reaches it on via host.docker.internal (NOT localhost -- from inside
#     the container that's the container itself, not the host). -------------
python3 "$(dirname "$0")/support/mock_oidc_issuer.py" serve \
  "${ISSUER_PORT}" "${ISSUER_URL}" "${WORKDIR}/issuer-key.pem" &
ISSUER_PID=$!
for _ in $(seq 1 30); do
  curl -sf "http://127.0.0.1:${ISSUER_PORT}/.well-known/openid-configuration" >/dev/null 2>&1 && break
  sleep 0.2
done
curl -sf "http://127.0.0.1:${ISSUER_PORT}/.well-known/openid-configuration" >/dev/null 2>&1 \
  || fail "mock OIDC issuer never became ready"

sign() {
  python3 "$(dirname "$0")/support/mock_oidc_issuer.py" sign "${WORKDIR}/issuer-key.pem" \
    --iss "${ISSUER_URL}" --aud "${AUDIENCE}" "$@"
}

# --- Render with the REAL oidc extension pointed at the mock issuer, and
#     redirect every pipeline's exporter to `file` so refusal.reason is
#     inspectable. Unlike assert-client-address-xff.sh, `.extensions` /
#     `receivers.otlp.protocols.http.auth` are NOT deleted -- keeping the
#     gate active is the entire point of this script. ----------------------
rendered="$(
  helm template ci "${CHART}" \
    --set aiCliOtel.enabled=true \
    --set "aiCliOtel.oidc.issuerUrl=${ISSUER_URL}" \
    --set "aiCliOtel.oidc.audience=${AUDIENCE}"
)"
printf '%s\n' "${rendered}" > "${WORKDIR}/rendered.yaml"

cfg="$("${YQ}" eval-all "select(.kind == \"OpenTelemetryCollector\") | select(.metadata.name | contains(\"${FRAGMENT}\")) | .spec.config" "${WORKDIR}/rendered.yaml")"
[ -n "${cfg// /}" ] || fail "No OpenTelemetryCollector matching \"${FRAGMENT}\" rendered."

printf '%s\n' "${cfg}" \
  | "${YQ}" '.exporters = {"file/refusals": {"path": "/out/refusals.json"}}' \
  | "${YQ}" '(.service.pipelines.[].exporters) = ["file/refusals"]' \
  > "${WORKDIR}/collector-config.yaml"

docker rm -f otel-refusal-assert >/dev/null 2>&1 || true
docker run -d --name otel-refusal-assert \
  --add-host=host.docker.internal:host-gateway \
  -p "${OTEL_PORT}:4318" \
  -v "${WORKDIR}/collector-config.yaml:/etc/otelcol-contrib/config.yaml:ro" \
  -v "${WORKDIR}/out:/out" \
  -v "${WORKDIR}/refusals:/var/log/collector" \
  "${IMAGE}" >/dev/null

# Same rationale as assert-client-address-xff.sh: poll the endpoint, don't
# grep an INFO-level readiness log line -- `service.telemetry.logs.level:
# warn` (lightbridge-governance#275) suppresses it.
ready() {
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 \
    -X POST "http://localhost:${OTEL_PORT}/v1/traces" \
    -H "Content-Type: application/json" -d '{"resourceSpans":[]}' 2>/dev/null || true)"
  [ -n "${code}" ] && [ "${code}" != "000" ]
}
for _ in $(seq 1 30); do
  ready && break
  sleep 1
done
ready || fail "otel collector never became ready -- see docker logs otel-refusal-assert"

# refusal.reason (and refusal.issuer) land as log-record attributes on the
# exported LogRecord. Each classified refusal is its own JSON line in the
# file exporter's output; grep for the reason we expect, waiting the same
# way assert-client-address-xff.sh does for a trace.
wait_for_reason() {
  local want="$1"
  for _ in $(seq 1 15); do
    grep -q "\"refusal.reason\",\"value\":{\"stringValue\":\"${want}\"}" "${WORKDIR}/out/refusals.json" 2>/dev/null && return 0
    sleep 1
  done
  return 1
}

assert_refusal() {
  local name="$1" want="$2"; shift 2
  curl -s -o /dev/null -X POST "http://localhost:${OTEL_PORT}/v1/traces" \
    -H "Content-Type: application/json" -d '{"resourceSpans":[]}' "$@"
  wait_for_reason "${want}" \
    || fail "${name}: no refusal classified as \"${want}\" (see ${WORKDIR}/out/refusals.json)"
  echo "Runtime: ${name} -> refusal.reason=${want}."
}

# 1. No Authorization header at all.
assert_refusal "missing_credential" "missing_credential"

# 2. Header present but not the `Bearer <token>` two-part shape the
#    extension's `strings.Split(header, " ")` requires exactly.
assert_refusal "malformed_token (bad header shape)" "malformed_token" \
  -H "Authorization: NotBearerAtAll"

# 3. `Bearer <garbage>`: not JWT-shaped enough to extract an unverified `iss`.
assert_refusal "malformed_token (unparseable token)" "malformed_token" \
  -H "Authorization: Bearer not-a-jwt-at-all"

# 4. A well-formed, correctly-signed token presenting an `iss` this extension
#    never configured a provider for.
#
#    ⚠️ This is NOT classified `unknown_issuer` -- found while writing this
#    test, and worth knowing, not just working around: `resolveProvider`
#    (extension.go) returns the SOLE configured provider whenever exactly one
#    is configured, unconditionally, without checking the token's claimed
#    issuer at all ("if len(providerContainers) == 1 { return that one }").
#    Every collector this chart renders has exactly one provider (one
#    audience per collector, by design -- see the `opencodeOtel` comment in
#    values.yaml), so the `"...could not resolve provider"` branch in
#    `transform/classify_refusal` is unreachable dead code for every
#    collector shape this chart currently produces. An issuer mismatch
#    instead reaches `pc.Verify`, which rejects it as "id token issued by a
#    different provider" -- a `msg == "...token verification failed"` case
#    that matches neither `expired` nor `wrong_audience`, so it correctly
#    lands in the generic `verification_failed` catch-all. That is the real,
#    current behavior this asserts -- not a workaround for an untestable case.
unknown_issuer_token="$(sign --iss "http://host.docker.internal:9/never-configured" --aud "${AUDIENCE}")"
assert_refusal "issuer mismatch (resolveProvider ignores it; classified via Verify)" "verification_failed" \
  -H "Authorization: Bearer ${unknown_issuer_token}"

# 5. Real issuer, real signature, real audience -- but `exp` in the past.
#    This is the case PR #321's review found: `verification_failed`'s
#    classification used to depend on running AFTER this statement in the
#    list. Reordering them in _helpers.tpl reproduces that bug: this
#    assertion would then see `verification_failed` instead.
expired_token="$(sign --iss "${ISSUER_URL}" --aud "${AUDIENCE}" --exp-offset-seconds -3600)"
assert_refusal "expired" "expired" \
  -H "Authorization: Bearer ${expired_token}"

# 6. Real issuer, real signature, real (non-expired) `exp` -- but `aud`
#    doesn't match this collector's configured audience. Same order-
#    dependency risk as case 5.
wrong_audience_token="$(python3 "$(dirname "$0")/support/mock_oidc_issuer.py" sign "${WORKDIR}/issuer-key.pem" \
  --iss "${ISSUER_URL}" --aud "some-other-audience")"
assert_refusal "wrong_audience" "wrong_audience" \
  -H "Authorization: Bearer ${wrong_audience_token}"

# 7. Real issuer, real iss/aud/exp -- but signed with a DIFFERENT keypair the
#    real JWKS cannot verify. Neither expired nor wrong-audience: this is
#    what the generic `verification_failed` catch-all exists for.
bad_sig_token="$(sign --iss "${ISSUER_URL}" --aud "${AUDIENCE}" --bad-signature)"
assert_refusal "verification_failed (bad signature)" "verification_failed" \
  -H "Authorization: Bearer ${bad_sig_token}"
