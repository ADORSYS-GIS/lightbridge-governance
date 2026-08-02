-- Reverse dependency order: integrations references applications and
-- tenants; applications and identity_maps reference tenants (the FK
-- constraints added by hand to up.sql). tenants drops last of the three.
DROP TABLE integrations;

DROP TABLE applications;

DROP TABLE identity_maps;

DROP TABLE tenants;

DROP TABLE ingest_manifests;
