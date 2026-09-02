#!/usr/bin/env bash
# Asserts every PUBLIC OTLP collector's OIDC auth gate survives chart rendering.
#
# Pattern A (RFC-0003 §4) is the user-present push: laptop OTLP hits a public
# otel*.<domain> ingress and must be authenticated by the collector's
# `oidcauthextension` -- otherwise anyone who can reach the endpoint can
# write telemetry rows (ticket #170 AC1-3).
#
# There are TWO such collectors, `aiCliOtel` and `opencodeOtel`, because
# `oidcauthextension` accepts exactly ONE `audience` string per extension
# instance and the two client fleets present tokens with different `aud`
# claims from the same issuer (see values.yaml's `opencodeOtel` block for the
# live 401 transcript). Both are checked here, by name: a gate that only holds
# for one of two public endpoints is not a gate.
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
# the shared `publicOtelCollector` define makes assertion 4 fail naming that
# line, for both collectors.
set -euo pipefail

CHART="${1:-charts/lightbridge-governance}"
OIDC=oidc

# Both public collectors are off by default (values.yaml); they must be
# enabled or the CRs below are never emitted and every assertion would pass by
# finding nothing -- the same green-but-ran-nothing failure mode this script
# exists to prevent. Render them explicitly and refuse to proceed if absent.
rendered="$(
  "${HELM_BIN:-helm}" template ci "${CHART}" \
    --set aiCliOtel.enabled=true \
    --set opencodeOtel.enabled=true
)"

# The chart renders three OpenTelemetryCollector CRs (the copilot-sync
# collector is enabled by default alongside these two). Only the public ones
# carry the OIDC gate, so target each by name -- the copilot collector must
# not satisfy these assertions by accident, and neither public collector may
# stand in for the other.
#
# Each entry is `<CR name fragment>:<the audience it must trust>`. The
# audience is asserted by VALUE, not merely non-empty: the whole reason two
# collectors exist is that one trusted audience cannot serve both fleets, so
# a copy-paste that left both trusting `governance-auth-cli` would silently
# lock OpenCode out again -- exactly the production 401 this pair was built
# to fix.
COLLECTORS=(
  "ai-cli-otel:governance-auth-cli"
  "opencode-otel:opencode-cli"
)

for entry in "${COLLECTORS[@]}"; do
  fragment="${entry%%:*}"
  want_audience="${entry#*:}"

  collector="$(
    printf '%s\n' "${rendered}" \
      | yq eval-all "select(.kind == \"OpenTelemetryCollector\") | select(.metadata.name | contains(\"${fragment}\"))" -
  )"

  if [ -z "${collector// /}" ]; then
    echo "::error::No OpenTelemetryCollector matching \"${fragment}\" rendered with both public collectors enabled." >&2
    echo "Either the CR stopped rendering or the enable override changed." >&2
    exit 1
  fi

  fail() {
    echo "::error::OIDC auth gate missing on ${fragment}: $1" >&2
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

  # 3. audience is exactly the one this collector exists to trust.
  audience="$(printf '%s\n' "${collector}" | yq '.spec.config.extensions.oidc.audience // ""')"
  if [ "${audience}" != "${want_audience}" ]; then
    fail "spec.config.extensions.oidc.audience != \"${want_audience}\" (got: ${audience})"
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

  echo "OIDC auth gate present on ${fragment}: extension, issuer, audience=${audience}, receiver authenticator, service extensions."
done

# Refuse to pass having checked nothing: the two public collectors must not
# have collapsed into one CR (a shared name would make both `contains()`
# selections match the same object and hide a missing collector).
public_count="$(
  printf '%s\n' "${rendered}" \
    | yq eval-all 'select(.kind == "OpenTelemetryCollector") | select(.spec.config.extensions.oidc != null) | .metadata.name' - \
    | grep -v '^---$' | sort -u | wc -l | tr -d ' '
)"
if [ "${public_count}" -ne "${#COLLECTORS[@]}" ]; then
  echo "::error::Expected ${#COLLECTORS[@]} distinct OIDC-gated collectors, found ${public_count}." >&2
  exit 1
fi
