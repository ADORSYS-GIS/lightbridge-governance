-- Reverse dependency order: integrations references applications, which
-- references tenants (the FK constraints added by hand to up.sql).
DROP TABLE integrations;

DROP TABLE applications;

DROP TABLE tenants;

DROP TABLE identity_maps;

DROP TABLE ingest_manifests;
