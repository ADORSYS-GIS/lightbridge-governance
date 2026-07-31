# Revoke an integration token

**When:** a token has leaked, an agent is being retired, or an integration is being reissued.

## 1. Revoke

```bash
governance-ctl integration revoke --id <integration-id> --reason "<why>"
```

This flips `status` to `revoked`. `/internal/v1/resolve` reads it on the next lookup, so the
decision is authoritative and server-side -- nothing has to be changed on the agent.

## 2. Know how long it takes

**Up to the resolve cache TTL, currently 60 seconds** (`resolveCache.ttlSeconds`). The
result is cached in-process because it sits in Authorino's ext_authz hot path (ADR-0006).

So "revoked tokens stop working" means *within a minute*, not instantly. If a leak needs
to be closed faster than that, restart the API to drop the cache:

```bash
kubectl -n governance rollout restart deploy/lightbridge-governance
```

⚠️ On this platform ArgoCD selfHeal reverts `rollout restart` for chart-managed workloads.
Confirm the pods actually cycled rather than assuming:

```bash
kubectl -n governance get pods -l app.kubernetes.io/name=lightbridge-governance -w
```

## 3. Confirm it is closed

Do not infer this from the absence of traffic -- absence of traffic is also what a working
agent that is simply idle looks like.

```bash
curl -si https://otel.ai.camer.digital/v1/traces \
  -H "Authorization: Bearer <the revoked token>" \
  -H 'Content-Type: application/json' -d '{}' | head -1
```

Expect `HTTP/2 401`. Anything else means the revocation has not taken effect.

## 4. Reissue, if that is the intent

```bash
governance-ctl integration create --application <name> --provider microsoft-foundry
```

Then follow [onboard-a-foundry-integration.md](./onboard-a-foundry-integration.md) from §2.
⚠️ The agent owner must **publish a new agent version** for the new token to take effect --
a Foundry constraint. Budget for that before revoking a token that is in active use.

## 5. Data already ingested

Revocation stops ingestion. It does **not** delete what was already stored. If the leak
requires removing that data too, that is a retention/deletion action against both Postgres
and the S3 raw archive, and it should be recorded -- the raw archive is deliberately
immutable, so deleting from it is a deliberate, logged act, not a cleanup.
