-- Telemetry models: executions, model_calls, tool_calls, model_pricing (#30)
--
-- All money is micro-USD (ADR-0008). All writes are idempotent on (trace_id, span_id).
-- `tenant_id` is derived from the authenticated credential, never from the telemetry body.

-- NOTE: the following column(s) use `@default(dbgenerated())`, a marker meaning
-- the value is expected to come from a real Postgres-level default set some other
-- way (hand-authored SQL, a trigger, GENERATED ... AS IDENTITY, etc). cratestack
-- does not emit a DEFAULT clause for it. Added by hand here, same as the init
-- migration's `applications`/etc (#18) -- without it every create through the
-- generated CRUD 500s on a NOT NULL violation:
--   - executions.created_at / updated_at
--   - model_calls.created_at / updated_at
--   - tool_calls.created_at / updated_at
--   - model_pricing.created_at / updated_at / effective_from

CREATE TABLE executions (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    integration_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    user_email TEXT,
    -- The Keycloak sub (internal user ID) derived from the ingest token via
    -- IdentityMap. This is the authoritative identity for attribution (#35),
    -- never the payload's user.email (which is self-asserted and may be absent).
    internal_user_id TEXT,
    started_at TIMESTAMPTZ NOT NULL,
    duration_ms BIGINT NOT NULL,
    -- NULL = cost unknown (no pricing row, or a model call with unknown token
    -- counts). Unknown is honest: a zero would read as "free" on a dashboard.
    estimated_cost_micro_usd BIGINT,
    raw_backend TEXT,
    raw_schema_version BIGINT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE model_calls (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    model TEXT NOT NULL,
    -- NULL = unknown (the payload did not report token counts): the cost is
    -- then also NULL, never a zero that a dashboard would read as "free".
    input_tokens BIGINT,
    output_tokens BIGINT,
    cost_micro_usd BIGINT,
    PRIMARY KEY (id)
);

CREATE TABLE tool_calls (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    duration_ms BIGINT NOT NULL,
    PRIMARY KEY (id)
);

CREATE TABLE model_pricing (
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    id TEXT NOT NULL,
    model TEXT NOT NULL,
    input_per_million_micro_usd BIGINT NOT NULL,
    output_per_million_micro_usd BIGINT NOT NULL,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT now(),
    effective_to TIMESTAMPTZ,
    PRIMARY KEY (id)
);

-- cratestack's postgres emitter (0.5.1) still does not emit FOREIGN KEY for
-- declared `@relation` fields (cratestack/cratestack#260) -- same hand-add
-- as the init migration and environment_model migration.
ALTER TABLE model_calls
    ADD CONSTRAINT model_calls_execution_id_fkey
    FOREIGN KEY (execution_id) REFERENCES executions (id);

ALTER TABLE tool_calls
    ADD CONSTRAINT tool_calls_execution_id_fkey
    FOREIGN KEY (execution_id) REFERENCES executions (id);

-- cratestack also still does not emit CREATE UNIQUE INDEX for a model-level
-- `@@unique([...])` (cratestack/cratestack#262) -- same hand-add as the init
-- migration and environment_model migration.
--
-- NOTE for the next person editing this file: never write a semicolon character
-- inside a plain SQL comment anywhere in this file, only real statement
-- terminators. `apply_pending`'s naive splitter (cratestack/cratestack#270)
-- breaks on it exactly like real SQL, mid-sentence, confirmed the hard way
-- while drafting the environment_model migration.
CREATE UNIQUE INDEX executions_trace_id_span_id_key
    ON executions (trace_id, span_id);

CREATE UNIQUE INDEX model_calls_trace_id_span_id_key
    ON model_calls (trace_id, span_id);

CREATE UNIQUE INDEX tool_calls_trace_id_span_id_key
    ON tool_calls (trace_id, span_id);

CREATE UNIQUE INDEX model_pricing_model_effective_from_key
    ON model_pricing (model, effective_from);

-- Developer identity binding (#35): add column to integrations table to bind
-- tokens to specific developers. This enables per-developer attribution.
ALTER TABLE integrations
    ADD COLUMN internal_user_id TEXT;