#!/usr/bin/env bash
# Live test for redact-gateway. Requires REDACT_API_KEY exported.
#
# Single:   ./scripts/test-redact-live.sh
# Full:     REDACT_TEST=1 ./scripts/test-redact-live.sh
# Concurrent: REPEAT=5 ./scripts/test-redact-live.sh
#
# just-level (requires API key in env):
#   just redact-test-live             # single clean request
#   REDACT_TEST=1 just redact-test-live # full suite

set -euo pipefail

GATEWAY_URL="${REDACT_GATEWAY_URL:-http://localhost:8080}"
API_KEY="${REDACT_API_KEY:-}"
REPEAT="${REPEAT:-1}"
TEST_MODE="${REDACT_TEST:-}"
MODEL="${REDACT_MODEL:-llama-3.1-8b-instant}"
CHAT="$GATEWAY_URL/v1/chat/completions"

RED=$'\033[0;31m'
GRN=$'\033[0;32m'
YLW=$'\033[1;33m'
BLU=$'\033[0;34m'
NRM=$'\033[0m'

ok() { echo -e "${GRN}[PASS]${NRM} $*"; }
ng() { echo -e "${RED}[FAIL]${NRM} $*"; }
info() { echo -e "${BLU}[INFO]${NRM} $*"; }
warn() { echo -e "${YLW}[WARN]${NRM} $*"; }
hdr() {
  echo ""
  echo -e "${BLU}═══════════════════════════════════════════════════════════${NRM}"
  echo -e "${BLU}  $*${NRM}"
  echo -e "${BLU}═══════════════════════════════════════════════════════════${NRM}"
}

die() { echo -e "${RED}error: $1${NRM}" >&2; exit 1; }

# Usage: request POST/GET url body_file_or_dash
request() {
  local expect_status="${1:-200}"; shift
  local method="${1:-POST}"; shift
  local url="$1"; shift
  local body="$1"; shift
  local capture_file
  capture_file=$(mktemp)
  local http_code

  http_code=$(curl -s -o "$capture_file" -w "%{http_code}" \
    -X "$method" \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -H "Connection: close" \
    "$url" \
    ${body:+-d "$body"} \
    2>&1) || {
    ng "→ connection failed"
    rm -f "$capture_file"
    return 1
  }

  local result
  result=$(cat "$capture_file")
  rm -f "$capture_file"

  case "$http_code" in
    "$expect_status") ok "→ $http_code"; cat <<< "$result" | head -c 200;;
    *) ng "→ HTTP $http_code (expected $expect_status)"
       cat <<< "$result" | head -c 300;;
  esac
  return 0
}

# Usage: stream_req label body_json
stream_req() {
  local label="$1"; shift
  local body="$1"; shift
  local tmpf
  tmpf=$(mktemp)
  local beg end ms bytes http_code

  beg=$(date +%s%N)
  http_code=$(curl -s --no-buffer \
    -X POST "$CHAT" \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -H "Connection: close" \
    -d "$body" \
    > "$tmpf" 2>&1) || {
    ng "→ connection failed"
    rm -f "$tmpf"
    return 1
  }
  end=$(date +%s%N)
  ms=$(( end - beg ))
  bytes=$(wc -c < "$tmpf")

  if grep -q 'content_blocked' "$tmpf" 2>/dev/null; then
    ok "→ BLOCKED ${ms}ms"
  elif [ "$bytes" -gt 100 ]; then
    ok "→ STREAMED ${ms}ms (${bytes}B)"
  else
    ng "→ EMPTY/SHORT ${ms}ms (${bytes}B)"
  fi
  rm -f "$tmpf"
}

# Streaming, expect block (response path)
stream_req_block() {
  local label="$1"; shift
  local body="$1"; shift
  local tmpf
  tmpf=$(mktemp)
  local beg end ms http_code

  beg=$(date +%s%N)
  http_code=$(curl -s --no-buffer \
    -X POST "$CHAT" \
    -H "Authorization: Bearer $API_KEY" \
    -H "Content-Type: application/json" \
    -H "Connection: close" \
    -d "$body" \
    > "$tmpf" 2>&1) || {
    ng "→ connection failed"
    rm -f "$tmpf"
    return 1
  }
  end=$(date +%s%N)
  ms=$(( end - beg ))

  local bytes
  bytes=$(wc -c < "$tmpf")
  if grep -q 'content_blocked' "$tmpf" 2>/dev/null; then
    ok "→ BLOCKED mid-stream ${ms}ms"
  else
    ng "→ NOT BLOCKED ${ms}ms (${bytes}B)"
  fi
  rm -f "$tmpf"
}

metrics() { curl -sf "$GATEWAY_URL/metrics" 2>/dev/null | grep "^redact_"; }

# ── Main ────────────────────────────────────────────────────────────────────────
# Shell-check: all guards come BEFORE any logic
[ -z "$API_KEY" ] && die "REDACT_API_KEY is not set"

GW_HEALTH=$(curl -sf "$GATEWAY_URL/livez" > /dev/null 2>&1 && echo "ok" || echo "FAIL")
EX_HEALTH=$(curl -sf "http://localhost:9501/livez" > /dev/null 2>&1 && echo "ok" || echo "FAIL")

hdr "HEALTH CHECKS"
echo "  redact-gateway /livez : $GW_HEALTH"
echo "  redact-extproc  /livez : $EX_HEALTH"
[ "$GW_HEALTH" = "FAIL" ] && die "Gateway is not responding (run 'just redact-up' first)"

info "Gateway : $GATEWAY_URL"
info "Provider : $(grep PROVIDER_BASE_URL .env 2>/dev/null | grep -v "^#" | head -1)"
info "Concurrency: $REPEAT"

# ── Simple run ────────────────────────────────────────────────────────────────
if [ "$TEST_MODE" != "1" ] && [ "$REPEAT" = "1" ]; then
  hdr "SIMPLE TEST — clean streaming request"
  info "... model: $MODEL"
  beg=$(date +%s%3N)
  request 200 POST "$CHAT" "$(printf '%s\n' \
    "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Count from 1 to 3.\"}],\"stream\":true}")"
  end=$(date +%s%3N)
  info "Time: $(( end - beg ))ms"
  hdr "METRICS"
  metrics
  exit 0
fi

# ── Full suite ────────────────────────────────────────────────────────────────
hdr "FULL REDACTION TEST SUITE"

info "1. Clean streaming"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"What is a goroutine in Go?\"}],\"stream\":true}")
  stream_req "clean-stream" "$body"
}

info "2. Email in prompt (request scan → replace)"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"My email is alice@corp.io. Explain microservices.\"}]}")
  request 200 POST "$CHAT" "$body" >/dev/null
}

info "3. SSN in prompt (request scan → mask)"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"SSN 123-45-6789 fix this bug.\"}]}")
  request 200 POST "$CHAT" "$body" >/dev/null
}

info "4. API key in prompt (BLOCK — never reaches LLM)"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"here is my key sk-ant-api03-test123456789abcdefghijklmnopqrstuvwxyz\"}]}")
  request 422 POST "$CHAT" "$body" >/dev/null
}

info "5. Model echo email (response scan → replace)"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with exactly this string and nothing else: admin@internal.corp.io\"}],\"stream\":true}")
  stream_req "response-replace" "$body"
}

info "6. Model echo API key (response BLOCK — scanner fires mid-stream)"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Repeat and nothing else: gho_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7\"}],\"stream\":true}")
  stream_req_block "response-block" "$body"
}

info "7. Non-streaming clean"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"What is 2+2?\"}]}")
  request 200 POST "$CHAT" "$body" >/dev/null
}

info "8. Multi-PII: email + SSN"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Email bob@enterprise.com about SSN 888-77-6666.\"}]}")
  request 200 POST "$CHAT" "$body" >/dev/null
}

info "9. Phone number"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Call +1-800-555-1234 for support.\"}]}")
  request 200 POST "$CHAT" "$body" >/dev/null
}

info "10. Credit card"
{
  body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Card 4532015112830366 valid?\"}]}")
  request 200 POST "$CHAT" "$body" >/dev/null
}

# ── Concurrent ────────────────────────────────────────────────────────────────
[ "$REPEAT" -gt 1 ] 2>/dev/null && {
  hdr "CONCURRENT LOAD — ${REPEAT}x parallel"
  start=$(date +%s%3N)
  for _ in $(seq 1 "$REPEAT"); do
    {
      body=$(printf '%s\n' "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"Hello.\"}],\"stream\":true}")
      curl -s --no-buffer -X POST "$CHAT" \
        -H "Authorization: Bearer $API_KEY" \
        -H "Content-Type: application/json" \
        -d "$body" > /dev/null 2>&1 || true
    } &
  done
  wait
  end=$(date +%s%3N)
  ok "All ${REPEAT} requests done in $(( end - start ))ms"
}

hdr "FINAL METRICS"
metrics