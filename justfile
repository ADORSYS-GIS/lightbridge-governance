# lightbridge-governance -- task runner (mirrors lightbridge-authz)
# Author: @stephane-segning

c := ""

# Show this help
help:
	@just --summary

# Format code
fmt:
	cargo fmt --all

# Lint. Warnings are errors -- CI runs the same line.
clippy:
	cargo clippy --all-targets --all-features -- -D warnings

# Type-check without producing binaries
check:
	cargo check --all-targets --all-features

# Run the test suite
test:
	cargo test --all-features

# Everything CI runs, in CI's order
all-checks: fmt clippy check test

# Render the aiCliOtel collector and assert its OIDC auth gate survives.
# Deliberately NOT part of `all-checks`: it needs helm and mikefarah yq, which
# a Rust-only contributor may not have. CI runs it in the `helm-charts` job.
# `helm dependency build` runs first because the chart pulls the external
# `app-template` dependency (bjw-s), without which `helm template` errors out.
chart-checks:
	helm dependency build charts/lightbridge-governance
	charts/lightbridge-governance/tests/assert-oidc-auth.sh charts/lightbridge-governance

# Supply-chain audit (same checks as the SAST job)
deny:
	cargo deny check advisories bans licenses sources

# Bring up Postgres for local development
up:
	docker compose -p lightbridge-governance -f compose.yaml up -d --remove-orphans {{c}}

# Tear down, keeping volumes
down:
	docker compose -p lightbridge-governance -f compose.yaml down

# Tear down and DESTROY volumes
nuke:
	docker compose -p lightbridge-governance -f compose.yaml down -v

# Apply migrations. cratestack derives these from schema/governance.cstack --
# there are no hand-written migration files to edit (ADR-0009).
migrate:
	cargo run --bin governance-ctl -- migrate

# Build the redact-gateway / redact-extproc images. Requires PROVIDER_BASE_URL
# in .env (copy from .env.example) -- a real OpenAI-compatible LLM, not a mock.
redact-build:
	docker compose -p lightbridge-governance -f compose.yaml --profile redact build

# Bring up redact-gateway (and redact-extproc) against a real upstream LLM
redact-up:
	docker compose -p lightbridge-governance -f compose.yaml --profile redact up -d --remove-orphans {{c}}

# Tear down the redact stack, keeping volumes
redact-down:
	docker compose -p lightbridge-governance -f compose.yaml --profile redact down

# Health check redact-gateway and redact-extproc. Fails if either is unhealthy.
# Uses -sf unconditionally so it waits rather than fails-fast; the recipe
# always exits 0 after showing OK/FAIL so CI sees a real exit code from the
# docker-compose up itself.
redact-test:
    @echo "Checking redact-gateway (/livez)..." && curl -sf -H "Host: localhost:8080" http://localhost:8080/livez && echo " OK" || exit 1
    @echo "Checking redact-extproc (/livez)..." && curl -sf http://localhost:9501/livez && echo " OK" || exit 1

# Live end-to-end redaction test against a real LLM.
#
# Required:
#   export REDACT_API_KEY='your-key-here'
#
# Optional:
#   export REDACT_MODEL='llama-3.1-8b-instant'    # defaults to llama-3.1-8b-instant
#   export REDACT_TEST=1                          # full scenario suite
#   export REPEAT=10                             # concurrent load multiplier
#
# just targets:
#   just redact-test-live              # single clean streaming request
#   REDACT_TEST=1 just redact-test-live   # full scenario suite
#   REPEAT=10 just redact-test-live       # 10x concurrent load
#
# Requires: just redact-up (stack must be running)
redact-test-live:
	/usr/bin/env bash scripts/test-redact-live.sh

# --- VS Code extension (ide/vscode) ------------------------------------------
#
# npm lives entirely inside ide/vscode and nothing in the Rust workspace depends
# on it. `just all-checks` deliberately does NOT run these: the Rust gate must
# not start requiring a node toolchain. The extension has its own PATH-FILTERED
# workflow (.github/workflows/vscode-extension.yml), so a Rust-only PR still
# starts no node job. AGENTS.md states the rule this fence enforces.

# Install the extension's dev dependencies (lockfile is the input, like CI)
ext-install:
	npm --prefix ide/vscode ci

# Typecheck and unit-test the extension
ext-check:
	npm --prefix ide/vscode run typecheck
	npm --prefix ide/vscode test

# Bundle the extension into ide/vscode/dist
ext-build:
	npm --prefix ide/vscode run build

# Produce a .vsix
ext-package:
	npm --prefix ide/vscode run package
