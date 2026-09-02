#!/usr/bin/env bash
# Asserts the aiCliOtel collector's OIDC auth gate survives chart rendering.
#
# Pattern A (RFC-0003 §4) is the user-present push: laptop OTLP hits the
# public otel.<domain> ingress and must be authenticated by the collector's
# `oidcauthextension` -- otherwise anyone who can reach the endpoint can
# write telemetry rows (ticket #170 AC1-3).
#
# The refusal mechanism lives inside the OpenTelemetry Collector, not in Rust
# this repo controls, so the only thing we can pin here is the *structural
# presence* of the gate in the rendered manifest. This script renders the
# chart (Helm's own `tpl`/`include`/conditionals are the only faithful way)
# and asserts the five things that must be true for the gate to exist at all.
#
# Each failure names the exact guard that is missing, so a chart edit that
# accidentally drops the authenticator fails here rather than silently in
# production. Sabotage-checked: removing `auth: authenticator: oidc` from
# otelcollector-ai-cli.yaml makes assertion 4 fail naming that line.
set -euo pipefail

CHART="${1:-charts/lightbridge-governance}"
OIDC=oidc

# The aiCliOtel collector is off by default (values.yaml); it must be enabled
# or the CR below is never emitted and every assertion below would pass by
# finding nothing -- the same green-but-ran-nothing failure mode this script
# exists to prevent. Render it explicitly and refuse to proceed if absent.
rendered="$("${HELM_BIN:-helm}" template ci "${CHART}" --set aiCliOtel.enabled=true)"

# The chart renders two OpenTelemetryCollector CRs (the copilot-sync collector
# is enabled by default alongside this one). Only the ai-cli collector carries
# the OIDC gate, so target it by name -- the other must not satisfy these
# assertions by accident.
collector="$(
  printf '%s\n' "${rendered}" \
    | yq eval-all 'select(.kind == "OpenTelemetryCollector") | select(.metadata.name | contains("ai-cli-otel"))' -
)"

if [ -z "${collector// /}" ]; then
  echo "::error::No OpenTelemetryCollector rendered with aiCliOtel.enabled=true." >&2
  echo "Either the CR stopped rendering or the enable override changed." >&2
  exit 1
fi

fail() {
  echo "::error::OIDC auth gate missing: $1" >&2
  exit 1
}

# 1. The `oidc` extension exists.
if [ "$(printf '%s\n' "${collector}" | yq ".spec.config.extensions.${OIDC} == null")" = "true" ]; then
  fail "spec.config.extensions.oidc is absent"
fi

# 2. issuer_url is a non-empty string.
issuer="$(printf '%s\n' "${collector}" | yq '.spec.config.extensions.oidc.issuer_url // ""')"
if [ -z "${issuer}" ] || [ "${issuer}" = "null" ]; then
  fail "spec.config.extensions.oidc.issuer_url is empty"
fi

# 3. audience is a non-empty string.
audience="$(printf '%s\n' "${collector}" | yq '.spec.config.extensions.oidc.audience // ""')"
if [ -z "${audience}" ] || [ "${audience}" = "null" ]; then
  fail "spec.config.extensions.oidc.audience is empty"
fi

# 4. The OTLP HTTP receiver demands the oidc authenticator.
auth="$(printf '%s\n' "${collector}" | yq '.spec.config.receivers.otlp.protocols.http.auth.authenticator // ""')"
if [ "${auth}" != "${OIDC}" ]; then
  fail "spec.config.receivers.otlp.protocols.http.auth.authenticator != \"oidc\" (got: ${auth})"
fi

# 5. The oidc extension is wired into the service extensions list.
if ! printf '%s\n' "${collector}" | yq -e '.spec.config.service.extensions[] | select(. == "oidc")' >/dev/null 2>&1; then
  fail "spec.config.service.extensions does not include \"oidc\""
fi

echo "OIDC auth gate present: extension, issuer, audience, receiver authenticator, service extensions."
