# ADR-0010: Bidirectional scanning — request and response paths

- Status: Proposed
- Date: 2026-08-04
- Decision owners: @stephane-segning

## Implementation Status

As of 2026-08-04:
- ✅ Request path scanning: Both `redact-gateway` and `redact-extproc` scan all request bodies
  synchronously before forwarding.
- ✅ Response path scanning (`redact-gateway`): incremental `SseHoldBack` replaces the buffered
  `scan_sse` path, enabling bounded-memory streaming with time-to-first-token sub-linear in
  response duration.
- ✅ Metrics: Both paths report to the same Prometheus metrics (`redact_redactions_total`,
  `redact_scanned_fields_total`, etc.).
- ⏳ Response path scanning (`redact-extproc`): Envoy's streamed mode for response bodies is
  not yet wired to `SseHoldBack`. That change is tracked separately.

## Context

The redaction module (`governance-redact`, deployed as `redact-gateway` and `redact-extproc`) sits in
the AI request path to prevent sensitive data from leaving the boundary. Two directions must be
handled:

1. **Request path** — the user's prompt. Leaking a credential here means it reaches the LLM,
   which may log it, use it, or reflect it in a future response.

2. **Response path** — the LLM's completion. The model may surface training-data material (PII
   that appeared in its corpus), output a code suggestion that contains a leaked key, or generate
   documentation that includes names or identifiers from an earlier prompt.

The previous architecture (`censgate/redact`) only scanned the request. A leaked credential in a
prompt was stopped; a leaked SSN in the response was not. Both directions must be covered for the
guarantee to mean anything.

## Decision

**Both the request and the response are scanned before they cross the trust boundary.**

The scan happens on both paths, in both deployment shapes (`redact-gateway` and `redact-extproc`),
using the same `governance-redact` engine and the same profile policy. A blocked entity on either
path stops the exchange — it never completes, and nothing is forwarded past the scanner.

### Request path

The user sends a prompt (JSON body, OpenAI chat-completions shape). The scanner walks every
`content` field it finds and applies the profile's action:

| Action | What happens |
|--------|-------------|
| `Block` | The request is refused immediately (HTTP 422). The LLM **never receives it**. |
| `Replace` | The span is replaced with a label (e.g. `<EMAIL>`). The body is rewritten and forwarded. |
| `Mask` | The span is masked with a suffix (e.g. `123-45-6789` → `****6789`). |
| `Hash` | The span is replaced with a salted SHA-256 digest, consistent across a conversation. |
| `Allow` | The span is left untouched. |

The rewrite is applied in-place before the (possibly rewritten) body is forwarded. The LLM
receives the rewritten version — it never sees the original. A `Block` on a credential means the
entire request is refused; the LLM is blind to the request.

`redact-gateway` (buffered): the entire JSON body is read synchronously, parsed, and walked in one
pass before the upstream request is sent.

`redact-extproc` (Buffered mode): Envoy hands the entire `RequestBody` message at once (one message
covering the whole payload). Identical logic to `redact-gateway`'s request path.

### Response path

After the LLM streams back its completion, every byte is scanned before it reaches the client. This
is symmetric with the request path — a compromised LLM returning training-data PII is the same
failure class as a user putting a credential in a prompt.

The response path is harder because it is **streaming**, not batch. The model emits tokens as they
are generated. Scanning the complete response before releasing any byte would mean
time-to-first-token = time-to-last-token (20-second stream → 20-second wait before the client sees
anything). For the request path, buffering is fine (the request body is small and arrives at once).
For the response path, buffering a multi-megabyte completion before forwarding it is both slow
and memory-intensive.

The solution is **incremental scanning with a bounded hold-back window** (`SseHoldBack`):

1. Chunks arrive from the LLM as SSE frames (`data: {"choices":[{"delta":{"content":"..."}}]}`).
2. Each chunk's `delta.content` is fed into the scanner.
3. A bounded buffer ("hold-back window", 4 KB — see `DEFAULT_WINDOW` in
   `crates/governance-redact/src/holdback.rs`) accumulates unscanned text.
4. Once the buffer exceeds the window, the scanner checks the oldest part:
   - If no entity spans the buffer boundary → safe prefix is released, written into the SSE frame,
     and forwarded to the client.
   - If an entity does span the boundary → the stream is **blocked** at the hold-back point,
     the client receives an HTTP 200 SSE error event (`data: {"error":...}`), and the upstream
     connection is aborted. (A trailing-blocked entity is caught at `flush`.)
5. Any frame that carries no redactable content (structural SSE lines, `[DONE]`, role-only chunks)
   passes through immediately.
6. Frames are released in **strict arrival order** — a ready frame behind a held one waits.

The window (4 KB) exceeds our longest `Action::Block` entity (a PKCS#8 RSA 4096 key at ~3,300
bytes), so no credential subject to `Block` can straddle the boundary and have its safe prefix
released before detection fires. Raising the window further is possible but increases worst-case
output lag (see Consequences).

This gives three properties simultaneously:

- **Bounded memory**: only the hold-back window (4 KB) is retained per concurrent stream, not the
  whole response. Ten concurrent 100 MB streams use ~40 KB, not 1 GB.
- **Real-time streaming**: first token delay is the window fill time (~4 KB / token-arrival-rate),
  not the full stream duration.
- **No blind spots at token boundaries**: entities whose characters split across SSE chunk
  boundaries are reassembled by the UTF-8 carry decoder before the scanner sees them, so a
  partial multi-byte character never causes a misdecode.

`redact-gateway` feeds chunks from `reqwest`'s response stream to `SseHoldBack`. `redact-extproc`
streams response chunks via Envoy's `processingMode.response.body: Streamed` mode (not yet wired
to `SseHoldBack` — tracked separately).

### Entities covered (coding-assistant profile)

| Entity | Default action |
|--------|--------------|
| Email address | Replace → `<EMAIL>` |
| Phone number | Replace → `<PHONE>` |
| SSN | Mask → `****-**-6789` (last 4) |
| Credit card | Mask → `************6789` (last 4) |
| API key / secret | **Block** → request refused |
| Private key | **Block** → request refused |

Credentials are `Block`, not redacted — a leaked key should not reach the LLM at all, and a LLM
response containing a leaked key must not reach the client.

### Profile decisions

The redaction profile is configured at startup and is shared by both paths. An unknown profile
name is **rejected at startup**, never silently resolved to a weaker default — silently falling
back is the exact failure this service exists to prevent.

### Fail-closed failure model

On every profile except `observe-only`, any failure in the scanning path refuses the exchange:

| What went wrong | What the client sees | What the LLM sees |
|----------------|---------------------|-------------------|
| Request body unparseable | HTTP 400 | — |
| Request scan error | HTTP 502 | — |
| Credential found (Block) | HTTP 422 content_blocked | Nothing — request was refused first |
| Non-2xx upstream response | Upstream's error body, passed through | — |
| Response body unparseable | HTTP 502 | — |
| Response scan error | HTTP 502 | — |
| Response block (prohibited content, non-observe profile) | HTTP 200 + `data:{"error":{"code":"content_blocked",...}}` SSE event | LLM stopped mid-stream |
| Response block (`observe-only` profile) | Content forwarded; block logged | LLM continues; nothing blocked |

Note on response path: HTTP headers are committed before SSE streaming begins, so the status code cannot be changed after a block is detected mid-stream. The streaming gateway delivers the block signal inside the SSE body as an OpenAI-shaped error event — clients that handle the `error` field in SSE `data:` frames receive it unambiguously. `redact-extproc`'s buffered path can return HTTP 422 as in the table.

`observe-only` is the documented exception: an indeterminate result is logged and the exchange
continues, because the profile makes no promises in exchange.

## Consequences

**Positive**

- Entire request/response surface is scanned, request and response.
- Credentials are prevented at the gate — the LLM never receives a blocked payload.
- LLM-sourced PII (training data leakage, model memorization) is caught on output.
- Bounded memory per concurrent stream (hold-back window), enabling scale without OOM risk.
- Real-time streaming latency preserved by incremental scan-and-release.
- Both paths log to the same Prometheus metrics, giving a unified view of the redaction surface.

**Negative**

- `Person` / `Location` entities (NER) are not yet active — `pii`'s `candle-ner` feature ships a
  trait, not a model. All name detection falls back to pattern recognizers only.
- A `Block` on the response path stops the stream mid-completion. The user's request was
  processed; they get a partial response or an error. This is the right behavior (fail closed)
  but disruptive for sessions that otherwise completed normally.
- Request scanning adds latency to the upstream round-trip (one synchronous parse-and-walk pass
  before forwarding). At `~1 ms / KB` scanning speed this is negligible for typical prompt sizes
  (a few KB) but scales with the prompt.

**Neutral / follow-ups**

- Profile migration: the profile is configured at deployment (Helm values), not per-request.
  Tenant-specific profiles (one team uses `observe-only`, another uses `coding-assistant`) require
  routing logic upstream of this module.
- `observe-only` on the response path forwards content even when `Block` would fire on another
  profile — the block is logged but the SSE stream continues. This enables kill-switch-free
  monitoring but means the block signal is in-band (inside the stream), not in the HTTP status.
- Streaming scan latency: the hold-back window introduces a ceiling-delay proportional to the
  window size divided by the token arrival rate. At normal streaming speeds (~30 tokens/second),
  a 4 KB window fills in ~4 KB / (30 tokens × ~7 bytes/token) ≈ 19 seconds — a deliberate
  trade-off, so monitor this in production.
- Multi-choice (`n > 1`) traffic has not been exercised at scale on this platform.
  (`SseHoldBack` handles it, but interleaving is unvalidated.)
- No per-tenant or per-user policy today.

## Alternatives considered

- **Scan requests only, pass responses through** — rejected. A leaked SSN in the response is
  the same data-leak failure as a leaked API key in the prompt. The guarantee is only meaningful
  bidirectional, and a platform that scans prompts but not completions cannot claim to protect
  the data boundary.

- **Buffer entire response, then scan (current redact-gateway approach)** — accepted as the
  safe default while incremental scanning is validated. Confirmed safe: no entity can hide in
  a token split because the whole stream is available before any byte is forwarded. Cost:
  time-to-first-token = time-to-last-token, which is poor UX for long completions. `scan_sse`
  (buffered) is the conservative default; `SseHoldBack` (incremental) is the production target.

- **Buffer entire response, rewrite, then stream back** — same as above but re-emits the buffered
  body after scan. Same latency problem. `SseHoldBack` solves this by releasing safe text as soon
  as the window confirms it.

- **Scan on the client side (JavaScript intercept before send / after receive)** — rejected.
  Client-side scanning is in the attacker's trust domain. It can be removed, manipulated, or
  bypassed. The scanning must live server-side, in the infrastructure this team controls.

- **Let the LLM provider handle PII redaction** — rejected. The provider may not have a policy
  equivalent to `coding-assistant`, and does not have the same data-isolation guarantees as the
  cluster. The request path is ours to control.

## Related

- RFC: `docs/rfc/0003-governance-redact-module.md` (proposed)
- Code: `crates/governance-redact/src/lib.rs`
- Code: `crates/governance-redact/src/payload.rs` (`scan_request`, `scan_response`)
- Code: `crates/governance-redact/src/sse.rs` (`SseHoldBack`, incremental response scanning)
- Code: `crates/governance-redact/src/streaming.rs` (`scan_sse`, buffered response scanning)
- Code: `crates/governance-redact/src/holdback.rs` (`HoldBack`, raw byte hold-back)
- Code: `crates/governance-redact/src/engine.rs` (`Engine::scan`, core scanning API)
- Code: `crates/governance-redact/src/profile.rs` (policy profiles and entity actions)
- Code: `app/redact-gateway/src/proxy.rs` (proxy request + response handler)
- Code: `app/redact-extproc/src/service.rs` (ext_proc request + response handler)
- Deployment: `charts/redact-gateway/values.yaml`
- ai-helm ADR (platform counterpart, networking in the Envoy/EasyFusion cluster)