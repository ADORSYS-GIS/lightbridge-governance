#!/usr/bin/env bash
# Ticket: config-shape bugs in the awss3 exporter (a top-level `compression`
# key that only `s3uploader` actually accepts) shipped to production and
# crashed both public collectors (`aiCliOtel`, `opencodeOtel`) with
# CrashLoopBackOff: "'awss3exporter.Config' has invalid keys: compression".
#
# assert-client-address-xff.sh already runs a rendered config under the real
# pinned image (otel/opentelemetry-collector-contrib:0.160.0), but it
# DELIBERATELY replaces `.exporters` with a `file` exporter and deletes
# `.extensions` so the OTTL probe can run unauthenticated against a
# `docker run` with no live OIDC issuer to reach -- that swap is exactly what
# let this bug through: the real `awss3` exporter block, and the `oidc`
# extension config, are never decoded by that script at all. This script
# exists to close that gap: it validates the UNMODIFIED rendered config,
# every exporter and extension included, without needing to actually start
# any component (no OIDC discovery, no S3 credentials, no egress) by using
# the collector binary's own `validate` subcommand.
#
# Sabotage-checked: reverting the `compression` key from `s3uploader` back to
# a sibling of it (the original shape) makes this fail with exactly the
# production error above, exit code 1 -- confirmed by hand before writing
# this script, not assumed.
#
# 2026-09-09: also asserts the AWS_REQUEST_CHECKSUM_CALCULATION /
# AWS_RESPONSE_CHECKSUM_VALIDATION env vars, added alongside the 0.158.0 ->
# 0.160.0 bump. Neither is expressible in `.spec.config` (they're plain pod
# env, not exporter config), so `validate` can't catch a missing one --
# `helm template` would render happily and the config would decode fine, it
# would just start silently re-attaching the checksum header that made
# Hetzner's Ceph RGW backend reject every PutObject with 403
# SignatureDoesNotMatch in production. `validate` still can't exercise the
# actual S3 call (confirmed live, by hand, against a local capture server
# comparing 0.158.0 vs 0.160.0+these-vars: the checksum header and its
# SignedHeaders entry disappear -- not repeatable here without a second
# S3-compatible backend and real credentials), so this is deliberately a
# structural check, same tier as assert-oidc-auth.sh's.
set -euo pipefail

CHART="${1:-charts/lightbridge-governance}"
YQ="${YQ_BIN:-yq}"
IMAGE="otel/opentelemetry-collector-contrib:0.160.0"

WORKDIR="${TMPDIR:-/tmp}/assert-otel-config-validates.$$"
rm -rf "${WORKDIR}"
mkdir -p "${WORKDIR}"
trap 'rm -rf "${WORKDIR}"' EXIT

fail() {
  echo "::error::${1}" >&2
  exit 1
}

# Both s3.enabled defaults to true (values.yaml), so this only needs to turn
# the collectors themselves on -- the awss3 exporter block that broke in
# production renders without any extra --set.
rendered="$(
  helm template ci "${CHART}" \
    --set aiCliOtel.enabled=true \
    --set opencodeOtel.enabled=true
)"
printf '%s\n' "${rendered}" > "${WORKDIR}/rendered.yaml"

checked=0
for fragment in ai-cli-otel opencode-otel; do
  cfg="$("${YQ}" eval-all "select(.kind == \"OpenTelemetryCollector\") | select(.metadata.name | contains(\"${fragment}\")) | .spec.config" "${WORKDIR}/rendered.yaml")"
  [ -n "${cfg// /}" ] || fail "No OpenTelemetryCollector matching \"${fragment}\" rendered."

  # Refuse to pass on an accidentally-truncated config -- an empty or
  # exporter-less document would "validate" trivially having decoded nothing.
  awss3_present="$(printf '%s\n' "${cfg}" | "${YQ}" 'has("exporters") and (.exporters | has("awss3"))')"
  [ "${awss3_present}" = "true" ] || fail "${fragment}: rendered config has no exporters.awss3 -- refusing to report a validation that checked nothing"

  # Structural: the checksum env vars live on the CR's pod spec, not inside
  # `.spec.config` -- `validate` below has no way to see them.
  manifest="$("${YQ}" eval-all "select(.kind == \"OpenTelemetryCollector\") | select(.metadata.name | contains(\"${fragment}\"))" "${WORKDIR}/rendered.yaml")"
  for var in AWS_REQUEST_CHECKSUM_CALCULATION AWS_RESPONSE_CHECKSUM_VALIDATION; do
    val="$(printf '%s\n' "${manifest}" | "${YQ}" ".spec.env[] | select(.name == \"${var}\") | .value")"
    [ "${val}" = "when_required" ] || fail "${fragment}: env ${var} = \"${val}\", want \"when_required\" (0.158.0's transfer manager ignored this entirely; 0.160.0 honors it, but only if it's actually set)"
  done

  printf '%s\n' "${cfg}" > "${WORKDIR}/${fragment}.yaml"

  if ! docker run --rm \
    -v "${WORKDIR}/${fragment}.yaml:/etc/otelcol-contrib/config.yaml:ro" \
    "${IMAGE}" validate --config=/etc/otelcol-contrib/config.yaml \
    > "${WORKDIR}/${fragment}.validate.log" 2>&1
  then
    cat "${WORKDIR}/${fragment}.validate.log" >&2
    fail "${fragment}: rendered config was rejected by ${IMAGE} -- see the decode error above"
  fi

  echo "${fragment}: rendered config (incl. the real awss3 exporter and oidc extension) is accepted by ${IMAGE}."
  checked=$((checked + 1))
done

[ "${checked}" -eq 2 ] || fail "Expected to validate 2 collectors, validated ${checked}."
