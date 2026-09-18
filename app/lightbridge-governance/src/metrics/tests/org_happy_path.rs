use std::time::Duration;

use super::{super::Metrics, connected_pool};

/// End-to-end against a real database: exercises the full happy path --
/// active/engaged users, daily and month-to-date cost, seats assigned
/// and seats never used all render with the tenant's actual values, and
/// `has_data` flips to `1` for both families.
#[tokio::test]
async fn a_tenant_with_data_renders_the_full_org_kpi_family() {
    let Some(pool) = connected_pool().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let metrics = Metrics::new();
    let tenant_id = format!("tenant-org-kpi-full-{}", cuid::cuid2());
    let org = "org-e2e";
    // Two fixed (not "today") days in the same calendar month, so
    // daily cost and month-to-date cost are deliberately DIFFERENT
    // numbers -- this is what makes the two separate assertions below
    // actually load-bearing (with only one day of data the two values
    // would coincide, and a bug that swapped them would go undetected).
    let earlier_day = "2026-04-05";
    let latest_day = "2026-04-08";

    cratestack::sqlx::query(
        "INSERT INTO copilot_org_dailys \
         (id, tenant_id, organization_id, report_day, active_users, engaged_users, \
          total_interactions, code_generations, code_acceptances, loc_suggested, \
          loc_added, loc_deleted, ai_credits, net_cost_micro_usd) \
         VALUES ($1, $2, $3, CAST($4 AS date), 5, 2, 0, 0, 0, 0, 0, 0, 0, 1_000_000)",
    )
    .bind(format!("metrics-e2e-org-earlier:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(earlier_day)
    .execute(&pool)
    .await
    .expect("insert earlier org daily fixture");

    cratestack::sqlx::query(
        "INSERT INTO copilot_org_dailys \
         (id, tenant_id, organization_id, report_day, active_users, engaged_users, \
          total_interactions, code_generations, code_acceptances, loc_suggested, \
          loc_added, loc_deleted, ai_credits, net_cost_micro_usd) \
         VALUES ($1, $2, $3, CAST($4 AS date), 17, 9, 0, 0, 0, 0, 0, 0, 0, 4_500_000)",
    )
    .bind(format!("metrics-e2e-org:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(latest_day)
    .execute(&pool)
    .await
    .expect("insert org daily fixture");

    cratestack::sqlx::query(
        "INSERT INTO copilot_seat_snapshots \
         (id, tenant_id, organization_id, snapshot_day, provider_user_id, user_login, \
          seat_assigned_at, last_activity_at, last_activity_editor, seat_state) \
         VALUES ($1, $2, $3, CAST($4 AS date), 'user-used', 'user-used', now(), now(), \
                 NULL, 'active')",
    )
    .bind(format!("metrics-e2e-seat-used:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(latest_day)
    .execute(&pool)
    .await
    .expect("insert used seat fixture");

    cratestack::sqlx::query(
        "INSERT INTO copilot_seat_snapshots \
         (id, tenant_id, organization_id, snapshot_day, provider_user_id, user_login, \
          seat_assigned_at, last_activity_at, last_activity_editor, seat_state) \
         VALUES ($1, $2, $3, CAST($4 AS date), 'user-never-used', 'user-never-used', now(), \
                 NULL, NULL, 'active')",
    )
    .bind(format!("metrics-e2e-seat-unused:{tenant_id}"))
    .bind(&tenant_id)
    .bind(org)
    .bind(latest_day)
    .execute(&pool)
    .await
    .expect("insert never-used seat fixture");

    metrics
        .refresh_org_kpis(&pool, &tenant_id, Duration::from_secs(3))
        .await;

    let out = metrics.render();
    assert!(
        out.contains(&format!(
            "governance_org_active_users{{organization_id=\"{org}\"}} 17"
        )),
        "must report the LATEST day's active_users (17), not the earlier day's (5) -- \
         got:\n{out}"
    );
    assert!(out.contains(&format!(
        "governance_org_engaged_users{{organization_id=\"{org}\"}} 9"
    )));
    assert!(
        out.contains(&format!(
            "governance_org_daily_cost_micro_usd{{organization_id=\"{org}\"}} 4500000"
        )),
        "daily cost must be only the latest day's own cost (4_500_000), not summed with \
         the earlier day -- got:\n{out}"
    );
    assert!(
        out.contains(&format!(
            "governance_org_cost_month_to_date_micro_usd{{organization_id=\"{org}\"}} 5500000"
        )),
        "month-to-date must sum both days in the month (1_000_000 + 4_500_000 = \
         5_500_000), not just the latest day's own cost -- got:\n{out}"
    );
    assert!(out.contains(&format!(
        "governance_org_seats_assigned{{organization_id=\"{org}\"}} 2"
    )));
    assert!(out.contains(&format!(
        "governance_org_seats_never_used{{organization_id=\"{org}\"}} 1"
    )));
    assert!(out.contains("governance_org_kpi_has_data{family=\"usage\"} 1"));
    assert!(out.contains("governance_org_kpi_has_data{family=\"seats\"} 1"));
}
