# syntax=docker/dockerfile:1

# ⚠️ cargo-auditable is NOT optional. Without it Trivy scans only the base OS and
# reports a clean result that means "it never looked at your Rust dependencies".
FROM rust:1-bookworm AS builder
WORKDIR /src
RUN cargo install cargo-auditable --locked
COPY . .
RUN cargo auditable build --profile prod \
      --bin lightbridge-governance --bin governance-ctl

# Distroless: no shell in the runtime image. rustls everywhere in the dependency
# tree means no OpenSSL and no system CA bundle is strictly required -- the
# webpki roots are compiled in. `ca-certificates` is copied anyway as insurance
# for the day someone flips on native-roots, whose failure mode is a TLS
# handshake error that never says the word "certificate".
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /src/target/prod/lightbridge-governance /usr/local/bin/
COPY --from=builder /src/target/prod/governance-ctl /usr/local/bin/
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/lightbridge-governance"]
