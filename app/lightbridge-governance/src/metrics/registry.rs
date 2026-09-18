use prometheus::{IntCounterVec, IntGaugeVec, Registry, opts};

/// The set of Prometheus metric families this process exposes on `/metrics`.
///
/// Construction is split along the same seam as the refresh logic: each
/// family (`connector` / `org`) builds and registers its own gauges in its
/// own module, and [`Metrics::new`] assembles the result. The fields are
/// `pub(super)` only so the sibling `tests` module can drive them directly;
/// none of them is part of the public API surface.
pub struct Metrics {
    registry: Registry,
    /// Unix timestamp (seconds) of the most recent report day `{provider}`
    /// has successfully ingested at least one report. Absent for a
    /// provider that has never synced, or before the first successful
    /// refresh -- never `0`, which reads as "just synced". Backs the
    /// runbook's "no successful sync in 36h" / "report older than 72h"
    /// alerts via `time() - metric > threshold` in PromQL.
    pub(super) connector_last_success_timestamp_seconds: IntGaugeVec,
    /// `1` once `{provider}` has EVER recorded a successful manifest row,
    /// `0` if a refresh has confirmed it never has. Absent only before the
    /// first successful refresh. This is what makes a freshly deployed
    /// connector (which has no timestamp to be stale) distinguishable from a
    /// healthy one -- see the module doc comment.
    pub(super) connector_has_synced: IntGaugeVec,
    /// Failed `/metrics` refresh attempts against Postgres, by `reason`.
    /// Covers both `governance_connector_*` (`timeout`/`query_error`,
    /// ADR-0007) and `governance_org_*` (`org_usage_timeout`/
    /// `org_usage_query_error`/`org_seats_timeout`/`org_seats_query_error`)
    /// -- one shared counter rather than a second one, since both are "a
    /// `/metrics` scrape's Postgres refresh failed" and an operator watching
    /// for scrape-path trouble should not need to know which family to
    /// check. Always present, starting at `0` for every reason -- unlike the
    /// gauges themselves, "no failures yet" IS a safe default for a counter,
    /// so every reason is set to `0` at registration rather than left
    /// absent. An alert can watch `increase(...[10m]) > 0` as a
    /// belt-and-suspenders signal independent of the gauges being absent or
    /// stale.
    pub connector_metrics_scrape_errors_total: IntCounterVec,
    /// Active users on `{organization_id}`'s most recent AVAILABLE report
    /// day (ADR-0001 tenant_id is in the query's WHERE clause, never a
    /// label -- see `governance_core::org_kpis`). Absent until a refresh has
    /// actually observed a row for that organization.
    pub(super) org_active_users: IntGaugeVec,
    /// Engaged users, same day/absence contract as `org_active_users`.
    pub(super) org_engaged_users: IntGaugeVec,
    /// Integer micro-USD (ADR-0008): net cost on `{organization_id}`'s most
    /// recent available report day.
    pub(super) org_daily_cost_micro_usd: IntGaugeVec,
    /// Integer micro-USD (ADR-0008): net cost summed from the first day of
    /// that report day's calendar month through the report day itself.
    pub(super) org_cost_month_to_date_micro_usd: IntGaugeVec,
    /// Seats assigned as of `{organization_id}`'s most recent seat snapshot.
    pub(super) org_seats_assigned: IntGaugeVec,
    /// Seats assigned as of the most recent snapshot with
    /// `last_activity_at IS NULL` -- the licence-waste signal. Same
    /// day/absence contract as the gauges above.
    pub(super) org_seats_never_used: IntGaugeVec,
    /// `1` once a refresh has confirmed at least one row exists for this
    /// tenant in the `family` table (`usage` = `copilot_org_dailys`,
    /// `seats` = `copilot_seat_snapshots`; any organization, any day), `0`
    /// once a refresh has confirmed there are none, absent before that
    /// family's first successful refresh.
    ///
    /// Labeled by `family`, not `organization_id`: the question this
    /// answers -- "does this TENANT have any data at all" -- is meaningless
    /// per-organization, since an organization that has never reported
    /// cannot appear as an `organization_id` label value in the first place
    /// (see the module doc comment / `org_kpis`'s absent-vs-zero contract).
    /// `family` is a fixed two-value set, so this stays as bounded as a
    /// genuinely unlabeled gauge would be.
    ///
    /// Deliberately an `IntGaugeVec`, not a plain scalar `IntGauge`: a
    /// scalar gauge always renders (defaulting to `0`) the instant it is
    /// registered, which would make "confirmed zero" and "never yet
    /// refreshed" both read as `0` -- exactly the ambiguity this gauge
    /// exists to remove. `IntGaugeVec` only materializes a series once
    /// `with_label_values(...)` is actually called, matching
    /// `governance_connector_has_synced`'s own absent-until-touched
    /// mechanism.
    pub(super) org_kpi_has_data: IntGaugeVec,
}

impl Metrics {
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "impossibility proof: metric construction only fails on duplicate names or \
                  invalid help text, and both are compile-time string literals here"
    )]
    pub fn new() -> Self {
        let registry = Registry::new();
        let connector = super::connector::build(&registry);
        let org = super::org::build(&registry);
        let connector_metrics_scrape_errors_total = IntCounterVec::new(
            opts!(
                "governance_connector_metrics_scrape_errors_total",
                "failed /metrics Postgres refresh attempts, by reason -- covers both \
                 governance_connector_* (ADR-0007) and governance_org_* (org-level KPI gauges)"
            ),
            &["reason"],
        )
        .expect("static metric definition");
        super::register(
            &registry,
            vec![Box::new(connector_metrics_scrape_errors_total.clone())],
        );

        let metrics = Self {
            registry,
            connector_last_success_timestamp_seconds: connector.last_success_timestamp_seconds,
            connector_has_synced: connector.has_synced,
            connector_metrics_scrape_errors_total,
            org_active_users: org.active_users,
            org_engaged_users: org.engaged_users,
            org_daily_cost_micro_usd: org.daily_cost_micro_usd,
            org_cost_month_to_date_micro_usd: org.cost_month_to_date_micro_usd,
            org_seats_assigned: org.seats_assigned,
            org_seats_never_used: org.seats_never_used,
            org_kpi_has_data: org.has_data,
        };

        // "No failures yet" is a legitimate, non-misleading default for a
        // counter (unlike the freshness/KPI gauges) -- initialize every
        // reason to 0 so the series exists from process start rather than
        // only appearing the first time something actually fails.
        for reason in [
            "timeout",
            "query_error",
            "org_usage_timeout",
            "org_usage_query_error",
            "org_seats_timeout",
            "org_seats_query_error",
        ] {
            metrics
                .connector_metrics_scrape_errors_total
                .with_label_values(&[reason]);
        }

        metrics
    }

    #[must_use]
    pub fn render(&self) -> String {
        use prometheus::Encoder;
        let encoder = prometheus::TextEncoder::new();
        let mut buf = Vec::new();
        if encoder.encode(&self.registry.gather(), &mut buf).is_err() {
            return String::new();
        }
        String::from_utf8(buf).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
