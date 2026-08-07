-- NOTE: the following column(s) use `@default(dbgenerated())`, a
-- marker meaning the value is expected to come from a real
-- Postgres-level default set some other way (hand-authored SQL, a
-- trigger, GENERATED ... AS IDENTITY, etc). cratestack does not
-- emit a DEFAULT clause for it. If no such default exists,
-- INSERTs that omit the column will fail with a NOT NULL violation:
--   - copilot_org_dailys.created_at
--   - copilot_org_dailys.updated_at
--   - copilot_repo_dailys.created_at
--   - copilot_repo_dailys.updated_at
--   - copilot_seat_snapshots.created_at
--   - copilot_seat_snapshots.updated_at
--   - copilot_user_dailys.created_at
--   - copilot_user_dailys.updated_at
--   - copilot_user_teams.created_at
--   - copilot_user_teams.updated_at

CREATE TABLE copilot_org_dailys (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    report_day TIMESTAMPTZ NOT NULL,
    active_users BIGINT NOT NULL,
    engaged_users BIGINT NOT NULL,
    total_interactions BIGINT NOT NULL,
    code_generations BIGINT NOT NULL,
    code_acceptances BIGINT NOT NULL,
    loc_suggested BIGINT NOT NULL,
    loc_added BIGINT NOT NULL,
    loc_deleted BIGINT NOT NULL,
    ai_credits BIGINT NOT NULL,
    net_cost_micro_usd BIGINT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE copilot_repo_dailys (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    report_day TIMESTAMPTZ NOT NULL,
    repository_id TEXT NOT NULL,
    coding_agent_activity BIGINT NOT NULL,
    code_review_activity BIGINT NOT NULL,
    pull_request_activity BIGINT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE copilot_seat_snapshots (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    snapshot_day TIMESTAMPTZ NOT NULL,
    provider_user_id TEXT NOT NULL,
    user_login TEXT NOT NULL,
    seat_assigned_at TIMESTAMPTZ,
    last_activity_at TIMESTAMPTZ,
    last_activity_editor TEXT,
    seat_state TEXT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE copilot_user_dailys (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    report_day TIMESTAMPTZ NOT NULL,
    provider_user_id TEXT NOT NULL,
    user_login TEXT NOT NULL,
    total_interactions BIGINT NOT NULL,
    code_generations BIGINT NOT NULL,
    code_acceptances BIGINT NOT NULL,
    ai_credits BIGINT NOT NULL,
    net_cost_micro_usd BIGINT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE copilot_user_teams (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    report_day TIMESTAMPTZ NOT NULL,
    user_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    team_slug TEXT NOT NULL,
    PRIMARY KEY (id)
);

-- cratestack's postgres emitter (0.5.1) does not emit CREATE UNIQUE INDEX for
-- a model-level `@@unique([...])` (cratestack/cratestack#262) -- same hand-add
-- as `ingest_manifests_natural_key`, etc. This is what makes the collector's
-- `ON CONFLICT DO UPDATE` idempotent: reprocessing a day targets these keys and
-- replaces rows rather than duplicating them (RFC-0001 idempotency invariant).
--
-- These tables hold the normalized Copilot report rows, keyed on the report's
-- natural grain so a republished report overwrites its predecessor.
CREATE UNIQUE INDEX copilot_org_dailys_natural_key
    ON copilot_org_dailys (tenant_id, organization_id, report_day);

CREATE UNIQUE INDEX copilot_user_dailys_natural_key
    ON copilot_user_dailys (tenant_id, organization_id, report_day, provider_user_id);

CREATE UNIQUE INDEX copilot_repo_dailys_natural_key
    ON copilot_repo_dailys (tenant_id, organization_id, report_day, repository_id);

CREATE UNIQUE INDEX copilot_user_teams_natural_key
    ON copilot_user_teams (tenant_id, organization_id, report_day, user_id, team_id);

CREATE UNIQUE INDEX copilot_seat_snapshots_natural_key
    ON copilot_seat_snapshots (tenant_id, organization_id, snapshot_day, provider_user_id);

