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

# Health check redact-gateway and redact-extproc
redact-test:
	@echo "Checking redact-gateway (/livez)..." && curl -sf http://localhost:8080/livez && echo " OK" || echo " FAILED"
	@echo "Checking redact-extproc (/livez)..." && curl -sf http://localhost:9501/livez && echo " OK" || echo " FAILED"
