# Deployment Verification 2026-08-06

## Issue

Production `redact-extproc` returning HTTP 500 on all requests.

## Root Causes

### 1. Bodyless GET state machine bug (PR #59 — `a3da8bf`)

Bodyless requests (GET, etc.) caused `processing state mismatch` errors because
the phase machine never advanced past `RequestBody` when no `RequestBody` gRPC
message arrived.

**Fix**: The `ResponseHeaders` handler advances the phase from `RequestBody` to
`ResponseBody` when it arrives without a preceding `RequestBody`.

### 2. Content-Length stripping on body mutation (PR #74 — `cd18c5b`)

When `redact-extproc` redacts PII/credentials in a request body, the body length
changes (e.g. `jane@example.com` → `<REDACTED>`, 92→86 bytes). The ext_proc
service sends a `BodyMutation` with the new bytes and a `HeaderMutation` to
update `Content-Length` to match.

**Envoy v1.32's ext_proc filter strips `Content-Length` to an empty string after
any body mutation**, regardless of what header mutation the processor sends. The
`allow_content_length_header` config field that preserves it was added in Envoy
v1.33+ and does not exist in v1.32.

The empty `Content-Length` reaches the upstream and causes:
- **HTTP/2 upstreams**: `RST_STREAM` with `PROTOCOL_ERROR` (Go h2c servers detect
  the mismatch between `Content-Length: ""` and DATA frame bytes)
- **HTTP/1.1 upstreams**: body misframing (`Content-Length: 0` but N bytes follow)

The upstream returns an error, ext_proc sees a non-JSON error response, fails
closed, and returns 502/500.

**Fix**: Remove `Content-Length` via `remove_headers` instead of trying to
overwrite it. This lets Envoy frame the mutated body correctly — chunked
transfer encoding over HTTP/1.1, DATA frames over HTTP/2 — neither of which
requires `Content-Length`.

### 3. Response header value safety (PR #78 — diagnostic)

The `HeaderValue` protobuf can carry the value in either `value` (string tag 2)
or `raw_value` (bytes tag 3). The ext_proc now reads whichever is non-empty,
ensuring SSE detection (`set_mode_from_headers`) works regardless of which
field Envoy populates.

## Status

| Fix | PR | Commit | Status |
|-----|----|--------|--------|
| Bodyless GET phase fix | #59 | `a3da8bf` | ✅ Merged 2026-08-06 |
| Content-Length removal fix | #74 | `cd18c5b` | ✅ Merged 2026-08-06 |
| Response header value safety | #78 | `pending` | ⏳ Open |

| Environment | Content-Length Fix | Streaming (SSE) | Action Required |
|------------|-------------------|-----------------|-----------------|
| **Local (compose)** | ✅ | ✅ Verified 200 | Done |
| **Local (k3d raw Envoy)** | ✅ | ✅ Verified 200 | Done |
| **Local (k3d EG + AI Gateway)** | ✅ | ✅ Verified 200 | Done |
| **Production** | ❓ Pending deploy | ❓ Pending deploy | Enable + deploy |

## Local Verification (Completed)

### Raw Envoy + ext_proc sidecar (k3d)
All scenarios pass with the Content-Length removal fix:

| Request | Before fix | After fix |
|---|---|---|
| GET /v1/models (bodyless) | 200 | 200 |
| POST with no PII | 200 | 200 |
| POST with PII (body 92→86 bytes) | **502** | **200** |
| POST with credential (blocked) | 422 | 422 |
| POST with stream=true + PII | **502** | **200** |
| POST with PII in response | **502** | **200** (redacted) |

### Envoy Gateway v1.8.3 + AI Gateway v1.0.0 (k3d)

Production replica with:
- `EnvoyExtensionPolicy` with ext_proc (processing: request Buffered, response Streamed)
- `Backend` resource pointing to `127.0.0.1:9500` (same-pod loopback)
- Cross-namespace `ReferenceGrant` from `converse-gateway` to `envoy-gateway-system`
- `ResponseHeaderMode=SEND` confirmed in generated xDS config
- Header values correctly populated (both `value` and `raw_value`)

## Action Required for Production

### 1. Verify the deployed image

```bash
# Check current cluster namespace
kubectl get namespaces | grep -E "envoy|gateway|ai"

# Find the core-gateway deployment
kubectl get deployments -A | grep core-gateway

# Get the image SHA
kubectl get deployment -n <namespace> core-gateway \
  -o jsonpath='{.spec.template.spec.containers[*].image}'
```

The image must include commit `cd18c5b` or later (PR #74 — Content-Length removal fix)

### 2. Configure ai-helm-values

```yaml
# environments/prod/values/core-gateway.yaml
redactExtproc:
  enabled: true
  image:
    repository: ghcr.io/adorsys-gis/redact-extproc
    tag: "sha-<commit-with-fix>"
```

### 3. Verify the fix works

```bash
# 1. Bodyless request (GET) — must NOT return "processing state mismatch" 500
curl -v https://core-gateway-internal.envoy-gateway-system.svc.cluster.local/v1/models

# 2. POST with PII (email) — must return 200 with email redacted to <REDACTED>
#    Before the fix this returned 502 because Envoy v1.32 stripped Content-Length
#    to an empty string after body mutation, causing the upstream to RST_STREAM.
curl -v https://core-gateway-internal.envoy-gateway-system.svc.cluster.local/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"mock","messages":[{"role":"user","content":"my email is jane@example.com"}]}'

# 3. POST with credential — must return 422 (blocked)
curl -v https://core-gateway-internal.envoy-gateway-system.svc.cluster.local/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"mock","messages":[{"role":"user","content":"token ghp_abcdefghijklmnopqrstuvwxyz0123456789"}]}'

# 4. POST with streaming + PII — must return 200 with email redacted
curl -v https://core-gateway-internal.envoy-gateway-system.svc.cluster.local/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"mock","stream":true,"messages":[{"role":"user","content":"my email is test@example.com"}]}'
```

## Architecture Context

Two deployment shapes share the same `governance-redact` engine:

| Component | Shape | Port | How to test |
|-----------|-------|------|-------------|
| `redact-gateway` | HTTP front proxy | 8080 | `curl localhost:8080/v1/chat/completions` |
| `redact-extproc` | Envoy ext_proc sidecar | 9500 (loopback) | Via Envoy gateway only |

### Data Flow

**Testing locally (docker compose):**
```
client -> redact-gateway:8080 -> upstream AI provider
```

**Production (Kubernetes + Envoy gateway):**
```
client -> core-gateway (Envoy) -> redact-extproc sidecar -> AI provider
                  NOT
client -> redact-gateway:8080 -> core-gateway
```

### Production Kubernetes Resources

The production setup uses Envoy Gateway + AI Gateway:

```
converse-gateway namespace:
  Gateway: core-gateway                     -- ingress point
  EnvoyExtensionPolicy:                     -- Lua + ext_proc in one resource
    - Lua: stamps x-billing-period headers
    - extProc -> Backend: redact-extproc (127.0.0.1:9500)
      processingMode: request.body=Buffered, response.body=Streamed
      messageTimeout: 200ms
      failOpen: false

envoy-gateway-system namespace:
  Backend: core-gateway-redact-extproc      -- static endpoint 127.0.0.1:9500
  EnvoyProxy: core-gateway-proxy            -- adds redact-extproc sidecar container
  ReferenceGrant: allow-backend-ref         -- permits cross-ns Backend reference
```

## Key Notes

- `redact-extproc` runs as a **sidecar inside the gateway pod** (loopback only)
- Clients must connect to the **Envoy gateway** (`core-gateway:443`), not
  `redact-gateway:8080` unless intentionally testing the front-proxy path
- The `redact-gateway` chart (`charts/redact-gateway/`) is for testing/canary,
  not the production integration path
- The `messageTimeout: 200ms` is tight but sufficient for regex-based redaction
- `failOpen: false` is an invariant — if the redactor can't process, the request
  must fail, not silently forward unscanned content