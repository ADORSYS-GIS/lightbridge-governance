#!/bin/sh
# Refreshes the vendored OAuth callback page from GHCR.
#
# The page is built in ADORSYS-GIS/converse-frontends (apps/governance-auth) and
# published as an OCI artifact. This pulls one build, by the SOURCE COMMIT SHA,
# and rewrites both the artifact and its provenance record.
#
# ⚠️ This is deliberately NOT part of `cargo build`. `include_str!` runs at
# compile time, so pulling during the build would put the network on the path of
# every build, break offline and air-gapped builds, and make the binary's
# contents depend on when it was compiled rather than on what is committed.
# Refreshing is an explicit act that produces a reviewable diff.
#
#   scripts/vendor-callback-page.sh <source-commit-sha>
set -eu

REPO="ADORSYS-GIS/converse-frontends"
ARTIFACT="ghcr.io/adorsys-gis/governance-auth-callback"
DEST="app/governance-auth/src/oauth/callback_page"

main() {
    sha="${1:-}"
    [ -n "$sha" ] || die "usage: $0 <converse-frontends commit sha>"
    command -v oras >/dev/null 2>&1 || die "oras is required: https://oras.land/docs/installation"

    tmp="$(mktemp -d)" || die "could not create a temporary directory"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "Pulling ${ARTIFACT}:sha-${sha}..."
    oras pull "${ARTIFACT}:sha-${sha}" --output "$tmp" \
        || die "could not pull sha-${sha}. Is that commit built and pushed?"

    src="${tmp}/index.html"
    [ -f "$src" ] || die "the artifact did not contain index.html"

    # The same property the other repo's build gate enforces, re-checked on
    # arrival: a registry is a different trust boundary from a build job, and
    # this file is compiled into a binary that serves it on loopback.
    grep -q '<link' "$src" && die "refusing: the artifact contains a <link> element"
    grep -q '@import' "$src" && die "refusing: the artifact contains a CSS @import"
    grep -c '__GOVERNANCE_AUTH_CALLBACK_STATUS__' "$src" | grep -qx 1 \
        || die "refusing: the status placeholder is not present exactly once"

    digest="$(shasum -a 256 "$src" | cut -d' ' -f1)"
    cp "$src" "${DEST}/callback.html"
    cat > "${DEST}/callback.source.json" <<JSON
{
  "repository": "${REPO}",
  "path": "apps/governance-auth",
  "commit": "${sha}",
  "sha256": "${digest}",
  "artifact": "${ARTIFACT}",
  "tag": "sha-${sha}"
}
JSON
    say "Vendored ${DEST}/callback.html"
    say "  source commit: ${sha}"
    say "  sha256:        ${digest}"
    say ""
    say "Now run: cargo test -p governance-auth --bin governance-auth callback_page"
}

say() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

main "$@"
