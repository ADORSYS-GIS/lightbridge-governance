# Deployment Verification 2026-08-06

## Issue

Production `redact-extproc` returning HTTP 500 on all requests.

## Root Cause

Commit `a3da8bf` (PR #59) fixes a state machine bug where bodyless requests (GET,
etc.) caused `processing state mismatch` errors.

The fix is on `main` (merged 2026-08-06 16:11:57 +0200).

## Status

| Environment | Image Status | Action Required |
|------------|--------------|-----------------|
| **Local (compose)** | ✅ Rebuilt 2026-08-06 | Done |
| **Production** | ❓ Unknown | Deploy latest image |

## Local Verification (Completed)

```bash
# Services are running with latest code:
docker compose -p lightbridge-governance --profile redact ps
# Output: redact-gateway and redact-extproc Up with latest images

# Health checks pass:
just redact-test
# Output: /livez OK for both services
```

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

### 2. Trigger image rebuild if needed

The CI workflow builds on push to main. Check if images include commit `a3da8bf`:

```bash
# List images on GHCR
# - ghcr.io/adorsys-gis/redact-gateway
# - ghcr.io/adorsys-gis/redact-extproc
# - ghcr.io/adorsys-gis/lightbridge-governance
```

To trigger a rebuild:
```bash
# Push a tag or any commit
git tag deployment/verify-20260806
git push origin deployment/verify-20260806
```

### 3. Rollout latest

```bash
# Restart the core-gateway deployment
kubectl -n <envoy-namespace> rollout restart deployment core-gateway

# Watch rollout
kubectl -n <envoy-namespace> rollout status deployment core-gateway
```

### 4. Verify the fix works

```bash
# Test against core-gateway with a bodyless request (GET)
curl -v https://core-gateway-internal.envoy-gateway-system.svc.cluster.local/v1/models

# Should NOT return "processing state mismatch" 500
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

## Key Notes

- `redact-extproc` runs as a **sidecar inside the gateway pod** (loopback only)
- Clients must connect to the **Envoy gateway** (`core-gateway:443`), not
  `redact-gateway:8080` unless intentionally testing the front-proxy path
- The `redact-gateway` chart (`charts/redact-gateway/`) is for testing/canary,
  not the production integration path