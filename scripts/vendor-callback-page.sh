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
# Overridable so the pull path is EXERCISABLE against a local registry --
# `docs/governance-auth/files.md` carries a runnable example. A refresh that
# only ever works against production is a refresh nobody can test.
CANONICAL="ghcr.io/adorsys-gis/governance-auth-callback"
ARTIFACT="${GOVERNANCE_AUTH_CALLBACK_ARTIFACT:-$CANONICAL}"
DEST="app/governance-auth/src/oauth/callback_page"

main() {
    sha="${1:-}"
    [ -n "$sha" ] || die "usage: $0 <converse-frontends commit sha>"
    command -v oras >/dev/null 2>&1 || die "oras is required: https://oras.land/docs/installation"

    tmp="$(mktemp -d)" || die "could not create a temporary directory"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    # ⚠️ Plain HTTP for loopback ONLY, and never for anything else. This is
    # the same HTTPS-or-loopback rule `crate::security` applies to the issuer
    # URL, for the same reason: a local registry has no certificate to present,
    # while a remote one downgraded to plaintext is an attack. The registry is
    # matched on host, not on a flag someone can pass by mistake.
    scheme=""
    case "$ARTIFACT" in
        # ⚠️ `'[::1]:'` is QUOTED. Unquoted, the brackets are a glob character
        # class matching a single `:` or `1`, so the IPv6 loopback pattern
        # silently matched nothing at all. Caught by shellcheck SC2102.
        127.0.0.1:*|localhost:*|'[::1]:'*) scheme="--plain-http" ;;
    esac

    say "Pulling ${ARTIFACT}:sha-${sha}..."
    # shellcheck disable=SC2086 # $scheme is a single literal flag or empty
    oras pull $scheme "${ARTIFACT}:sha-${sha}" --output "$tmp" \
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
  "artifact": "${CANONICAL}",
  "tag": "sha-${sha}"
}
JSON
    # ⚠️ `artifact` above records the CANONICAL publish location, not whatever
    # this run pulled from -- the field answers "where does this come from",
    # and a localhost ref committed into provenance would be a lie about the
    # supply chain. The override exists to exercise the pull path, so a run
    # that used it says so out loud rather than producing a file that looks
    # ordinary.
    [ "$ARTIFACT" = "$CANONICAL" ] || {
        say "warning: pulled from ${ARTIFACT}, not ${CANONICAL}."
        say "         The bytes are whatever that registry served. Do not commit this"
        say "         unless you know it is the same build."
    }

    say "Vendored ${DEST}/callback.html"
    say "  source commit: ${sha}"
    say "  sha256:        ${digest}"
    say ""
    say "Now run: cargo test -p governance-auth --bin governance-auth callback_page"
}

say() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

main "$@"
