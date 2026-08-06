# Deployment Verification 2026-08-06

## Issue

Production `redact-extproc` returning HTTP 500 on all requests.

## Root Cause

Commit `a3da8bf` (PR #59) fixes a state machine bug where bodyless requests (GET,
etc.) caused `processing state mismatch` errors.

The fix is on `main` (merged 2026-08-06 16:11:57 +0200).

## Action Required

The Docker image `ghcr.io/adorsys-gis/redact-extproc` must include this fix.

### Verify the deployed image

```bash
kubectl -n envoy-gateway-system get deployment core-gateway \
  -o jsonpath='{.spec.template.spec.containers[*].image}'
```

Compare against the current main image digests:

```bash
# Check what's on GHCR
ghcr ls usr ghcr.io/adorsys-gis/redact-extproc
```

### If the image is old (missing a3da8bf)

1. Trigger a rebuild by pushing any commit to main, or:
2. Manually re-tag and push:

```bash
# Fetch latest main
git fetch origin main

# Create a tag if needed to trigger the workflow
git tag deployment/verify-$(date +%Y%m%d) origin/main
git push origin deployment/verify-20260806
```

3. Wait for CI to build `ghcr.io/adorsys-gis/redact-extproc`
4. Update ai-helm-values to point to the new image SHA or tag
5. Rollout the deployment:

```bash
kubectl -n envoy-gateway-system rollout restart deployment core-gateway
```

### Verify the fix works

```bash
# Test against core-gateway with a bodyless request
curl -v https://core-gateway-internal.envoy-gateway-system.svc.cluster.local:v1/models

# Should return 200, not 500 with "processing state mismatch"
```

## Clients Must Use Envoy Gateway

The `redact-extproc` sidecar runs inside the Envoy gateway pod:

```
client -> core-gateway (Envoy + redact-extproc) -> AI provider
         NOT
client -> redact-gateway (HTTP proxy) -> core-gateway
```

Ensure clients are configured to point to `core-gateway` (port 443), not
`redact-gateway` (port 8080).