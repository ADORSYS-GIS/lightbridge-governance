#!/bin/sh
# governance-auth installer.
#
# Usage:
#   curl --proto '=https' --tlsv1.2 -fsSL https://adorsys-gis.github.io/lightbridge-governance/install.sh | sh
#   curl ... | sh -s -- --version v0.5.0 --bin-dir /usr/local/bin
#
# Environment equivalents: GOVERNANCE_AUTH_VERSION, BIN_DIR, GOVERNANCE_AUTH_LIBC.
#
# ⚠️ The whole body is a function invoked on the LAST line. A `curl | sh` whose
# connection drops mid-transfer feeds `sh` a truncated file; without the
# wrapper, `sh` executes the prefix it received -- which can be "downloaded and
# chmod'd, but not verified". With it, a truncated read never reaches the call
# and the run is a no-op.
set -eu

REPO="ADORSYS-GIS/lightbridge-governance"
RELEASES="https://github.com/${REPO}/releases"

main() {
    version="${GOVERNANCE_AUTH_VERSION:-latest}"
    bin_dir="${BIN_DIR:-${HOME}/.local/bin}"
    libc="${GOVERNANCE_AUTH_LIBC:-musl}"

    while [ $# -gt 0 ]; do
        case "$1" in
            --version) version="${2:?--version needs a tag}"; shift 2 ;;
            --bin-dir) bin_dir="${2:?--bin-dir needs a path}"; shift 2 ;;
            --libc)    libc="${2:?--libc needs musl or gnu}"; shift 2 ;;
            -h|--help) usage; return 0 ;;
            *) die "unknown option: $1 (try --help)" ;;
        esac
    done

    need curl
    asset="governance-auth-$(target "$libc")"

    if [ "$version" = latest ]; then
        base="${RELEASES}/latest/download"
    else
        base="${RELEASES}/download/${version}"
    fi

    tmp="$(mktemp -d)" || die "could not create a temporary directory"
    # Every exit path from here on cleans up, including the `die`s below --
    # a failed install must not leave a half-downloaded binary in /tmp.
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "Downloading ${asset} (${version})..."
    fetch "${base}/${asset}" "${tmp}/${asset}" \
        || die "no asset '${asset}' in the ${version} release. See ${RELEASES}"

    # ⚠️ A MISSING checksum is a refusal, not a skip. `self update` already
    # refuses to install an unchecked binary, and first install is the one
    # running on a machine with no prior trust anchor at all -- so it is the
    # last place to relax the rule.
    fetch "${base}/${asset}.sha256" "${tmp}/${asset}.sha256" \
        || die "no ${asset}.sha256 in the ${version} release; refusing to install an unverified binary"

    verify "$tmp" "$asset" \
        || die "checksum mismatch for ${asset}; refusing to install. Nothing was changed."
    say "Checksum OK."

    [ -d "$bin_dir" ] || mkdir -p "$bin_dir" || die "cannot create ${bin_dir}"
    [ -w "$bin_dir" ] || die "${bin_dir} is not writable. Re-run with --bin-dir <dir>, or use sudo for a system path."

    chmod 0755 "${tmp}/${asset}"
    # ⚠️ Rename, not `cp`. `mv` within one filesystem is atomic, so a running
    # `token` invocation -- Claude Code and Codex spawn it every few minutes --
    # either sees the old inode or the new one, never a half-written file on
    # $PATH. `cp` truncates in place and has exactly that window.
    mv -f "${tmp}/${asset}" "${bin_dir}/governance-auth" \
        || die "could not install into ${bin_dir}"

    say "Installed ${bin_dir}/governance-auth"
    "${bin_dir}/governance-auth" --version 2>/dev/null || true
    path_hint "$bin_dir"
}

usage() {
    cat <<EOF
governance-auth installer

  --version <tag>   release to install (default: latest)
  --bin-dir <dir>   install location   (default: \$HOME/.local/bin)
  --libc <musl|gnu> Linux libc flavour (default: musl)

Environment: GOVERNANCE_AUTH_VERSION, BIN_DIR, GOVERNANCE_AUTH_LIBC
EOF
}

# Maps this machine onto one of the six published assets.
#
# ⚠️ These names must match `update.rs::asset_name()` character for character.
# A mismatch does not fail here -- it installs fine and then makes every later
# `self update` ask for an asset that does not exist.
target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            case "$1" in
                musl|gnu) ;;
                *) die "--libc must be musl or gnu, got: $1" ;;
            esac
            case "$arch" in
                # ⚠️ musl is the DEFAULT on Linux, not the fallback. The gnu
                # assets link against the runner's glibc 2.39 and do not start
                # on Ubuntu 22.04, Debian 12, RHEL 9 or Amazon Linux 2023 --
                # they fail in the dynamic loader before `main`, which reads
                # as a corrupt download rather than an OS mismatch.
                x86_64|amd64)   echo "x86_64-unknown-linux-$1" ;;
                aarch64|arm64)  echo "aarch64-unknown-linux-$1" ;;
                *) die "unsupported Linux architecture: ${arch}. Published assets: ${RELEASES}/latest" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64)         echo "x86_64-apple-darwin" ;;
                arm64|aarch64)  echo "aarch64-apple-darwin" ;;
                *) die "unsupported macOS architecture: ${arch}. Published assets: ${RELEASES}/latest" ;;
            esac
            ;;
        *) die "unsupported operating system: ${os}. Published assets: ${RELEASES}/latest" ;;
    esac
}

# `-L` follows the `latest/download` redirect; `--proto '=https'` and
# `--tlsv1.2` are the cheap half of the trust story and are kept here as well
# as in the documented one-liner, because the documented form gets copy-pasted
# and trimmed.
fetch() {
    curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"
}

# `sha256sum -c` reads `<hex>  <filename>` and resolves the filename relative
# to the working directory, which is why this runs in a subshell that cds into
# the download dir rather than passing an absolute path.
verify() (
    cd "$1" || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$2.sha256" >/dev/null 2>&1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -c "$2.sha256" >/dev/null 2>&1
    else
        die "neither sha256sum nor shasum is available; refusing to install an unverified binary"
    fi
)

# ⚠️ Prints, never edits. `governance-auth configure` already writes a managed
# block into up to four shell rc files, and two independent writers to the same
# `.zshrc` is how that becomes a mess someone has to untangle by hand
# (ADR-0012 §4).
path_hint() {
    case ":${PATH}:" in
        *":$1:"*) return 0 ;;
    esac
    cat >&2 <<EOF

$1 is not on your PATH. Add it:

  export PATH="$1:\$PATH"

Put that in your shell rc file yourself -- this installer deliberately does not
edit rc files, because governance-auth writes its own managed block into them.
EOF
}

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }
say()  { printf '%s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

main "$@"
