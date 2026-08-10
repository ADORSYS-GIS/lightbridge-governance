# ADR-0011: Bridge copilot-sync's run-detail metrics from push to pull with a dedicated collector

- Status: Proposed
- Date: 2026-08-07
- Decision owners: @stephane-segning

## Context

ADR-0007 settled how `governance_connector_*` -- the ~10 low-cardinality operational
health gauges (is this connector synced, how stale, how many scrape errors) -- gets to
Mimir: the collector writes every run outcome to `ingest_manifests`, and the always-on
API derives those gauges from that table on `/metrics`. ADR-0007 states plainly why the
CronJob itself can't be the source: *"a CronJob pod that exits cannot be scraped."*

That answer covers exactly what is reconstructible from `ingest_manifests` -- a
provider's existence and freshness. It cannot express what only exists at run time and
is never written to that table: how many reports a given run fetched and at what
per-report outcome, how many normalized rows it upserted, whether `verify` found
manifest drift, how many users are currently unmapped. `governance-ctl`
(`app/governance-ctl/src/metrics.rs`) already computes all of this in-process and
attempts an OTLP push of it before the pod exits; today that push has nowhere
in-cluster to land (`copilot.otlp.host` defaults to empty, so the push is a no-op WARN)
because, per ADR-0007, "OTLP to Alloy carries traces, not first-party metrics."

A dedicated `OpenTelemetryCollector` (`charts/lightbridge-governance/templates/
otelcollector.yaml`, gated by `values.yaml`'s `copilotOtel` block) has been added to
give that push somewhere to land: it receives the OTLP push, holds it in a Prometheus
exporter's in-memory cache, and exposes it for Alloy's `ServiceMonitor` scrape to pull
into Mimir. This is a new component in a codebase whose house rule is to read the
relevant ADRs before touching what they cover, and it touches two: ADR-0007 rejected
"a new component with stale-metric semantics" in its Alternatives section (a Prometheus
pushgateway), and ADR-0003 states Mimir keeps "only the ~10 low-cardinality
`governance_connector_*` operational metrics... and nothing else." This ADR exists to
make that collision an explicit, recorded decision rather than a chart change that
silently reinterprets both.

## Decision

Adopt the dedicated `OpenTelemetryCollector` as a push-to-pull bridge for exactly one
thing: the `governance_copilot_*` family of **per-run detail** metrics that
`governance-ctl` already computes and that cannot be derived from `ingest_manifests`.
It does not replace, duplicate, or take over `governance_connector_*`, which stays
exactly as ADR-0007 defined it -- API-derived, database-backed, authoritative for
freshness/health.

```
copilot-sync --OTLP/gRPC:4317--> collector (in-memory) --prometheus exporter--> ServiceMonitor --> Alloy --> Mimir
```

Eight series ship today, all defined in `app/governance-ctl/src/metrics.rs`, and **all
of them are gauges** carrying "value as of the last run":
`governance_copilot_last_run_timestamp_seconds` (gauge, label `command`),
`governance_copilot_days` (gauge), `governance_copilot_reports` (gauge, labels
`report`/`status`), `governance_copilot_rows` (gauge, label `report`),
`governance_copilot_ever_synced` (gauge),
`governance_copilot_last_success_age_seconds` (gauge, omitted as a data point
-- not zeroed -- when never synced), `governance_copilot_unmapped_users` (gauge), and
`governance_copilot_manifest_drift` (gauge, `verify` runs only).

The first four were counters when this bridge was first built, and had to change as
part of the same work -- a counter cannot survive this producer's lifecycle. `push()`
constructs a fresh `SdkMeterProvider` per run and the pod exits, so a cumulative
counter restarts from zero every time: `governance_copilot_run` was incremented exactly
once per run and therefore reported `1` forever, making `increase()`/`rate()` read a
flat zero -- indistinguishable from a job that ran once and died, and immune to
PromQL's reset detection because the value never even decreases. It was replaced
outright by a timestamp gauge, which is genuinely useful: since the value *is* a unix
timestamp, `time() - governance_copilot_last_run_timestamp_seconds` is a working
freshness signal despite `send_timestamps: false`. All labels are small,
fixed sets (`command` ∈ {sync, verify, ...}, `report` ∈ the handful of Copilot report
types, `status` ∈ {ok, empty, error-ish outcomes}) -- no per-user, per-repository, or
per-run identifier is a label, so this does not reopen the cardinality problem ADR-0003
was written to close.

## Consequences

**Positive**
- Per-run detail (reports fetched by outcome, rows upserted, drift found, unmapped
  users) becomes visible in Grafana at all, which it is not today (`copilot.otlp.host`
  defaults empty, so the push has always been a silent no-op).
- Reuses the pull pipeline (`ServiceMonitor` → Alloy → Mimir) already built and trusted
  for `governance_connector_*`; no new ingestion path for Grafana to learn.
- Mostly a chart addition (`otelcollector.yaml`, `servicemonitor-copilot-otel.yaml`,
  `ciliumnetworkpolicy-copilot-otel.yaml`) giving an existing, already-working push a
  destination -- `governance-ctl` already spoke `OTEL_EXPORTER_OTLP_ENDPOINT`/OTLP-gRPC.
  The one unavoidable Rust change was the instrument-type correction described above:
  the metrics were only ever a silent no-op before (`copilot.otlp.host` defaulted
  empty), so pointing them at a real destination is precisely what exposed that
  counters cannot work here. Worth noting for future connectors -- **a CronJob that
  builds a fresh meter provider per run must emit gauges, never counters.**

**Negative**
- `governance_copilot_*` is **dashboard-grade, not alert-grade**. Its entire state lives
  in the collector's memory (`replicas: 1`, no `PodDisruptionBudget`). A restart -- node
  drain, image bump, OOM, reschedule -- blanks every series until the next `copilot-sync`
  run, up to 6h later on the `0 */6 * * *` schedule, and nothing pages on that: there is
  no alert on "governance_copilot_* went missing," by design, because a missing series
  here is not distinguishable from "no run happened yet today."
- `metric_expiration: 30h` means a value survives between the normal 6h runs, but a
  connector that has actually been dead for more than ~30h does not read as *stale* --
  it reads as **absent**, a cliff rather than a gradual signal. A dashboard panel wired
  to `governance_copilot_*` without a companion "no data" state will look identical
  whether Copilot sync has been failing for 31 hours or was never deployed.
- This is a second metric family reaching Mimir beyond what ADR-0003 says Mimir keeps
  ("the ~10 low-cardinality `governance_connector_*` operational metrics... and nothing
  else"). See **Relationship to ADR-0007 and ADR-0003** below -- this is not a footnote,
  it is a direct contradiction of that sentence as written.
- New version-skew surface: the `ServiceMonitor`'s selector
  (`app.kubernetes.io/instance: <namespace>.<CR name>`, `app.kubernetes.io/component:
  opentelemetry-collector`) targets a Service the OpenTelemetry Operator generates and
  names itself (`<CR name>-collector`) -- this chart does not define that Service and
  cannot pin its shape. If a future Operator version changes that naming or label
  scheme, `copilot-sync`'s pushes are silently dropped by the default-deny
  `CiliumNetworkPolicy` (no matching pod to reach) and the `ServiceMonitor` resolves to
  zero targets -- neither side surfaces an application-visible error; the failure mode
  is quiet absence, discoverable only by noticing a dashboard is empty.
- One more component to run, patch, and reason about: an `OpenTelemetryCollector` CR,
  its generated `Deployment`/`Service`, a `ServiceMonitor`, and a `CiliumNetworkPolicy`,
  versus zero new components under ADR-0007's original answer.

**Neutral / follow-ups**
- The authoritative freshness signal for alerting remains
  `governance_connector_last_success_timestamp_seconds` (`app/lightbridge-governance/
  src/metrics.rs`), which is entirely unaffected by this collector's restarts, label
  drift, or NetworkPolicy misconfiguration -- an operator debugging "is Copilot sync
  actually healthy" should reach for that gauge first, not `governance_copilot_*`.
- ADR-0003's "Mimir keeps only ... and nothing else" sentence needs a bookkeeping
  update reflecting this decision (see below). This ADR's scope does not include
  editing ADR-0003's body.
- Confirm the OpenTelemetry Operator is actually installed on the target cluster before
  deploying with `copilotOtel.enabled: true` -- the chart cannot verify this at render
  time, and the CR simply never reconciles if the CRD is absent.
- Revisit if `governance_copilot_*` ever needs to be alert-grade: that would require
  either the Postgres-derived approach (see Alternatives) or a `replicas: 2` + PDB +
  file-storage-extension setup for the collector, which is a materially bigger
  component than what is proposed here.

## Alternatives considered

- **Do nothing / leave `copilot.otlp.host` empty (status quo)** -- rejected: this is the
  behavior today, and it means zero run-level visibility. `governance-ctl` already
  computes all of this; discarding a push that already exists in code for want of a
  destination is the weakest alternative, not a neutral one.

- **Push straight to Alloy over OTLP** -- rejected, and already rejected by ADR-0007
  in identical words: "OTLP to Alloy carries traces, not first-party metrics." Nothing
  about this pipeline changes that; Alloy's OTLP receiver still does not turn OTLP
  metrics into Mimir series on its own.

- **Write run detail to Postgres and derive it in the API, exactly like
  `governance_connector_*`** -- taken seriously, and honestly the stronger long-term
  answer. It would add a handful of columns (or a new `ingest_run_detail`-shaped table)
  written by the same idempotent upsert `governance-ctl` already performs against
  `ingest_manifests`, and a second `connector_metrics.rs`-style derivation in the API.
  Against the collector actually built, it would: survive restarts by construction (no
  30h expiration cliff, no memory-only state), avoid the OpenTelemetry Operator
  version-skew risk entirely, add no new Kubernetes component or NetworkPolicy, and sit
  squarely inside ADR-0002 (Postgres is the system of record) and ADR-0003's own stated
  preference for routing detail data through Postgres/SQL rather than Prometheus
  labels -- report-by-status and rows-by-report-type are exactly the kind of tabular
  detail ADR-0003 argues belongs in columns, not squeezed through label cardinality.
  It loses here only on immediate cost: it requires a schema change via
  `crates/governance-core/schema/governance.cstack` (ADR-0009), a migration, and new API
  derivation code, versus a chart-only change to something `governance-ctl` already
  emits. That is an implementation-convenience reason, not an architectural one -- see
  the honest assessment in the report accompanying this ADR.

- **A real Prometheus pushgateway** -- this is what ADR-0007 already rejected ("a new
  component with stale-metric semantics, to publish numbers we can derive from a table
  we already write"). The collector proposed here is not textually a pushgateway, but
  it is the same component-shape for the metrics it carries: a receiver that accepts a
  push and holds it in memory for a puller, with the identical staleness failure mode
  (a dead source reads as a frozen last value, not as "unknown," until expiration). It
  differs from what ADR-0007 rejected in one respect only: the family it carries
  (`governance_copilot_*`) is *not* derivable from a table we already write, so this
  is not "publishing numbers we can derive" -- it is publishing numbers that exist
  nowhere else. That distinction is why this ADR accepts the same tradeoff ADR-0007
  declined, for a narrower purpose, rather than being the same rejected alternative
  under a different CRD.

## Relationship to ADR-0007 and ADR-0003

**ADR-0007 is extended, not superseded.** Its core decision -- `governance_connector_*`
is derived by the always-on API from `ingest_manifests`, no cache service -- is
unchanged and remains the authoritative freshness/health signal. This ADR adds a
second, narrower metric family (per-run detail) that ADR-0007 did not consider and
that cannot be expressed by its derivation approach. It does, however, knowingly adopt
the shape of the alternative ADR-0007's own Alternatives section rejected (a
push-then-pull cache component with stale-metric semantics) -- for a family of metrics
ADR-0007 never evaluated that tradeoff against. A reader of ADR-0007 alone would
reasonably conclude "no pushgateway-shaped component exists here"; that stops being
true once `copilotOtel.enabled: true` ships. ADR-0007's own text is left alone per the
immutability rule; this paragraph is the record of what changed.

**ADR-0003 is partially superseded, plainly.** Its Decision section states Mimir keeps
"the ~10 low-cardinality `governance_connector_*` operational metrics" and "nothing
else." That sentence becomes false the moment this pipeline is enabled: Mimir gains an
eight-series second family. The rest of ADR-0003 -- Grafana reading Postgres directly
for business dashboards, alerting staying on `governance_connector_*`, the
low-cardinality-label rationale for why business data doesn't belong in Prometheus at
all -- is untouched and still correct; only the "and nothing else" boundary is wrong
after this decision.

Per this directory's own rule (`docs/adr/README.md`, "ADRs are immutable once
Accepted"), the correct bookkeeping is: ADR-0003's status line should become
`Superseded by ADR-0011` (partial -- scope-limited to the "and nothing else" clause,
not its Postgres-for-business-dashboards decision, which stands), with a one-paragraph
note added at the top of ADR-0003 stating what changed, and its body otherwise left
alone. This ADR's own scope is limited to adding one new file plus its README index
row; updating ADR-0003's header is a follow-up action, not performed here.

## Related

- RFC: `docs/rfc/0001-github-copilot-connector.md` (RFC-0001) -- the spec this run-detail
  data originates from
- ADR-0002 (Postgres is the system of record) -- the alternative this ADR did not take
- ADR-0003 (Grafana reads Postgres directly) -- partially superseded, see above
- ADR-0007 (the API derives connector metrics; no cache service) -- extended, see above
- ADR-0009 (cratestack is the only persistence layer) -- what the Postgres-derived
  alternative would have used
- Runbook: `docs/runbooks/copilot-sync-failed.md`
