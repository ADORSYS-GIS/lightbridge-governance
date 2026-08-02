-- NOTE: the following column(s) use `@default(dbgenerated())`, a
-- marker meaning the value is expected to come from a real
-- Postgres-level default set some other way (hand-authored SQL, a
-- trigger, GENERATED ... AS IDENTITY, etc). cratestack does not
-- emit a DEFAULT clause for it. Added by hand here, same as the init
-- migration's `applications`/etc (#18) -- without it every create through
-- the generated CRUD 500s on a NOT NULL violation:
--   - environments.created_at / updated_at

ALTER TABLE applications DROP COLUMN environment;

ALTER TABLE integrations DROP COLUMN environment;

CREATE TABLE environments (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    application_id TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (id)
);

ALTER TABLE integrations ADD COLUMN environment_id TEXT NOT NULL;

-- cratestack's postgres emitter (0.5.1) still does not emit FOREIGN KEY for
-- declared `@relation` fields (cratestack/cratestack#260) -- same hand-add
-- as the init migration, extended to Environment's relations and
-- Integration's new environment_id.
ALTER TABLE environments
    ADD CONSTRAINT environments_tenant_id_fkey
    FOREIGN KEY (tenant_id) REFERENCES tenants (id);

ALTER TABLE environments
    ADD CONSTRAINT environments_application_id_fkey
    FOREIGN KEY (application_id) REFERENCES applications (id);

ALTER TABLE integrations
    ADD CONSTRAINT integrations_environment_id_fkey
    FOREIGN KEY (environment_id) REFERENCES environments (id);

-- cratestack also still does not emit CREATE UNIQUE INDEX for a model-level
-- `@@unique([...])` (cratestack/cratestack#262) -- same hand-add as the init
-- migration. `applications_tenant_id_name_environment_key` (init migration)
-- was implicitly dropped by `DROP COLUMN environment` above (Postgres drops
-- an index when a column it references is dropped) -- this replaces it with
-- the new, narrower uniqueness rule now that environment lives on its own
-- model.
--
-- NOTE for the next person editing this file: never write a semicolon
-- character inside a plain SQL comment anywhere in this file, only real
-- statement terminators. `apply_pending`'s naive splitter (cratestack/
-- cratestack#270) breaks on it exactly like real SQL, mid-sentence,
-- confirmed the hard way while drafting this very file: prose using that
-- punctuation mark here produced a genuine "syntax error" on apply.
CREATE UNIQUE INDEX applications_tenant_id_name_key
    ON applications (tenant_id, name);

CREATE UNIQUE INDEX environments_natural_key
    ON environments (tenant_id, application_id, name);
