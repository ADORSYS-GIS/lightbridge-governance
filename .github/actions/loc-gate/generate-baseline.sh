#!/usr/bin/env bash
#
# Regenerates the LoC-gate grandfather baseline (see ADORSYS-GIS/lightbridge-governance#172).
#
# The baseline records every Rust file already over the ceiling together with its
# current line count. The gate lets those files be touched but not grow past the
# recorded count. Re-run this only when the debt is deliberately reduced (a file
# split, a test module extracted) — never to paper over a file that grew.
#
# Usage: generate-baseline.sh [threshold] [output-file]
set -euo pipefail

THRESHOLD="${1:-200}"
OUT="${2:-.github/loc-baseline.json}"
PATHS="${3:-crates app}"

roots=()
for root in ${PATHS}; do
  roots+=("${root}")
done

# `wc -l` over multiple files emits a trailing "total" row; drop it so it is not
# mistaken for a real file.
find "${roots[@]}" -name '*.rs' -not -path '*/target/*' -print0 \
  | xargs -0 wc -l \
  | awk -v t="${THRESHOLD}" '$1 > t && $2 != "total" { print $2 "\t" $1 }' \
  | sort \
  | jq -R -s '
      [ split("\n")[] | select(length > 0) ]
      | map( split("\t") | { key: .[0], value: (.[1] | tonumber) } )
      | from_entries
    ' > "${OUT}"

echo "Wrote ${OUT} ($(jq 'length' "${OUT}") grandfathered files)."
