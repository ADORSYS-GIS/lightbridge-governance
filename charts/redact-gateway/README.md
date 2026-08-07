# redact-gateway (chart)

Renders the [`redact-gateway`](../../app/redact-gateway) front proxy's `Deployment` and
`Service` through
[`bjw-s/app-template`](https://github.com/bjw-s-labs/helm-charts/tree/main/charts/library/common)
v4 (see `values.yaml`'s `app-template` key), plus a `ServiceMonitor`, `ExternalSecret` for
the hash salt, an internal-CA `Certificate` (cert-manager) for the upstream TLS connection,
and a `CiliumNetworkPolicy` that states exactly what this proxy is allowed to reach, as
local templates (`templates/`).

## Values-repo-first

Same convention as `lightbridge-governance`'s chart: `values.yaml` is structural defaults
only. Deployed values live in `ai-helm-values` (`environments/<env>/values/redact-gateway.yaml`)
and must exist there before the ai-helm change merges, or `ignoreMissingValueFiles` silently
falls back to these defaults.

## The `internalCaTrust` Certificate is a trust bundle, not a client cert

`templates/certificate.yaml` issues a throwaway leaf from the `self-signed-ca`
`ClusterIssuer` and uses only its `ca.crt` (the Home Root CA) — never the leaf's own
`tls.crt`/`tls.key`. The point is to **trust** that CA when connecting to
`core-gateway-internal`, not to **present** a client certificate. Don't repurpose the leaf
material for client auth; that's not what it's for.

## The `CiliumNetworkPolicy` selectors were read off live state, not guessed

Every `toEndpoints` selector in `templates/ciliumnetworkpolicy.yaml` was confirmed against
`kubectl -n <ns> get svc <name> -o jsonpath='{.spec.selector}'` before being written — an
earlier draft used a selector that matched nothing (the internal listener is inside the
*shared* core-gateway Envoy Deployment, not a pod of its own). A wrong selector here fails
**open** on egress (Cilium simply never matches the rule), not loudly, so re-verify against
live state before editing rather than trusting the label name alone.

The kubelet-probe carve-out (`fromEntities: [host, remote-node, health]`) exists because any
`CiliumNetworkPolicy` with ingress rules makes the selected pods deny-by-default for
ingress — without it, every readiness probe fails and the pod never goes `Ready`, which
reads exactly like a crash loop rather than a network-policy gap.

## Never `optional: true` on the salt's `secretKeyRef`

Same rule as the sibling chart: an optional ref lets a pod that beats ESO capture an empty
salt and hash with it forever (the incident this rule is named after in `values.yaml`).

The `REDACT_HASH_SALT` `secretKeyRef` under `app-template.controllers.main` in
`values.yaml` omits `optional` rather than setting `optional: false`: app-template v4's own
`values.schema.json` rejects an `optional` key on `secretKeyRef` outright. Omitting it is
functionally identical, since Kubernetes defaults `optional` to `false` (required) when it's
absent — see the sibling chart's README for the full explanation.

## Rendering locally

```bash
helm lint charts/redact-gateway
helm template charts/redact-gateway
```
