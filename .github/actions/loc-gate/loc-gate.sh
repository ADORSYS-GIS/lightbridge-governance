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
#
# Two independent guards:
# 1. the per-file ceiling scan (below) — a measured file may not grow past its
#    recorded ceiling;
# 2. the baseline ratchet (ADORSYS-GIS/lightbridge-governance#344, ported with
#    ADORSYS-GIS/lightbridge-governance#172's hole) — the baseline ITSELF may
#    not rise in the same change unless the change carries an explicit
#    `loc-baseline-raise` PR label. Regenerating the baseline to meet grown
#    code closes nothing (lightbridge-governance's omel.rs walked 1522 -> 1837
#    across four commits, one titled "chore(loc-gate): raise the baseline").
set -euo pipefail

BASE_SHA="${INPUT_BASE_SHA:?base-sha input is required}"
HEAD_SHA="${INPUT_HEAD_SHA:-${GITHUB_SHA:-}}"
THRESHOLD="${INPUT_THRESHOLD:-200}"
BASELINE_FILE="${INPUT_BASELINE_FILE:-.github/loc-baseline.json}"
PATHS="${INPUT_PATHS:-crates app}"
LABELS="${INPUT_LABELS:-}"

if [[ -z "${HEAD_SHA}" ]]; then
  echo "::error::head-sha input is required (or GITHUB_SHA must be set)."
  exit 1
fi

# --- Override label (ADORSYS-GIS/lightbridge-governance#344) -------------------
# A baseline raise is a deliberate decision, never a side effect: the PR must
# carry the `loc-baseline-raise` label (maintainer approval of the change to
# the baseline file is then enforced by CODEOWNERS review, not by this script —
# GitHub does not expose who applied a label). Lowering entries stays
# always-free and needs no label.
OVERRIDE_LABEL="loc-baseline-raise"
# Exact comma-bounded match: the list is wired as `join(labels, ',')` with no
# spaces, and collapsing spaces to commas (an earlier draft) would let a
# look-alike label such as "wip loc-baseline-raise" satisfy the match.
has_override_label() {
  [[ ",${LABELS}," == *",${OVERRIDE_LABEL},"* ]]
}

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

# --- Baseline ratchet (ADORSYS-GIS/lightbridge-governance#344) ------------------
# Diff the committed baseline between base and head. A key whose value rises,
# or a key that appears for the first time, is a raise: fail unless the change
# carries the override label. Decreases and removals always pass — that is the
# one direction a burn-down moves in.
fail=0

# Read the checked-in baseline at a SHA; a missing file is an empty baseline.
baseline_at() {
  if git cat-file -e "${1}:${BASELINE_FILE}" 2>/dev/null; then
    git show "${1}:${BASELINE_FILE}"
  else
    echo "{}"
  fi
}

base_baseline="$(baseline_at "${BASE_SHA}")"
head_baseline="$(baseline_at "${HEAD_SHA}")"

# Emits `RAISE\t<key>\t<base>\t<head>` for an existing key that went up and
# `NEW\t<key>\t(null)\t<head>` for a first appearance. Channels and exit
# status are kept separate: a jq failure (tool missing, future jq rewording
# its errors, malformed baseline) is keyed on the EXIT CODE and fails the
# gate — never on pattern-matching jq's error text, which is fail-open.
jq_ok=1
violations="$(jq -r --argjson b "${base_baseline}" --argjson h "${head_baseline}" '
  ($h | keys[]) as $k
  | if ($b | has($k) | not) then
      "NEW\t\($k)\t(null)\t\($h[$k])"
    elif $h[$k] > $b[$k] then
      "RAISE\t\($k)\t\($b[$k])\t\($h[$k])"
    else
      empty
    end
' <<<"${head_baseline}" 2>/dev/null)" || jq_ok=0

if (( ! jq_ok )); then
  echo "::error::Could not evaluate ${BASELINE_FILE} (jq failed at base or head). \
Fail-closed: an unparseable baseline cannot be verified as unraised. Fix the \
baseline's JSON; the ${OVERRIDE_LABEL} label does not rescue this."
  fail=1
elif [[ -n "${violations}" ]]; then
  if has_override_label; then
    echo "::notice::Baseline raise approved via the ${OVERRIDE_LABEL} label for:"
    while IFS=$'\t' read -r _kind key _base head_val; do
      echo "::notice file=${key}::${key}: ceiling now ${head_val} (override: ${OVERRIDE_LABEL})"
    done <<<"${violations}"
  else
    while IFS=$'\t' read -r kind key base_val head_val; do
      if [[ "${kind}" == "NEW" ]]; then
        echo "::error file=${key}::${key}: NEW baseline entry (${head_val}). New \
grandfather entries require an explicit decision: add the ${OVERRIDE_LABEL} \
label to this PR and have a maintainer approve the ${BASELINE_FILE} change. \
Otherwise, split the file to <= ${THRESHOLD} LoC."
      else
        echo "::error file=${key}::${key}: baseline raised ${base_val} -> ${head_val}. \
The ratchet only moves DOWN. A raise is an explicit decision: add the \
${OVERRIDE_LABEL} label to this PR and have a maintainer approve the \
${BASELINE_FILE} change. Otherwise, split the file."
      fi
    done <<<"${violations}"
    echo "::error::To override: add the ${OVERRIDE_LABEL} label to this PR and have a \
maintainer approve the ${BASELINE_FILE} change. Lowering an entry never needs it."
    fail=1
  fi
fi

# --- Diff-scoped per-file ceiling scan ------------------------------------------
# Three-dot diff (merge-base..head) so only files this change actually touched
# are considered, not everything that differs from the base branch tip.
# `git diff --name-status` emits a rename as `R<score>\t<old>\t<new>` (rename
# detection is on by default). Reading only `status path` would swallow the new
# path into `path` as `old\tnew` (tab included), so the `-f` check below would
# fail and the renamed file would be skipped unmeasured. Capture the third field
# and, for renames, measure the NEW path while keeping the OLD path's
# grandfathered ceiling — a rename is "touching", not "growing".
while IFS=$'\t' read -r status path newpath; do
  # Reset every iteration: `read` only ever *sets* `baseline_key` inside the
  # `R*` arm below, so without this a rename earlier in the diff leaves it
  # set for every later `A`/`M` file too — silently swapping their real
  # baseline entry for the renamed file's old path (which is usually absent
  # from the baseline, so the file falls back to the bare 200-line
  # threshold and can fail even though it is correctly grandfathered).
  # Measured: a rename anywhere ahead of a grandfathered, untouched-in-this-
  # diff file's own `M` line reproduced this exact false failure.
  baseline_key=""
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
  echo "::error::LoC gate failed. Split the file(s) above, or — only for a genuinely \
pre-existing file that grew by legitimate means — raise its entry via the \
${OVERRIDE_LABEL} label, see the ${BASELINE_FILE} errors above."
  exit 1
fi

echo "LoC gate passed."
