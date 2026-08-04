# lightbridge-governance

Governance for AI usage across providers: who is using what, what it costs, which seats are
idle, and whether anything sensitive is leaving the building. Two roughly independent
systems live in this one workspace ([ADR-0005](docs/adr/0005-one-workspace-registry-first-connectors-as-crates.md)):

1. **The registry and its connectors** — tenants, applications, environments, integrations,
   and the two connectors that write usage data into them.
2. **The redaction subsystem** — scans AI request/response traffic for secrets and PII
   before it reaches or leaves the platform. Independent of the registry; neither depends
   on the other being deployed.

Two connectors, once built:

| Connector | Shape | Source |
|---|---|---|
| **GitHub Copilot** | pull — polls GitHub's daily aggregated reports | [RFC-0001](docs/rfc/0001-github-copilot-connector.md) |
| **Microsoft Foundry** | push — authenticated OTLP from hosted agents | [RFC-0002](docs/rfc/0002-microsoft-foundry-otlp-ingestion.md) |

Both normalize into one provider-agnostic model in Postgres. **The dashboards are the
product** — Grafana reads that database directly ([ADR-0003](docs/adr/0003-grafana-reads-postgres-directly.md)).

## Layout

Every directory below has its own README with the decisions and gotchas specific to it —
this file only orients; follow the links for the details.

**Registry and connectors:**

| Path | What's there |
|---|---|
| [`crates/governance-core`](crates/governance-core/README.md) | Registry, credentials, normalized model, money |
| [`crates/governance-copilot`](crates/governance-copilot/README.md) | The pull connector — scaffold, blocked on [#7](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/7) |
| [`crates/governance-foundry`](crates/governance-foundry/README.md) | The push connector — scaffold, blocked on [#8](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/8) |
| [`app/lightbridge-governance`](app/lightbridge-governance/README.md) | API server (bin): registry routes, `/internal/v1/resolve`, `/metrics` |
| [`app/governance-ctl`](app/governance-ctl/README.md) | Collector CLI (bin): `sync`, `replay`, `verify`, `status`, `migrate` |
| `crates/governance-core/schema/governance.cstack` | The schema — tables, migrations, CRUD, routes |
| [`charts/lightbridge-governance`](charts/lightbridge-governance/README.md) | `Deployment`, `copilot-sync` `CronJob`, `ServiceMonitor` |

**Redaction (independent subsystem — neither side depends on the registry):**

| Path | What's there |
|---|---|
| [`crates/governance-redact`](crates/governance-redact/README.md) | The policy engine: detection, profiles, holdback |
| [`app/redact-gateway`](app/redact-gateway/README.md) | Front-proxy binary wrapping it (bin) |
| [`app/redact-extproc`](app/redact-extproc/README.md) | Envoy `ext_proc` sidecar binary wrapping it (bin) |
| [`charts/redact-gateway`](charts/redact-gateway/README.md) | `Deployment`, `Certificate`, `CiliumNetworkPolicy` |

**Docs:** [`docs/{adr,rfc,runbooks,spikes}/`](docs/README.md) — why, what, what-to-do-at-3am,
and time-boxed findings.

## Getting started

```bash
just up          # local Postgres
just migrate
just all-checks  # fmt + clippy -D warnings + check + test
```

## Testing the redaction module

`redact-gateway` and `redact-extproc` both wrap `governance-redact` (the redaction
engine) and hold no credential of their own — the caller's `Authorization` header is
forwarded upstream untouched (see `app/redact-gateway/src/main.rs` module docs). That
means testing them for real means pointing at a **real LLM**, not a stub: a mock
upstream can confirm the proxy's plumbing but proves nothing about redaction against
actual model output.

`redact-gateway` is the one to test directly — it's a normal HTTP proxy you can curl.
`redact-extproc` is an Envoy `ext_proc` sidecar (ADR-0116 in `app/redact-extproc`'s doc
comments); Envoy itself calls the upstream, so exercising it end-to-end needs an Envoy
instance in front of it, which this compose stack does not set up.

```bash
cp .env.example .env
# edit .env: set PROVIDER_BASE_URL to a real OpenAI-compatible provider's root
# (OpenAI, Groq, Together, ...) — see .env.example for the exact list and shape.

just redact-build   # build the images (one-time)
just redact-up      # start redact-gateway (+ redact-extproc) against .env
just redact-test    # verify both are healthy
```

**Services:**

| Service | Port | Purpose |
|---------|------|---------|
| `redact-gateway` | 8080 | redaction proxy — test this directly |
| `redact-extproc` | 9500/9501 | gRPC ext_proc sidecar — needs Envoy in front to exercise |

**Testing the proxy** — the gateway forwards the caller's own `Authorization` header,
so supply a real API key for whatever provider `PROVIDER_BASE_URL` points at:

```bash
# Clean prompt — passes through, forwarded to the real model, response scanned
curl -s http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"llama-3.1-8b-instant","messages":[{"role":"user","content":"Say hello in one sentence."}]}'

# PII in the request — coding-assistant profile replaces/masks it before it
# ever reaches the model; inspect the outbound body via RUST_LOG=debug if needed
curl -s http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"llama-3.1-8b-instant","messages":[{"role":"user","content":"My email is john.smith@example.com, summarize: hello"}]}'

# A leaked credential in the prompt — blocked outright, never forwarded (Action::Block)
curl -s http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $YOUR_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"llama-3.1-8b-instant","messages":[{"role":"user","content":"here is my key sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}'
# -> 422, {"error":{"type":"content_blocked", ...}}

# Health checks
curl http://localhost:8080/livez    # gateway
curl http://localhost:9501/livez    # extproc (metrics/health side only)
curl http://localhost:8080/metrics  # Prometheus counters: redact_redactions_total, redact_blocked_total, ...

# Tear down
just redact-down
```

## Where things are decided

Start at [`docs/adr/README.md`](docs/adr/README.md). The load-bearing ones:

- [ADR-0001](docs/adr/0001-single-tenant-deployable-not-saas.md) — single-tenant deployable, not SaaS. A customer runs their own install.
- [ADR-0002](docs/adr/0002-postgres-is-the-system-of-record-not-parquet-on-s3.md) — Postgres is the system of record; S3 is the raw archive.
- [ADR-0004](docs/adr/0004-observability-stack-stays-single-tenant.md) — the LGTM stack stays single-tenant; the database is the isolation boundary.
- [ADR-0006](docs/adr/0006-foundry-auth-reuses-core-gateway-and-authorino.md) — reuse core-gateway + Authorino; build no auth service.
- [ADR-0009](docs/adr/0009-cratestack-only-rest-transport-cbor-payloads.md) — cratestack is the only persistence layer; REST transport, CBOR payloads.

## Status

- **Registry**: built and deployable — schema, migrations, credential issuance/revocation,
  `/internal/v1/resolve`, and a real Helm chart all exist and are tested.
- **Redaction**: built and deployable — `governance-redact` plus both binaries that wrap it
  (`redact-gateway`, `redact-extproc`) exist and are tested; `redact-extproc` deploys as a
  sidecar in the gateway pod, not from a chart in this repo.
- **Copilot connector**: scaffold only. [#7](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/7)'s
  spike confirmed GitHub App installation tokens work against the report endpoints, but
  surfaced a live blocker — the org's "Copilot usage metrics" policy is currently disabled —
  and the empirical run with a real App hasn't landed yet. [#12](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/12)
  is next once that's resolved.
- **Foundry connector**: scaffold only, blocked on [#8](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/8)'s
  tenancy spike.

## Licence

Apache-2.0.
