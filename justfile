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
