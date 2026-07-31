-- Registry + normalized connector tables (ADR-0005).
--
-- Money is integer micro-USD everywhere (ADR-0008). Never NUMERIC-as-float,
-- never DOUBLE PRECISION.
--
-- `tenant_id` is on every row even though a deployment serves ONE tenant
-- (ADR-0001) -- it makes our install and a customer's share one schema, and it
-- is the single column every query filters on.

CREATE TABLE tenant (
    id          TEXT PRIMARY KEY,
    name        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE application (
    id          TEXT PRIMARY KEY,
    tenant_id   TEXT        NOT NULL REFERENCES tenant (id),
    name        TEXT        NOT NULL,
    owner       TEXT,
    environment TEXT        NOT NULL DEFAULT 'production',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name, environment)
);

-- One credential per integration. We store ONLY the argon2id hash; the token is
-- returned once at creation and never again. Revocation is a status flip that
-- /internal/v1/resolve reads (ADR-0006).
CREATE TABLE integration (
    id                TEXT PRIMARY KEY,
    tenant_id         TEXT        NOT NULL REFERENCES tenant (id),
    application_id    TEXT        NOT NULL REFERENCES application (id),
    provider          TEXT        NOT NULL,
    environment       TEXT        NOT NULL DEFAULT 'production',
    credential_hash   TEXT        NOT NULL,
    status            TEXT        NOT NULL DEFAULT 'active',
    content_capture   TEXT        NOT NULL DEFAULT 'metadata_only',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_telemetry_at TIMESTAMPTZ,
    CONSTRAINT integration_status_ck
        CHECK (status IN ('active', 'suspended', 'revoked')),
    CONSTRAINT integration_content_capture_ck
        CHECK (content_capture IN ('metadata_only', 'redacted', 'full'))
);

CREATE INDEX integration_tenant_status_idx ON integration (tenant_id, status);

-- Maps a provider-side principal to an internal identity. Deliberately NOT
-- matched on display name -- verified email or an explicit mapping only.
CREATE TABLE identity_map (
    tenant_id        TEXT        NOT NULL REFERENCES tenant (id),
    provider         TEXT        NOT NULL,
    provider_user_id TEXT        NOT NULL,
    internal_user_id TEXT,
    team_id          TEXT,
    cost_center_id   TEXT,
    mapping_source   TEXT        NOT NULL,
    valid_from       TIMESTAMPTZ NOT NULL DEFAULT now(),
    valid_to         TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, provider, provider_user_id, valid_from)
);

-- Every ingest run writes one row per (day, report). This is what makes
-- reprocessing idempotent AND what the API derives connector health from,
-- because a CronJob pod cannot be scraped (ADR-0007).
CREATE TABLE ingest_manifest (
    tenant_id      TEXT        NOT NULL REFERENCES tenant (id),
    provider       TEXT        NOT NULL,
    scope_id       TEXT        NOT NULL,
    report_day     DATE        NOT NULL,
    report_type    TEXT        NOT NULL,
    status         TEXT        NOT NULL,
    record_count   BIGINT      NOT NULL DEFAULT 0,
    checksum       TEXT,
    schema_version INT         NOT NULL DEFAULT 1,
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at   TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, provider, scope_id, report_day, report_type)
);

CREATE INDEX ingest_manifest_completed_idx
    ON ingest_manifest (tenant_id, provider, completed_at DESC);
