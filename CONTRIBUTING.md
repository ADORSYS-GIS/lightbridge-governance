# Contributing

## Before you write code

Read the [ADRs](docs/adr/README.md) covering the area you are touching. They exist so that
decisions are not re-litigated silently — if you disagree with one, write a superseding ADR
rather than quietly building the other thing.

## Commits

Conventional Commits, enforced in CI. The type drives release-please:

- `feat:` — minor bump. `fix:` — patch. `!` or `BREAKING CHANGE:` — major.
- `docs:`, `refactor:`, `ci:`, `build:`, `perf:`, `test:`, `chore:`.

The body explains **why**; the diff already shows what. AI-assisted commits carry the
`Co-Authored-By:` trailer.

## Pull requests

The governance PR template is mandatory: **AI Usage Declaration**, a real **source-of-truth
link** (a URL or `#123` — a boilerplate governance link does not count), and a
**Verification** section with actual evidence. The `governance` CI job fails a
non-compliant description.

Verification means what you ran and what it printed. "Tests pass" is not evidence; the
output is.

## Checks

```bash
just all-checks   # what CI runs, in CI's order
just deny         # supply-chain audit
```

Clippy warnings are errors. If a lint is genuinely wrong, `#[expect(...)]` it with a reason
rather than `#[allow(...)]` — an expectation that stops being true fails the build, an
allow rots silently.

## Tests

One behaviour per test, named for the behaviour. Prefer a test that would have caught the
bug over a test that covers the line. For anything touching ingestion, the meaningful test
is that **reprocessing the same day changes no row counts** — idempotency is the property,
not coverage.

## Adding a connector

It is a crate under `crates/`, not a repository ([ADR-0005](docs/adr/0005-one-workspace-registry-first-connectors-as-crates.md)).
Write the RFC first, land the ADRs it produces, then build.
