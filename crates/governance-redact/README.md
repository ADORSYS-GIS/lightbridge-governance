# governance-redact

PII and secret detection/redaction for the AI request path. This is the policy engine both
`redact-gateway` (front proxy) and `redact-extproc` (Envoy sidecar, ADR-0116) wrap — it has
no HTTP, no gRPC, and no process of its own.

## Why this crate exists

The platform previously ran a third-party redaction gateway (`censgate/redact`) as a front
proxy. Its published release couldn't even start (a glibc mismatch between its build and
runtime images), and it sat in the fail-closed path of every AI request. A binary in that
position has to be one we build, test, and can fix — hence owning the policy layer here.

Detection itself (`pii` crate: text in, spans out) stays a bounded, pure-function library
dependency with no network and no release engineering of ours riding on it. If it stalls,
it can be vendored or replaced without touching the request path.

## What this crate does not do

- **It does not detect personal names.** See `Profile::detects_names` — that needs a model
  the `pii` crate's `candle-ner` feature doesn't ship.
- **It does not stream.** `Engine::scan` takes a complete string. Chunk-boundary handling
  (holding back enough of a stream to catch an entity split across chunks) belongs to the
  caller, since only the caller knows the transport's chunk boundaries.

## Modules

| Module | Owns |
|---|---|
| [`engine`](src/engine.rs) | `Engine`, `Verdict`, `Span` — the scan entry point and its outcome types. |
| [`profile`](src/profile.rs) | Named policies (`coding-assistant`, `secrets-only`, `observe-only`): which entities matter, `Action` per entity, and whether the profile fails closed. An unknown profile name is **rejected at startup**, never resolved to a weaker default. |
| [`secrets`](src/secrets.rs) | Pattern/validator-based secret recognizers (API keys, tokens, private keys). |
| [`payload`](src/payload.rs) | `scan_request`/`scan_response` — walks an OpenAI-shaped JSON body (chat messages, embeddings input, tool-call arguments) and redacts only recognised text fields. |
| [`streaming`](src/streaming.rs) | `scan_sse` — applies the engine across an SSE stream using `holdback`'s window. |
| [`sse`](src/sse.rs) | SSE framing: parsing/re-emitting `data:` lines, the `[DONE]` sentinel, multi-choice streams. |
| [`holdback`](src/holdback.rs) | `HoldBack` — the chunk-boundary buffering window itself; `DEFAULT_WINDOW` must exceed the longest entity that must be caught whole, or that entity can straddle the release boundary forever. |
| [`error`](src/error.rs) | This crate's `Error`/`Result`. |

## Fail-closed is a profile property, not a global

Whether an indeterminate scan result blocks or passes through is decided per-profile
(`Profile::fail_closed`), not by this crate globally — check the profile in question rather
than assuming.

## Testing

`cargo test -p governance-redact` — no external services needed; every test here is a pure
function over strings/bytes. `blocked_verdict_never_carries_the_secret_value` (in
`engine.rs`) exists specifically to guard the rule that a blocked verdict must never carry
the entity's actual value, only its category — the metric-label version of this same rule
lives one layer up, in the two binaries that wrap this crate.
