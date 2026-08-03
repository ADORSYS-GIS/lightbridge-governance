# redact-extproc

An Envoy `ext_proc` gRPC server applying `governance-redact` to gateway traffic (ADR-0116).
Deployed as a **sidecar in the gateway pod**, not a standalone Deployment — reached over
loopback by the Envoy `ext_proc` filter, not over the pod network. There is no chart for it
in this repo; it deploys as part of whatever chart owns the gateway pod itself.

## How this differs from `redact-gateway`

Same engine (`governance-redact::Engine`), same validation stance in `config.rs` (mirrors
`redact-gateway`'s deliberately — reject an unknown profile rather than default to a weaker
one, never let the salt reach `Debug`), but a different integration shape:

- `redact-gateway` is a **front proxy** — a separate hop the request round-trips through.
- `redact-extproc` is an **Envoy `ext_proc` filter** — no extra hop, but it can only act on
  what Envoy's processing mode hands it, and Envoy's two directions use different modes.

## Two directions, two processing modes

- **Request** — arrives as one whole `RequestBody` message (`processingMode.request.body:
  Buffered`). Identical logic to `redact-gateway`'s request path, since the input shape is
  identical.
- **Response** — arrives incrementally (`processingMode.response.body: Streamed`) and is
  scanned via `governance_redact::SseHoldBack`, which is SSE-frame-aware: it extracts
  exactly `delta.content` before redacting, and snaps every release to a whole SSE frame
  boundary so a redaction can never land mid-frame. See [`service.rs`](src/service.rs)'s
  module doc for what this closed relative to the front-proxy era (a raw-byte `HoldBack`
  with no notion of SSE structure).

⚠️ **Known gap, not yet handled:** a response chunk boundary landing mid-UTF-8 codepoint.
Real streamed non-ASCII text will hit this. Current behavior fails closed rather than
silently misdecoding — see `handle_response_chunk` in `service.rs`.

## Configuration

`LISTEN_ADDR` (default `127.0.0.1:9500`, loopback-only on purpose — see
[`config.rs`](src/config.rs)), `METRICS_LISTEN_ADDR` (default `0.0.0.0:9501`),
`REDACT_PROFILE`, `REDACT_HASH_SALT`, `RESPONSE_HOLD_BACK_BYTES` (must exceed the longest
entity that must be caught whole — see `governance_redact::holdback` for why).

## Running locally

```bash
REDACT_HASH_SALT=dev-salt cargo run --bin redact-extproc
```

There's no upstream to forward to here (unlike `redact-gateway`) — this only implements the
`ExternalProcessor` gRPC contract; Envoy is the caller, not a client you'd curl directly.
