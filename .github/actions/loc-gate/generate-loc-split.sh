#!/usr/bin/env bash
#
# Regenerates the production-vs-test LoC baseline (ADORSYS-GIS/lightbridge-governance#173).
#
# This is a PLANNING artifact, deliberately separate from .github/loc-baseline.json
# (the #172 grandfather ceiling, a flat path->count map that the CI gate consumes).
# The grandfather ceiling must stay flat — loc-gate.sh diffs it between base and
# head — so the split lives in its own file and its own generator.
#
# What it records, per over-threshold Rust file:
#   - total:  wc -l line count
#   - prod:   lines outside any brace-matched `#[cfg(test)]` block
#   - test:   lines inside such blocks
# ranked by PRODUCTION LoC descending — the real burn-down order, because the
# headline total overstates the debt (config.rs was ~840 lines of precedence
# tests; extracting tests is cheap, splitting production logic is not).
#
# Test-only files are excluded from the burn-down list: `**/tests/**` directories
# and `**/tests.rs` files are 100% test code by construction, so ranking them by
# "production" LoC would put a test file at the top of a production burn-down.
#
# The artifact records the commit it was measured at (AC4) and is regenerable by
# this script (AC3), never hand-maintained.
#
# Usage: generate-loc-split.sh [threshold] [output-file] [roots...]
#   threshold   files over this many TOTAL lines are included (default 200)
#   output-file default .github/loc-split-baseline.json
#   roots       space-separated roots to scan (default "crates app")
#
# Requires: git, jq, python3 (all preinstalled on GitHub-hosted runners; python3
# is used only for the brace-matching counter, never by the server image).
set -euo pipefail

THRESHOLD="${1:-200}"
OUT="${2:-.github/loc-split-baseline.json}"
shift 2 || true
PATHS="${*:-crates app}"

COMMIT="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
DATE="$(date -u +%Y-%m-%d)"

roots=()
for root in ${PATHS}; do
  roots+=("${root}")
done

# Brace-match `#[cfg(test)]` blocks and emit `path\ttotal\tprod\ttest\ttest_only`
# per file. Paths arrive as argv (NUL-safe via xargs -0). The counter is a small
# state machine:
#   - `#[cfg(test)] mod tests;`            (no brace)  -> not inline, skip
#   - `#[cfg(test)] mod tests { ... }`     (inline)    -> counted as test
#   - `#[cfg(test)] fn foo() { ... }`      (inline)    -> counted as test
#   - `#[cfg(test)]` on its own line, brace on the next line -> counted
# A line is "test" iff it falls inside a brace-matched cfg(test) block.
#
# `test_only` is 1 when the file is 100% test code and must be excluded from the
# production burn-down: it lives under `tests/`, is a `tests.rs` sibling, or is
# declared as an external `#[cfg(test)] mod <name>;` from another file (e.g.
# `app/governance-ctl/src/test_support.rs`, gated in main.rs) — such a module is
# compiled only into test binaries and is not production debt.
count() {
  local tmp
  tmp="$(mktemp)"
  cat > "${tmp}" <<'PY'
import sys
import os
import re

def find_test_only_modules(paths):
    """Paths declared as external `#[cfg(test)] mod <name>;` from another file."""
    test_only = set()
    for path in paths:
        with open(path, encoding="utf-8", errors="replace") as f:
            lines = f.read().splitlines()
        i = 0
        n = len(lines)
        while i < n:
            if "#[cfg(test)]" in lines[i]:
                j = i + 1
                while j < n and not lines[j].strip():
                    j += 1
                # the `mod <name>;` may sit on the attribute line or the next one
                for k in (i, j):
                    if k >= n:
                        continue
                    m = re.match(r"\s*mod\s+([A-Za-z0-9_]+)\s*;", lines[k])
                    if m:
                        name = m.group(1)
                        d = os.path.dirname(path)
                        c1 = os.path.join(d, name + ".rs")
                        c2 = os.path.join(d, name, "mod.rs")
                        if os.path.exists(c1):
                            test_only.add(c1)
                        elif os.path.exists(c2):
                            test_only.add(c2)
                        break
                i = j + 1
                continue
            i += 1
    return test_only

def split_file(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = f.read().splitlines()
    total = len(lines)
    test_lines = set()
    i = 0
    n = len(lines)
    while i < n:
        if "#[cfg(test)]" in lines[i]:
            # item starts at the next non-blank line
            j = i + 1
            while j < n and not lines[j].strip():
                j += 1
            kind, start = find_block_start(lines, j, n)
            if kind != "brace":
                # external `mod tests;` (or nothing): not an inline block
                i = start + 1 if start < n else n
                continue
            # brace-match from the opening brace; every line j..closing is test
            k = start
            depth = 0
            while k < n:
                for ch in lines[k]:
                    if ch == "{":
                        depth += 1
                    elif ch == "}":
                        depth -= 1
                        if depth == 0:
                            for t in range(j, k + 1):
                                test_lines.add(t)
                            i = k + 1
                            break
                else:
                    k += 1
                    continue
                break
            else:
                # ran off the end inside a block (unterminated) -> count to EOF
                for t in range(j, n):
                    test_lines.add(t)
                break
            continue
        i += 1
    return total, total - len(test_lines), len(test_lines)

def find_block_start(lines, j, n):
    """First of '{' or ';' at/after line j. Returns ('brace'|'semi'|None, line)."""
    for k in range(j, n):
        line = lines[k]
        b = line.find("{")
        s = line.find(";")
        if b == -1 and s == -1:
            continue
        if b == -1:
            return ("semi", k)
        if s == -1:
            return ("brace", k)
        return ("brace", k) if b < s else ("semi", k)
    return (None, n)

paths = sys.argv[1:]
test_only_mods = find_test_only_modules(paths)
for path in paths:
    total, prod, test = split_file(path)
    is_to = 1 if (path in test_only_mods or "/tests/" in path or path.endswith("/tests.rs")) else 0
    print(f"{path}\t{total}\t{prod}\t{test}\t{is_to}")
PY
  xargs -0 python3 "${tmp}"
  rm -f "${tmp}"
}

find "${roots[@]}" -name '*.rs' -not -path '*/target/*' -print0 \
  | count \
  | awk -v t="${THRESHOLD}" '$2 > t && $5 == 0 { print }' \
  | sort -t $'\t' -k3,3nr \
  | jq -R -s --arg commit "${COMMIT}" --arg date "${DATE}" --argjson threshold "${THRESHOLD}" '
      [ split("\n")[] | select(length > 0) ]
      | map( split("\t") | { path: .[0], total: (.[1] | tonumber), prod: (.[2] | tonumber), test: (.[3] | tonumber) } )
      | { commit: $commit, measured_at: $date, threshold: $threshold, files: . }
    ' > "${OUT}"

echo "Wrote ${OUT}: $(jq '.files | length' "${OUT}") over-threshold files, ranked by production LoC, at ${COMMIT}."
