# redact-gateway

An OpenAI-compatible redaction proxy. Sits in front of the AI gateway and scans prompts on
the way out, model output on the way back:

```text
client -> redact-gateway -> core-gateway-internal (Authorino -> AI Gateway -> provider)
```

A front proxy, not an Envoy `ext_proc` filter — see ai-helm ADR-0113 for why (no
filter-chain ordering dependency, no fork of the AI Gateway). `redact-extproc`
([sibling app](../redact-extproc)) is the newer `ext_proc` approach, per ADR-0116;
the two are not redundant by accident — see that app's README for how they differ.

## This service authenticates nobody

The caller's `Authorization` header is forwarded upstream untouched; the upstream gateway
authenticates exactly as it would without this proxy in the path. This binary holds no
credential of its own, so compromising it yields no token. Trust is network-level: ClusterIP
plus a `CiliumNetworkPolicy` (see `charts/redact-gateway/templates/ciliumnetworkpolicy.yaml`).

## Routes

| Route | Auth |
|---|---|
| `/livez`, `/readyz`, `/metrics` | none — deliberately outside the proxy path, so an orchestrator can probe a service that's otherwise refusing traffic |
| `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings` | forwarded, unauthenticated by this proxy |

## Configuration

All of `LISTEN_ADDR`, `PROVIDER_BASE_URL`, `REDACT_PROFILE`, `REDACT_HASH_SALT`,
`MAX_BODY_BYTES` are required or defaulted — see [`config.rs`](src/config.rs) for the full
list, including `PROVIDER_CA_FILE` (a mounted PEM, not `SSL_CERT_FILE` — this binary's HTTP
client is rustls, which doesn't read that OpenSSL-only variable; the predecessor service did
and setting it here would fail at connect time on the first request).

⚠️ `REDACT_HASH_SALT` must be stable for the deployment's lifetime and secret — see
[`config.rs`](src/config.rs)'s doc comment for why a rotating or guessable salt defeats the
hash action entirely.

## `MAX_BODY_BYTES` is a memory ceiling, not a nicety

Detection buffers the whole body (that's what stops an entity hiding across a chunk split),
so the memory cost is by design. Without a cap, an upstream that streams without stopping
OOM-kills the pod.

## Deploying

`charts/redact-gateway` renders this as a `Deployment` with an internal-CA `Certificate`
(cert-manager), an `ExternalSecret` for the hash salt, and a `CiliumNetworkPolicy` narrowed
to a specific caller namespace. See that chart's README for the values contract.
