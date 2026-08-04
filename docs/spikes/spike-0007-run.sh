#!/usr/bin/env bash
# Spike 0007 — GitHub App installation token on Copilot report endpoints.
#
# Companion to docs/spikes/0007-github-app-token-on-copilot-reports.md. Runs the
# ticket's manual test plan and appends labelled evidence to an evidence file.
# Throwaway scaffolding: when implementation (#12) lands, secrets move to ESO and
# this script is deleted.
#
# Secrets NEVER leave the process: the App JWT and installation token exist only
# in shell variables, are never echoed, and never written to the evidence file.
# Raw response bodies (which contain signed download URLs on a 200) are written
# to $SPIKE_BODY_FILE for parsing only and removed on every exit path via trap.
#
# Prerequisites (checked at start): curl, jq, openssl, and the throwaway GitHub
# App registered and installed on the org (ticket #7 test plan step 1).
#
# Usage:
#   GH_APP_ID=123456 \
#   GH_APP_PRIVATE_KEY_FILE=/path/to/app.private-key.pem \
#   ./spike-0007-run.sh                    # baseline (full permissions)
#   ./spike-0007-run.sh --after-policy     # after the org policy toggle is Enabled
#   ./spike-0007-run.sh --no-members       # after Members: Read is removed from the App
#
# Env:
#   GH_APP_ID                GitHub App ID (required)
#   GH_APP_PRIVATE_KEY_FILE  Path to the App's PEM private key (required)
#   GH_ORG                   Org to test (default: adorsys-gis)
#   DAY                      Report day YYYY-MM-DD (default: yesterday UTC)
#   API_VERSION              GitHub REST API version (default: 2026-03-10)
#   EVIDENCE_FILE            Where labelled evidence lines are appended
#                            (default: $PWD/spike-0007-evidence.txt)

set -euo pipefail

# Raw response bodies hold signed download URLs on a 200 (time-limited
# credentials) — the file must not survive any exit path, including set -e
# aborts and Ctrl-C, so removal is a trap, not a happy-path line.
SPIKE_BODY_FILE="/tmp/spike-body.$$"
trap 'rm -f "$SPIKE_BODY_FILE"' EXIT

# ── config ────────────────────────────────────────────────────────────────────
GH_ORG="${GH_ORG:-adorsys-gis}"
API_VERSION="${API_VERSION:-2026-03-10}"
EVIDENCE_FILE="${EVIDENCE_FILE:-$PWD/spike-0007-evidence.txt}"
if [[ -z "${DAY:-}" ]]; then
  # bash 3.2 (macOS default) does not handle `case` inside a command substitution
  # inside a default-value expansion; keep it a plain conditional.
  if [[ "$(uname -s)" == "Darwin" ]]; then
    DAY=$(date -u -v-1d +%F)
  else
    DAY=$(date -u -d yesterday +%F)
  fi
fi
LABEL="baseline"

for arg in "$@"; do
  case "$arg" in
    --after-policy) LABEL="after-policy-toggle" ;;
    --no-members) LABEL="without-members-read" ;;
    --help | -h)
      grep '^#' "$0" | grep -v '^#!' | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $arg (see --help)" >&2; exit 2 ;;
  esac
done

fail() { echo "error: $*" >&2; exit 1; }

# The "green does not mean tested" rule applied to a spike: an env-var-absent
# early return that reports success is exactly the failure this guard prevents.
[[ -n "${GH_APP_ID:-}" ]] || fail "GH_APP_ID is not set (throwaway App from test plan step 1)"
[[ -n "${GH_APP_PRIVATE_KEY_FILE:-}" && -f "$GH_APP_PRIVATE_KEY_FILE" ]] \
  || fail "GH_APP_PRIVATE_KEY_FILE is not set or not a file (test plan step 1)"
for tool in curl jq openssl; do
  command -v "$tool" >/dev/null || fail "$tool is required"
done

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

# ── step 2: mint an installation token (never printed) ───────────────────────
mint_install_token() {
  local now exp header payload signing_input jwt
  now=$(date +%s)
  exp=$((now + 600))
  header=$(printf '{"alg":"RS256","typ":"JWT"}' | b64url)
  payload=$(printf '{"iat":%d,"exp":%d,"iss":%s}' "$now" "$exp" "\"$GH_APP_ID\"" | b64url)
  signing_input="$header.$payload"
  jwt="$signing_input.$(printf '%s' "$signing_input" \
    | openssl dgst -sha256 -sign "$GH_APP_PRIVATE_KEY_FILE" \
    | b64url)"

  local installation_id token_json
  installation_id=$(curl -sS -f \
    -H "Authorization: Bearer $jwt" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: $API_VERSION" \
    "https://api.github.com/app/installations" \
    | jq -r --arg org "$GH_ORG" '.[] | select((.account.login | ascii_downcase) == ($org | ascii_downcase)) | .id' | head -1)
  [[ -n "$installation_id" ]] || fail "no App installation found for org $GH_ORG (is the App installed there?)"

  token_json=$(curl -sS -f -X POST \
    -H "Authorization: Bearer $jwt" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: $API_VERSION" \
    "https://api.github.com/app/installations/$installation_id/access_tokens")
  jq -r '.token' <<<"$token_json"
}

# ── step 3/4: probe the report endpoint, record status + redacted body ───────
probe() {
  local endpoint="$1" status body message host url
  status=$(curl -sS -o "$SPIKE_BODY_FILE" -w '%{http_code}' \
    -H "Authorization: Bearer $INSTALL_TOKEN" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: $API_VERSION" \
    "https://api.github.com$endpoint")
  body=$(cat "$SPIKE_BODY_FILE")
  rm -f "$SPIKE_BODY_FILE"
  message=$(jq -r '.message // empty' <<<"$body" 2>/dev/null || true)

  echo "[$LABEL] GET $endpoint -> HTTP $status"
  echo "[$LABEL] GET $endpoint -> HTTP $status ${message:+| $message}" >>"$EVIDENCE_FILE"
  [[ -z "$message" ]] || echo "[$LABEL] redacted body: {\"message\": $(jq -R . <<<"$message")}"

  if [[ "$status" == "200" ]]; then
    # Step 6: identify the signed-download host, verbatim, for the egress policy.
    url=$(jq -r '.download_links[0] // empty' <<<"$body")
    if [[ -n "$url" ]]; then
      host=$(printf '%s' "$url" | sed -E 's|https?://([^/]+)/.*|\1|')
      echo "[$LABEL] signed-download host: $host"
      echo "[$LABEL] signed-download host: $host" >>"$EVIDENCE_FILE"
      echo "[$LABEL] first report file is NDJSON; refusing to print its contents"
    else
      echo "[$LABEL] 200 but no download_links in body"
    fi
  elif [[ "$status" == "403" ]]; then
    case "$message" in
      *"policy must be enabled"*)
        echo "[$LABEL] => org Copilot usage metrics policy is DISABLED."
        echo "   Flip it: https://github.com/organizations/$GH_ORG/settings/policies/copilot"
        echo "   (Copilot > Policies > Features > 'Copilot usage metrics' > Enabled), then re-run"
        echo "   with --after-policy." ;;
      *)
        echo "[$LABEL] => 403 without the policy message. Likely a permission problem:"
        echo "   confirm the App holds Copilot metrics: Read, Copilot seat management: Read,"
        echo "   Members: Read, Metadata: Read (or, for --no-members runs, this is expected)." ;;
    esac
  elif [[ "$status" == "404" ]]; then
    echo "[$LABEL] => 404. Day $DAY may not exist yet (org-level data starts 2025-12-12;"
    echo "   freshness ~2 days). Re-run with DAY=YYYY-MM-DD set to an older date."
  fi
}

record() { printf '%s\n' "$1" >>"$EVIDENCE_FILE"; }

echo "spike-0007 run ($LABEL): org=$GH_ORG day=$DAY"
record "===== $LABEL run: $(date -u +%Y-%m-%dT%H:%M:%SZ) org=$GH_ORG day=$DAY ====="

INSTALL_TOKEN=$(mint_install_token)

# Step 3: the report endpoint. Ticket's other endpoints follow the same shape;
# organization-1-day is sufficient to settle the token + policy questions.
probe "/orgs/$GH_ORG/copilot/metrics/reports/organization-1-day?day=$DAY"
probe "/orgs/$GH_ORG/copilot/metrics/reports/users-1-day?day=$DAY"

echo
echo "Evidence appended to: $EVIDENCE_FILE"
echo "Next manual steps:"
echo "  1. Enable the org policy toggle (if the run says so), re-run: ./spike-0007-run.sh --after-policy"
echo "  2. Remove 'Members: Read' from the App, re-run: ./spike-0007-run.sh --no-members"
echo "  3. Re-add 'Members: Read', run once more to confirm 200 + download host."
echo "  4. DELETE the throwaway App (github.com/settings/apps) — required AC."
echo "  5. Post this evidence on #5 and close #7."
