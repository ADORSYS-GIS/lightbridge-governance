#!/usr/bin/env bash
#
# Verifies the committed production-vs-test LoC baseline against the acceptance
# criteria of ADORSYS-GIS/lightbridge-governance#173.
#
# This is distinct from generate-loc-split-tests.sh, which tests the generator's
# counting mechanics on synthetic files. This suite asserts that the COMMITTED
# artifact (.github/loc-split-baseline.json) actually satisfies the ticket:
#
#   AC1  Per-file: total LoC, production LoC, test LoC.
#   AC2  Files ranked by production LoC (the real burn-down order).
#   AC3  Committed and regenerable by a script, not hand-maintained.
#   AC4  Measured at a named commit.
#
# The baseline is a SNAPSHOT at a named commit (AC4): it records the exact code
# state it was measured at, so the numbers are reproducible by checking out that
# commit. It is NOT expected to equal the current HEAD — the repo advances past
# it as feature work lands, and the baseline is regenerated deliberately when
# debt is reduced. AC3b therefore regenerates at the RECORDED commit, not HEAD,
# and AC4c checks the recorded commit is a real commit in the repo.
#
# Plain bash + git + jq + python3 (all preinstalled on GitHub-hosted runners).
#
# Usage: verify-loc-split-acs.sh [generator-script]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../../../.." && pwd)"
GEN="${1:-${HERE}/../generate-loc-split.sh}"
BASELINE="${REPO_ROOT}/.github/loc-split-baseline.json"

pass=0
fail=0

check() {
  local name="$1" expect="$2" actual="$3"
  if [[ "${actual}" == "${expect}" ]]; then
    echo "PASS: ${name}"
    pass=$((pass + 1))
  else
    echo "FAIL: ${name}"
    echo "  expected: ${expect}"
    echo "  actual:   ${actual}"
    fail=$((fail + 1))
  fi
}

# jq_eval <filter>: run jq against the baseline, trimming whitespace.
jq_eval() {
  jq -r "$1" "${BASELINE}"
}

[[ -f "${BASELINE}" ]] || { echo "FAIL: baseline file missing at ${BASELINE}"; exit 1; }

# ================================================================= AC1
# Per-file: total LoC, production LoC, test LoC.
echo "--- AC1: per-file total / production / test LoC ---"

# AC1a: the files array is non-empty (a baseline with nothing is not a baseline).
check "AC1a: baseline lists at least one file" "true" "$(jq_eval '.files | length > 0')"

# AC1b: every entry carries total, prod and test as numbers.
check "AC1b: every entry has total/prod/test" "true" "$(
  jq_eval '[.files[] | (has("total") and has("prod") and has("test"))] | all'
)"

# AC1c: for every file, total == prod + test (the split is exhaustive).
check "AC1c: total == prod + test for every file" "true" "$(
  jq_eval '[.files[] | (.total == (.prod + .test))] | all'
)"

# AC1d: every file's recorded total matches its real wc -l count, and the file
# exists in the tree (the baseline is not pointing at stale paths).
bad_total=0
while IFS=$'\t' read -r path total; do
  if [[ ! -f "${REPO_ROOT}/${path}" ]]; then
    echo "  FAIL: AC1d: ${path} does not exist in the tree"
    bad_total=1
    continue
  fi
  actual="$(wc -l < "${REPO_ROOT}/${path}" | tr -d '[:space:]')"
  if [[ "${actual}" != "${total}" ]]; then
    echo "  FAIL: AC1d: ${path} total ${total} != wc -l ${actual}"
    bad_total=1
  fi
done < <(jq -r '.files[] | [.path, .total] | @tsv' "${BASELINE}")
if [[ "${bad_total}" -eq 0 ]]; then
  echo "PASS: AC1d: every total matches wc -l and every path exists"
  pass=$((pass + 1))
else
  fail=$((fail + 1))
fi

# ================================================================= AC2
# Files ranked by production LoC descending — the real burn-down order.
echo "--- AC2: ranked by production LoC descending ---"
check "AC2: files sorted by prod descending" "true" "$(
  jq_eval '[.files as $f | range(0; ($f | length) - 1) | $f[.].prod >= $f[.+1].prod] | all'
)"

# ================================================================= AC3
# Committed and regenerable by a script, not hand-maintained.
echo "--- AC3: committed and regenerable ---"

# AC3a: the baseline is tracked by git (committed, not a stray working file).
if git -C "${REPO_ROOT}" ls-files --error-unmatch "${BASELINE}" >/dev/null 2>&1; then
  echo "PASS: AC3a: baseline is tracked by git"
  pass=$((pass + 1))
else
  echo "FAIL: AC3a: baseline is not tracked by git"
  fail=$((fail + 1))
fi

# AC3b: regenerating at the RECORDED commit reproduces the committed artifact.
# The baseline is a snapshot at its named commit, so we check out that commit's
# tree (a detached worktree), run the generator there, and compare. This proves
# the artifact is script-regenerable, not hand-maintained.
RECORDED_COMMIT="$(jq -r '.commit' "${BASELINE}")"
tmp_worktree="$(mktemp -d)"
tmp_out="$(mktemp)"
cleanup() {
  git -C "${REPO_ROOT}" worktree remove --force "${tmp_worktree}" >/dev/null 2>&1 || true
  rm -f "${tmp_out}"
}
trap cleanup EXIT
if git -C "${REPO_ROOT}" worktree add --detach "${tmp_worktree}" "${RECORDED_COMMIT}" >/dev/null 2>&1 \
   && ( cd "${tmp_worktree}" && bash "${GEN}" 200 "${tmp_out}" crates app >/dev/null ) \
   && jq -n --argjson a "$(jq '{commit, files}' "${BASELINE}")" \
            --argjson b "$(jq '{commit, files}' "${tmp_out}")" \
            '$a == $b' | grep -q true; then
  echo "PASS: AC3b: regenerating at the recorded commit reproduces the artifact"
  pass=$((pass + 1))
else
  echo "FAIL: AC3b: regenerating at ${RECORDED_COMMIT} differs from the committed artifact"
  if [[ -f "${tmp_out}" ]]; then
    diff <(jq -S '{commit, files}' "${BASELINE}") <(jq -S '{commit, files}' "${tmp_out}") || true
  fi
  fail=$((fail + 1))
fi

# ================================================================= AC4
# Measured at a named commit.
echo "--- AC4: measured at a named commit ---"

# AC4a: the artifact records a commit.
check "AC4a: artifact has a commit key" "true" "$(jq_eval 'has("commit")')"

# AC4b: the recorded commit is a real commit in the repo (a named commit you
# can check out and reproduce the numbers at — the point of AC4).
check "AC4b: recorded commit exists in the repo" "true" "$(
  git -C "${REPO_ROOT}" cat-file -e "$(jq -r '.commit' "${BASELINE}")^{commit}" 2>/dev/null && echo true || echo false
)"

echo
echo "loc-split AC verification: ${pass} passed, ${fail} failed"
[[ "${fail}" -eq 0 ]]
