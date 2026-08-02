-- The generator flagged the following as `@default(dbgenerated())` --
-- a marker meaning cratestack emits no DEFAULT and expects one to
-- exist some other way. Added by hand here (see cratestack's own
-- comment: "hand-authored SQL... etc"), since `.cstack` has no syntax
-- to express a real SQL default. Verified empirically: the generated
-- `create_record` path omits these columns from INSERT entirely when
-- absent from the Create input, so without a real DB default every
-- create through the generated CRUD 500s on a NOT NULL violation:
--   - applications.created_at / updated_at
--   - identity_maps.created_at / updated_at / valid_from
--   - ingest_manifests.created_at / updated_at / started_at
--   - integrations.created_at / updated_at
--   - tenants.created_at / updated_at

CREATE TABLE applications (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    name TEXT NOT NULL,
    owner TEXT,
    environment TEXT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE identity_maps (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_user_id TEXT NOT NULL,
    internal_user_id TEXT,
    team_id TEXT,
    cost_center_id TEXT,
    mapping_source TEXT NOT NULL,
    valid_from TIMESTAMPTZ NOT NULL DEFAULT now(),
    valid_to TIMESTAMPTZ,
    PRIMARY KEY (id)
);

CREATE TABLE ingest_manifests (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    report_day TIMESTAMPTZ NOT NULL,
    report_type TEXT NOT NULL,
    status TEXT NOT NULL,
    record_count BIGINT NOT NULL,
    checksum TEXT,
    schema_version BIGINT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (id)
);

CREATE TABLE integrations (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    application_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    environment TEXT NOT NULL,
    credential_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    content_capture TEXT NOT NULL,
    last_telemetry_at TIMESTAMPTZ,
    PRIMARY KEY (id)
);

CREATE TABLE tenants (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (id)
);

-- cratestack's postgres emitter (0.5.0) does not emit FOREIGN KEY/REFERENCES
-- for declared `@relation` fields at all -- verified empirically (grepped
-- cratestack-migrate's emitter for "REFERENCES"/"FOREIGN KEY": zero hits),
-- and there is no application-level check either (`create_record_with_executor`
-- validates auth-derived defaults and policies, never that a referenced row
-- exists). Added by hand so `applications.tenant_id` and
-- `integrations.application_id` -- both already declared as `@relation` in
-- the schema -- are actually enforced. Filed upstream:
-- https://github.com/cratestack/cratestack/issues/260
-- `identity_maps.tenant_id` and `integrations.tenant_id` have no `@relation`
-- declared yet (#16) -- add their FK constraints here when that lands.
ALTER TABLE applications
    ADD CONSTRAINT applications_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants (id);

ALTER TABLE integrations
    ADD CONSTRAINT integrations_application_id_fkey
    FOREIGN KEY (application_id) REFERENCES applications (id);
