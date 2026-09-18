#!/usr/bin/env bash
#
# Self-tests for the LoC gate (ADORSYS-GIS/lightbridge-authz#622).
#
# Plain bash against synthetic two-commit git repositories — no bats, nothing
# beyond git + jq (both preinstalled on GitHub-hosted runners). Each case
# builds a base commit and a head commit, points the gate at both SHAs, and
# asserts on the exit code and the failure message.
#
# Layout: the tested script is `../loc-gate.sh` relative to this file's dir.
#
# Usage: loc-gate-tests.sh [gate-script]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${1:-${HERE}/../loc-gate.sh}"

pass=0
fail=0

RUN_EXIT=0
RUN_OUTPUT=""

# new_repo <dir>: create an empty git repo with a crates/demo tree.
new_repo() {
  local dir="$1"
  rm -rf "${dir}"
  mkdir -p "${dir}/crates/demo" "${dir}/.github"
  (
    cd "${dir}"
    git init -q
    git config user.email gate@example.test
    git config user.name loc-gate-tests
    # A non-Rust placeholder content file so the workspace is never empty.
    seq 1 50 >"${dir}/crates/demo/lib.rs"
  ) >/dev/null
}

commit_base() {
  git -C "${REPO}" add -A
  git -C "${REPO}" -c user.email=gate@example.test -c user.name=t commit -q -m base
  BASE_SHA="$(git -C "${REPO}" rev-parse HEAD)"
}

commit_head() {
  git -C "${REPO}" add -A
  git -C "${REPO}" -c user.email=gate@example.test -c user.name=t commit -q -m head
  HEAD_SHA="$(git -C "${REPO}" rev-parse HEAD)"
}

# run_gate [labels]: invoke the gate inside the synthetic repo, whose working
# tree sits at the head commit (the per-file scan measures the working tree
# with `wc -l < path`).
run_gate() {
  local labels="${1:-}"
  RUN_EXIT=0
  RUN_OUTPUT="$(
    cd "${REPO}" &&
      INPUT_BASE_SHA="${BASE_SHA}" \
      INPUT_HEAD_SHA="${HEAD_SHA}" \
      INPUT_BASELINE_FILE=".github/loc-baseline.json" \
      INPUT_PATHS="crates" \
      INPUT_LABELS="${labels}" \
      bash "${GATE}" 2>&1
  )" || RUN_EXIT=$?
}

check() {
  local name="$1" expect="$2" actual="$3"
  if [[ "${actual}" == "${expect}" ]]; then
    echo "PASS: ${name}"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name}"
    echo "  expected exit: ${expect}"
    echo "  actual exit:   ${actual}"
    sed 's/^/    /' <<<"${RUN_OUTPUT}"
    fail=$((fail + 1))
  fi
}

output_contains() {
  if grep -qF -- "$2" <<<"${RUN_OUTPUT}"; then
    echo "PASS: $1"
    pass=$((pass + 1))
  else
    echo "FAIL: $1"
    echo "  expected output to contain: $2"
    echo "  actual output:"
    sed 's/^/    /' <<<"${RUN_OUTPUT}"
    fail=$((fail + 1))
  fi
}

# ================================================================= case 1
# RAISE of an existing entry, no label → must FAIL, naming file + both
# numbers + the override (AC1, AC2). The .rs itself is untouched in the head
# commit, so any failure can only come from the baseline ratchet — the
# per-file ceiling scan cannot fire.
REPO="$(mktemp -d "${TMPDIR:-/tmp}/loc-gate-XXXXXX")"
new_repo "${REPO}"
echo '{"crates/demo/lib.rs": 50}' >"${REPO}/.github/loc-baseline.json"
commit_base
echo '{"crates/demo/lib.rs": 70}' >"${REPO}/.github/loc-baseline.json"
commit_head
run_gate
check "raise-without-label: gate exits nonzero" "1" "${RUN_EXIT}"
output_contains "raise-without-label: names the file" "crates/demo/lib.rs"
output_contains "raise-without-label: shows the base number" "50"
output_contains "raise-without-label: shows the head number" "70"
output_contains "raise-without-label: states the override" "loc-baseline-raise"

# ================================================================= case 2
# RAISE with the loc-baseline-raise label → must PASS (the override path).
run_gate "loc-baseline-raise"
check "raise-with-label: gate exits zero" "0" "${RUN_EXIT}"

# ================================================================= case 3
# NEW key in the baseline, no label → must FAIL (AC1: any new key). The
# file's count equals its own new entry, so the per-file scan passes it.
echo '{"crates/demo/lib.rs": 50, "crates/demo/new.rs": 250}' >"${REPO}/.github/loc-baseline.json"
seq 1 250 >"${REPO}/crates/demo/new.rs"
commit_head
run_gate
check "new-key-without-label: gate exits nonzero" "1" "${RUN_EXIT}"
output_contains "new-key-without-label: names the file" "crates/demo/new.rs"
output_contains "new-key-without-label: states the override" "loc-baseline-raise"

# ================================================================= case 4
# NEW key with the label → must PASS. Note the per-file scan must ALSO pass
# it: the raised entry (250) is the measured working-tree count.
run_gate "loc-baseline-raise"
check "new-key-with-label: gate exits zero" "0" "${RUN_EXIT}"

# ================================================================= case 5
# DECREASE of an entry (a split shrank the file) → must PASS.
echo '{"crates/demo/lib.rs": 50}' >"${REPO}/.github/loc-baseline.json"
rm "${REPO}/crates/demo/new.rs"
commit_head
run_gate
check "decrease/removal: gate exits zero" "0" "${RUN_EXIT}"

# ================================================================= case 6
# Existing behaviour preserved: baseline unchanged, grandfathered file grows
# past its recorded ceiling → must FAIL even WITH the label. This is the
# per-file scan, not the ratchet — a raise cannot be used to exceed it.
BASE_SHA="$(git -C "${REPO}" rev-parse HEAD)"
seq 1 120 >"${REPO}/crates/demo/lib.rs"
commit_head
run_gate "loc-baseline-raise"
check "growth-past-ceiling: gate exits nonzero even with the label" "1" "${RUN_EXIT}"
output_contains "growth-past-ceiling: names the ceiling breach" "exceeds the allowed ceiling"

# ================================================================= case 7
# Baseline untouched (PR does not edit it), file shrinks within its ceiling →
# gate passes a normal change with no ratchet involvement.
seq 1 40 >"${REPO}/crates/demo/lib.rs"
commit_head
run_gate
check "normal-change-no-baseline-edit: gate exits zero" "0" "${RUN_EXIT}"

# ================================================================= case 8
# MALFORMED baseline at head (not JSON) → must FAIL even before the raise
# decision: an unparseable baseline cannot be verified as unraised, and the
# override label does not rescue it (fail-closed, exit-status keyed — the
# earlier draft detected jq errors by message text, which missed failures
# whose text did not match, silently passing a raise).
echo 'NOT JSON AT ALL' >"${REPO}/.github/loc-baseline.json"
commit_head
run_gate
check "malformed-baseline-head: gate exits nonzero" "1" "${RUN_EXIT}"
output_contains "malformed-baseline-head: states fail-closed refusal" "Fail-closed"

run_gate "loc-baseline-raise"
check "malformed-baseline-head: label does NOT rescue an unparseable baseline" "1" "${RUN_EXIT}"

rm -rf "${REPO}"; REPO="$(mktemp -d "${TMPDIR:-/tmp}/loc-gate-XXXXXX")"
new_repo "${REPO}"
echo '{"crates/demo/lib.rs": 50}' >"${REPO}/.github/loc-baseline.json"
commit_base
echo '{"crates/demo/lib.rs": 70}' >"${REPO}/.github/loc-baseline.json"
commit_head

# ================================================================= case 9
# Label look-alikes must NOT unlock the override: the list is a comma-joined
# exact match, so names containing loc-baseline-raise as one space-delimited
# word or as a suffix/prefix are not the label.
run_gate "wip loc-baseline-raise"
check "look-alike-with-space: raise still fails" "1" "${RUN_EXIT}"
run_gate "pending-loc-baseline-raise"
check "look-alike-suffix: raise still fails" "1" "${RUN_EXIT}"
run_gate "loc-baseline-raise-X"
check "look-alike-prefixed: raise still fails" "1" "${RUN_EXIT}"

rm -rf "${REPO}"

echo
echo "loc-gate self-tests: ${pass} passed, ${fail} failed"
[[ "${fail}" -eq 0 ]]
