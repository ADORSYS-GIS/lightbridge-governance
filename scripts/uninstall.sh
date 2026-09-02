#!/bin/sh
# governance-auth uninstaller.
#
#   curl --proto '=https' --tlsv1.2 -fsSL https://adorsys-gis.github.io/lightbridge-governance/uninstall.sh | sh
#   curl ... | sh -s -- --purge
#
# ⚠️ Order matters, and it is not the obvious one. The SCHEDULE goes first and
# the LOGOUT happens while the binary still exists:
#
#   1. an orphaned timer is the worst leftover -- it wakes every 300s for ever,
#      failing, on a machine whose owner believes the tool is gone;
#   2. `logout` REVOKES the refresh token at the authorization server. Deleting
#      the binary first leaves a live credential on disk with nothing able to
#      revoke it, which is a security leftover, not an untidy one.
#
# Function-wrapped and invoked on the last line, for the same truncation reason
# as install.sh -- more so here, where a half-run script deletes some things.
set -eu

LABEL="digital.camer.ai.governance-auth.copilot-push"
UNIT="governance-auth-copilot-push"
BEGIN='# >>> governance-auth otel (managed) >>>'
END='# <<< governance-auth otel (managed) <<<'

main() {
    bin_dir="${BIN_DIR:-${HOME}/.local/bin}"
    purge=0

    while [ $# -gt 0 ]; do
        case "$1" in
            --bin-dir) bin_dir="${2:?--bin-dir needs a path}"; shift 2 ;;
            --purge)   purge=1; shift ;;
            -h|--help) usage; return 0 ;;
            *) die "unknown option: $1 (try --help)" ;;
        esac
    done

    binary="${bin_dir}/governance-auth"
    [ -x "$binary" ] || binary="$(command -v governance-auth 2>/dev/null || true)"

    remove_schedule
    revoke_session "$binary"

    if [ -n "$binary" ] && [ -e "$binary" ]; then
        rm -f "$binary" && say "Removed ${binary}"
    else
        say "No governance-auth binary found (already removed?)"
    fi

    # State holds the session; cache holds OIDC discovery. Both are ours
    # outright and neither is hand-edited, so both go unconditionally.
    remove_dir "${XDG_STATE_HOME:-}" "${HOME}/.local/state/governance-auth" \
        "${HOME}/Library/Application Support/governance-auth"
    remove_dir "${XDG_CACHE_HOME:-}" "${HOME}/.cache/governance-auth" \
        "${HOME}/Library/Caches/governance-auth"

    if [ "$purge" -eq 1 ]; then
        config="${XDG_CONFIG_HOME:-${HOME}/.config}/governance-auth"
        [ -d "$config" ] && rm -rf "$config" && say "Removed ${config}"
        strip_rc_blocks
    fi

    leftovers "$purge"
}

usage() {
    cat <<EOF
governance-auth uninstaller

  --bin-dir <dir>   where the binary lives (default: \$HOME/.local/bin)
  --purge           also remove ~/.config/governance-auth and the managed
                    block from your shell rc files

Without --purge your config file and rc blocks are left alone: the config is
hand-editable and the rc files are yours.
EOF
}

# Both platforms, unconditionally -- a machine can carry a stale unit from an
# install that moved between them, and removing one that was never there is a
# no-op rather than an error.
remove_schedule() {
    if command -v launchctl >/dev/null 2>&1; then
        plist="${HOME}/Library/LaunchAgents/${LABEL}.plist"
        if [ -f "$plist" ]; then
            # `bootout` on a job that is not loaded exits non-zero; that is not
            # a failure here, so it is swallowed deliberately.
            launchctl bootout "gui/$(id -u)/${LABEL}" >/dev/null 2>&1 || true
            rm -f "$plist" && say "Removed ${plist}"
        fi
        log="${HOME}/Library/Logs/governance-auth-copilot-push.log"
        [ -f "$log" ] && rm -f "$log" && say "Removed ${log}"
    fi

    units="${HOME}/.config/systemd/user"
    if [ -f "${units}/${UNIT}.timer" ] || [ -f "${units}/${UNIT}.service" ]; then
        if command -v systemctl >/dev/null 2>&1; then
            # ⚠️ Before the files go: `disable` resolves the unit's [Install]
            # section, which needs the file to still exist, and a timer stopped
            # after its unit is gone stays loaded until the next reboot.
            systemctl --user disable --now "${UNIT}.timer" >/dev/null 2>&1 || true
        fi
        rm -f "${units}/${UNIT}.timer" "${units}/${UNIT}.service"
        say "Removed ${units}/${UNIT}.{service,timer}"
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --user daemon-reload >/dev/null 2>&1 || true
        fi
    fi
}

# Best-effort by design: an expired session, a revoked client or an offline
# authorization server must not stop an uninstall. The local files go either
# way -- this is the one chance to also invalidate the token server-side.
revoke_session() {
    [ -n "$1" ] && [ -x "$1" ] || return 0
    if "$1" logout >/dev/null 2>&1; then
        say "Session revoked and cleared."
    else
        say "warning: could not revoke the session (expired, or the IdP is unreachable)."
        say "         Local files are still removed below; revoke it in the IdP if it matters."
    fi
}

# $1 is an XDG override whose child we want; $2/$3 are the Linux and macOS
# defaults. Only one of the three exists on any given machine.
remove_dir() {
    for candidate in "${1:+$1/governance-auth}" "$2" "$3"; do
        if [ -z "$candidate" ] || [ ! -d "$candidate" ]; then
            continue
        fi
        rm -rf "$candidate" && say "Removed ${candidate}"
    done
}

# Deletes only between the markers governance-auth wrote, leaving every other
# byte of the file alone. `awk` rather than `sed -i`: BSD and GNU `sed` disagree
# about `-i`'s argument, and this script runs on both.
#
# ⚠️ The awk REFUSES an unbalanced pair (exit 3) instead of guessing, and this
# is not hypothetical caution -- the naive version deletes from the opening
# marker to end-of-file, which on a real `.zshrc` silently ate the developer's
# own aliases and exports below the block. Measured, then fixed.
# `otel.rs::upsert_block` takes the same position on the writing side, with
# `a_half_present_marker_pair_is_refused_rather_than_guessed_at` pinning it.
strip_rc_blocks() {
    for rc in "${HOME}/.bashrc" "${HOME}/.zshrc" "${HOME}/.profile" \
              "${HOME}/.bash_profile" "${HOME}/.config/fish/config.fish"; do
        [ -f "$rc" ] || continue
        grep -qF "$BEGIN" "$rc" 2>/dev/null || grep -qF "$END" "$rc" 2>/dev/null || continue
        tmp="$(mktemp)" || continue
        if awk -v b="$BEGIN" -v e="$END" '
            index($0, b) { if (skip) { bad = 1 } ; skip = 1; next }
            index($0, e) { if (!skip) { bad = 1 } ; skip = 0; next }
            !skip        { print }
            END          { if (skip) { bad = 1 } ; if (bad) { exit 3 } }
        ' "$rc" > "$tmp"; then
            mv -f "$tmp" "$rc" && say "Removed the managed block from ${rc}"
        else
            rm -f "$tmp"
            say "warning: ${rc} has an unbalanced governance-auth marker pair."
            say "         Refusing to guess where the block ends -- remove it by hand."
        fi
    done
}

# What is deliberately NOT touched, and why. Saying nothing here is how a
# developer is left with an ANTHROPIC_BASE_URL pointing at a gateway they can
# no longer authenticate to, and no idea what put it there.
leftovers() {
    cat >&2 <<EOF

Left alone (they belong to other tools, and governance-auth is gone now):
  ~/.claude/settings.json          apiKeyHelper, otelHeadersHelper, env.OTEL_*
  ~/.codex/config.toml             model_providers.governance, otel.*
  <VS Code>/User/settings.json     github.copilot.chat.otel.*

Running \`governance-auth configure\` with no --gateway-url/--otel-endpoint
BEFORE uninstalling would have retracted those automatically. To clean them by
hand, delete the keys listed above.
EOF
    [ "$1" -eq 1 ] || say "Kept ~/.config/governance-auth (re-run with --purge to remove it)."
}

say() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

main "$@"
