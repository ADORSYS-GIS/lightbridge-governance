#!/usr/bin/env bash
#
# LoC gate — see ADORSYS-GIS/lightbridge-governance#172.
#
# Fails when a Rust file added or modified in this change exceeds the ceiling.
# Files already over the ceiling are grandfathered against a committed baseline:
# they may be touched but must not grow past the count recorded there.
#
# The gate is diff-scoped, never tree-wide: untouched legacy files never fail it,
# and a one-line edit to a grandfathered file passes as long as it does not grow.
set -euo pipefail

BASE_SHA="${INPUT_BASE_SHA:?base-sha input is required}"
HEAD_SHA="${INPUT_HEAD_SHA:-${GITHUB_SHA:-}}"
THRESHOLD="${INPUT_THRESHOLD:-200}"
BASELINE_FILE="${INPUT_BASELINE_FILE:-.github/loc-baseline.json}"
PATHS="${INPUT_PATHS:-crates app}"

if [[ -z "${HEAD_SHA}" ]]; then
  echo "::error::head-sha input is required (or GITHUB_SHA must be set)."
  exit 1
fi

# --- Load the grandfather baseline: path -> allowed line count ----------------
declare -A BASELINE
if [[ -f "${BASELINE_FILE}" ]]; then
  while IFS=$'\t' read -r path count; do
    [[ -n "${path}" ]] || continue
    BASELINE["${path}"]="${count}"
  done < <(jq -r 'to_entries[] | [.key, (.value | tostring)] | @tsv' "${BASELINE_FILE}")
else
  echo "::warning::Baseline file ${BASELINE_FILE} not found; only the ${THRESHOLD}-LoC ceiling applies."
fi

# --- Is a changed path inside one of the scanned roots? -----------------------
in_paths() {
  local file="$1"
  local root
  for root in ${PATHS}; do
    case "${file}" in
      "${root}"/* | "${root}")
        return 0
        ;;
    esac
  done
  return 1
}

# --- Diff-scoped scan ----------------------------------------------------------
# Three-dot diff (merge-base..head) so only files this change actually touched
# are considered, not everything that differs from the base branch tip.
fail=0
# `git diff --name-status` emits a rename as `R<score>\t<old>\t<new>` (rename
# detection is on by default). Reading only `status path` would swallow the new
# path into `path` as `old\tnew` (tab included), so the `-f` check below would
# fail and the renamed file would be skipped unmeasured. Capture the third field
# and, for renames, measure the NEW path while keeping the OLD path's
# grandfathered ceiling — a rename is "touching", not "growing".
while IFS=$'\t' read -r status path newpath; do
  case "${status}" in
    A | M) ;; # added, modified — measure `path`
    R*) # renamed — measure the new path, keep the old path's ceiling
      baseline_key="${path}"
      path="${newpath}"
      ;;
    *) continue ;; # D (deleted), C (copied) and anything else — ignore
  esac

  [[ "${path}" == *.rs ]] || continue
  in_paths "${path}" || continue
  [[ -f "${path}" ]] || continue

  count="$(wc -l < "${path}" | tr -d '[:space:]')"
  ceiling="${BASELINE[${baseline_key:-${path}}]:-${THRESHOLD}}"

  if (( count > ceiling )); then
    echo "::error file=${path}::${path}: ${count} LoC exceeds the allowed ceiling of ${ceiling}"
    fail=1
  fi
done < <(git diff --name-status "${BASE_SHA}...${HEAD_SHA}")

if (( fail )); then
  echo "::error::LoC gate failed. Split the file(s) above, or — only for genuinely "
  echo "::error::pre-existing files — raise their entry in ${BASELINE_FILE}."
  exit 1
fi

echo "LoC gate passed."
