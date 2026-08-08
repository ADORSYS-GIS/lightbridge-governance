# Captured fixtures

Empty today. This directory exists so a real OTLP export, once someone with cluster access
captures one, is a drop-in: copy the (scrubbed) payload to `<provider>/<case>.json`, run

```text
GOVERNANCE_FOUNDRY_BLESS_FIXTURES=1 cargo test -p governance-foundry --test normalizer_fixtures
```

to generate `<provider>/<case>.expected.json`, review the diff by hand, and commit both. No
change to `tests/normalizer_fixtures.rs` or to any `src/` file is needed -- the harness
walks this directory exactly like `../synthetic/`.

See [`docs/integrations/foundry-golden-fixtures.md`](../../../../docs/integrations/foundry-golden-fixtures.md#capture-procedure)
for the actual capture steps, and [`../README.md`](../README.md) for the fixture pair shape
and what a captured fixture retires from RFC-0002's Verification section that a synthetic
one cannot.

**Before committing a capture:** scrub it. A real export may carry a genuine
`user.email`, a `session.id` that correlates to a real workspace, or prompt/tool content if
the integration's capture mode was ever above `metadata_only`. Replace anything identifying
with an obviously-synthetic placeholder (`captured-user@example.invalid`, a random
`session.id`) before it reaches a commit -- this repo is public.
