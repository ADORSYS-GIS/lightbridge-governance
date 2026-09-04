# lightbridge-governance (chart)

Renders the API `Deployment`, the `copilot-sync` `CronJob` and their `Service` through
[`bjw-s/app-template`](https://github.com/bjw-s-labs/helm-charts/tree/main/charts/library/common)
v4 (see `values.yaml`'s `app-template` key), plus a `ServiceMonitor`, a `CiliumNetworkPolicy`
and an `ExternalSecret` as local templates (`templates/`) for the
[`lightbridge-governance`](../../app/lightbridge-governance) API server and the
[`governance-ctl`](../../app/governance-ctl) collector CLI — one image, two binaries
(`.docker/Dockerfile`), two workloads.

## Values-repo-first

`values.yaml` holds structural defaults only. Deployed values live in the private
`ai-helm-values` repo (`environments/<env>/values/lightbridge-governance.yaml`). **That
file must exist on `ai-helm-values@main` before the ai-helm change merges** — otherwise
`ignoreMissingValueFiles` silently falls back to this chart's defaults instead of erroring.

## Rendering locally

```bash
helm lint charts/lightbridge-governance
helm template charts/lightbridge-governance
```

Both conditionals are independently toggleable and verified to actually suppress their
resource:

```bash
helm template charts/lightbridge-governance --set serviceMonitor.enabled=false
helm template charts/lightbridge-governance --set externalSecret.enabled=false
```

## The `ExternalSecret` property names are an assumption, not a verified fact

`externalSecret.dbPasswordProperty` (`governance_db_password`) and
`externalSecret.internalResolveTokenProperty` (`governance_internal_resolve_token`) are this
chart's best guess at what these properties are named inside the shared
`ai/camer/digital/prod/env` secret store entry. Unlike `redact-gateway`'s `saltProperty`
(verified against the live entry when that chart was written), these have **not** been
confirmed against the real store. Check before relying on this in a real environment.

Both are plain random opaque values (`openssl rand -hex 32` or similar) — there is
nothing to compute or derive for either of them.

## `DATABASE_URL` is assembled from parts, not a single opaque secret

`.Values.global.databaseUrl` is a plain (non-secret) value set per-environment in
`ai-helm-values`, e.g. `postgresql://governance:$(DB_PASSWORD)@<cnpg-rw-service>:5432/governance`.

It lives under `global`, not a plain top-level key: `app-template`'s env-string templating
runs against that subchart's own scoped `.Values` (its own keys plus `global`, per Helm's
usual subchart isolation), so a plain top-level value would render as empty inside the
container regardless of what a deployed override set it to — `global.*` is the one path
Helm actually propagates into every subchart unchanged. **The deployed value in
`ai-helm-values` must match this path** (`global.databaseUrl`, not a bare `databaseUrl`).

`DB_PASSWORD` is a real env var (sourced from `dbPasswordProperty` via `secretKeyRef`,
declared with `dependsOn: DB_PASSWORD` on the `DATABASE_URL` entry in
`app-template.controllers.api.containers.main.env` and
`app-template.controllers.copilot-sync.containers.main.env` so it renders first — this env
block is a YAML map, and app-template does not otherwise preserve key order) — Kubernetes'
native `$(VAR_NAME)` substitution resolves it into the container's actual process
environment at start, and is **not** visible in `kubectl get pod -o yaml` (the API object
keeps the literal `$(DB_PASSWORD)` string; only the kubelet-constructed process env has the
real value). `$(VAR_NAME)` expansion only applies to a plain `value:` env entry, never to
one sourced via `envFrom` or `valueFrom` — which is why `DATABASE_URL` has to be a literal
templated value here rather than routed through a ConfigMap the way TENANT_ID/GH_ORG/etc.
are (see `templates/configmap-copilot.yaml`). Same substitution technique
`lightbridge-code-intelligence`'s `DATABASE_URL` already uses in `ai-helm-values`. Nothing
needs manual assembly — the host/port/database name are known, non-secret literals, and the
password is whatever's synced to `dbPasswordProperty` (the **same**
`ai/camer/digital/prod/env` property ai-helm's `charts/lightbridge-db` reads for the
`governance` CNPG role's own password Secret, so the two can never drift out of sync — an
operator generates that one password once, and both sides pick it up via ESO).

## Never `optional: true` on a `secretKeyRef`

Env vars bind once at pod start and never refresh. An optional ref lets a pod that beats ESO
to readiness capture an **empty** credential and run with it — the pod that already tried
this the wrong way just fails auth forever instead of loudly refusing to start.

Every `secretKeyRef` under `app-template.controllers.*.containers.main.env` in
`values.yaml` omits `optional` rather than setting `optional: false`: app-template v4's own
`values.schema.json` rejects an `optional` key on `secretKeyRef` outright (`helm template`
fails with `Additional property optional is not allowed`). Omitting it is functionally
identical — Kubernetes defaults `optional` to `false` (required) when it's absent — but if
you're adding a fourth `secretKeyRef`, leave the field out; don't add `optional: false` and
break `helm lint`.

## `copilotOtel`: a dedicated OpenTelemetryCollector, and what's unverified about it

`copilotOtel.enabled` (default `true`) renders an `opentelemetry.io/v1beta1` `OpenTelemetryCollector`
CR (`templates/otelcollector.yaml`) that turns copilot-sync's one-shot OTLP push into something
Alloy can scrape — ADR-0007 is explicit that "a CronJob pod that exits cannot be scraped", so this
collector receives the push on `:4317` and re-exposes it via a prometheus exporter that a
`ServiceMonitor` (`templates/servicemonitor-copilot-otel.yaml`) points Alloy at. It requires the
OpenTelemetry Operator on the cluster; this chart cannot verify that at render time.

Two things here are **not** verified against a live cluster (neither this session nor the one that
wrote this chart has one):

- **The generated Service's name and labels.** The `ServiceMonitor`'s selector was built by reading
  the OpenTelemetry Operator's own source (`internal/naming`, `internal/manifests/collector/service.go`,
  `internal/manifests/manifestutils/labels.go`) rather than guessed — see the long comment at the top
  of `templates/servicemonitor-copilot-otel.yaml` for the exact functions read and the reasoning.
  Confidence is high (this is source, not a blog post, and the naming scheme has been stable in the
  operator for a long time) but not confirmed against whichever operator version is actually
  installed here.
- **`readOnlyRootFilesystem: true` / `runAsNonRoot: true` on the collector pod**
  (`templates/otelcollector.yaml`) assume the `otlp receiver -> resource processor -> prometheus
  exporter` pipeline needs no writable path and that the upstream `otel/opentelemetry-collector`
  image bakes in a non-root user. Confirm against a real pod before relying on it.

Toggle it off and the collector, its `ServiceMonitor` and its `CiliumNetworkPolicy`
(`templates/ciliumnetworkpolicy-copilot-otel.yaml`) all disappear together, and
`templates/configmap-otlp.yaml` falls back to the manual `copilot.otlp.host` override (empty by
default) instead of continuing to point at a Service that no longer exists:

```bash
helm template charts/lightbridge-governance --set copilotOtel.enabled=false
```

## `grafanaDashboard`: a `GrafanaDashboard` CR, and what's unverified about it

`grafanaDashboard.enabled` (default `true`) renders a `grafana.integreatly.org/v1beta1`
`GrafanaDashboard` CR (`templates/grafanadashboard.yaml`) embedding
`dashboards/copilot-connector.json` — the Grafana Operator's own resource for shipping a
dashboard, matching ADR-0003's precedent (ai-helm ADR-0063's read-only Postgres
`GrafanaDatasource`) rather than a sidecar-labelled ConfigMap.

Two datasource UIDs (`grafanaDashboard.datasources.postgresUid`/`prometheusUid`) are
substituted into the dashboard JSON's `__DS_POSTGRES__`/`__DS_PROMETHEUS__` placeholder
tokens at `helm template` time — see that template's header comment for why this was
chosen over the operator's own `spec.datasources` mapping field, and for the two distinct
"wrong value" vs. "wrong token" failure modes. The template `fail`s the render (rather
than shipping silently) if either contracted token, or anything shaped like an
unrecognized `__DS_..._..__` token, survives substitution — both guards were verified by
deliberately breaking the substitution and confirming the render fails for the reason
predicted, then restoring it.

Two things here are **not** verified against a live cluster:

- **The `apiVersion`/`kind`/`spec.json` shape.** Confirmed via context7 (`/grafana/grafana-operator`)
  against the operator's own docs and its `grafanadashboards` CRD directly — high confidence on the
  schema, not confirmed against whichever operator version is actually installed here.
- **`instanceSelector` (`dashboards: "grafana"` by default).** Must match the labels on the
  deployed `Grafana` CR, which lives in ai-helm and which this chart cannot see. Wrong labels
  don't error — the CR applies cleanly and the operator simply never selects it.
- **`grafanaDashboard.datasources.prometheusUid` (`mimir` by default).** Not recorded anywhere
  in this repo, unlike the Postgres UID (`governance`, pinned by ADR-0003). A wrong value doesn't
  fail to render either — every Prometheus-backed panel silently shows "no data", indistinguishable
  from a real outage.

Toggle it off and the resource disappears cleanly:

```bash
helm template charts/lightbridge-governance --set grafanaDashboard.enabled=false
```

`dashboards/copilot-connector.json` in this chart is currently a **placeholder** (a minimal
valid dashboard exercising both substitution tokens) written so this template has something
real to render and verify against — it must be overwritten with the actual dashboard.

## `/metrics`, `/livez`, `/readyz` are real but `/metrics` is empty

The API server's `/metrics` endpoint exists (added alongside this chart, since a
`ServiceMonitor` pointing at a 404 is worse than no `ServiceMonitor`) but has no counters
registered yet — deriving `governance_connector_*` (ADR-0007) is separate, not-yet-done
work. `/livez`/`/readyz` are static `200 ok` and don't touch the database.
