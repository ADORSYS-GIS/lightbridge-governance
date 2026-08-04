# lightbridge-governance (chart)

Renders the API `Deployment`, the `copilot-sync` `CronJob`, a `Service`, a `ServiceMonitor`,
and an `ExternalSecret` for the [`lightbridge-governance`](../../app/lightbridge-governance)
API server and the [`governance-ctl`](../../app/governance-ctl) collector CLI — one image,
two binaries (`.docker/Dockerfile`), two workloads.

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

`externalSecret.databaseUrlProperty` (`governance_database_url`) and
`externalSecret.internalResolveTokenProperty` (`governance_internal_resolve_token`) are this
chart's best guess at what the two properties are named inside the shared
`ai/camer/digital/prod/env` secret store entry. Unlike `redact-gateway`'s `saltProperty`
(verified against the live entry when that chart was written), these have **not** been
confirmed against the real store. Check before relying on this in a real environment.

## Never `optional: true` on a `secretKeyRef`

Env vars bind once at pod start and never refresh. An optional ref lets a pod that beats ESO
to readiness capture an **empty** credential and run with it — the pod that already tried
this the wrong way just fails auth forever instead of loudly refusing to start. Both
`secretKeyRef`s in `templates/deployment.yaml` and `templates/cronjob.yaml` set
`optional: false` explicitly; if you're adding a third, do the same.

## `/metrics`, `/livez`, `/readyz` are real but `/metrics` is empty

The API server's `/metrics` endpoint exists (added alongside this chart, since a
`ServiceMonitor` pointing at a 404 is worse than no `ServiceMonitor`) but has no counters
registered yet — deriving `governance_connector_*` (ADR-0007) is separate, not-yet-done
work. `/livez`/`/readyz` are static `200 ok` and don't touch the database.
