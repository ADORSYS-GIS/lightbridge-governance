// Unit tests for the retry utility.
//
// Mirrors the Rust tests in `crates/governance-copilot/src/client.rs`
// (`retry_delay_*`, `is_retryable_status_*`) so the same guarantees hold
// on both sides of the gateway boundary.
//
// No network, no `vscode` — pure function coverage.
import assert from 'node:assert/strict';
import test from 'node:test';

import { isRetryableStatus, retryDelay } from '../retry.ts';

test('retryDelay: backs off exponentially without a Retry-After header', () => {
  // 500ms * 2^0, 500ms * 2^1, 500ms * 2^2
  assert.equal(retryDelay(1), 500);
  assert.equal(retryDelay(2), 1_000);
  assert.equal(retryDelay(3), 2_000);
  assert.equal(retryDelay(4), 4_000);
});

test('retryDelay: is capped at 30 s', () => {
  // A huge attempt number must not exceed the cap.
  assert.equal(retryDelay(100), 30_000);
  assert.equal(retryDelay(100, null), 30_000);
});

test('retryDelay: prefers Retry-After header over backoff', () => {
  assert.equal(retryDelay(1, '2'), 2_000);
  // Even at attempt 1 (backoff = 500ms) the server hint wins.
  assert.equal(retryDelay(1, '10'), 10_000);
});

test('retryDelay: caps a large Retry-After header', () => {
  // 3600s server hint → 30s cap.
  assert.equal(retryDelay(1, '3600'), 30_000);
});

test('retryDelay: ignores an unparseable Retry-After header', () => {
  // HTTP-date form, not delta-seconds — fall through to backoff.
  assert.equal(retryDelay(1, 'Wed, 21 Oct 2026 07:28:00 GMT'), 500);
  // Floating-point is not an integer — fall through.
  assert.equal(retryDelay(1, '1.5'), 500);
  // Empty string — fall through.
  assert.equal(retryDelay(1, ''), 500);
});

test('retryDelay: treats Retry-After of 0 as zero delay', () => {
  // "0" is a valid delta-seconds value meaning "retry now".
  assert.equal(retryDelay(1, '0'), 0);
});

test('isRetryableStatus: covers 429 and all 5xx', () => {
  assert.equal(isRetryableStatus(429), true, '429 must be retryable');
  assert.equal(isRetryableStatus(500), true, '500 must be retryable');
  assert.equal(isRetryableStatus(502), true, '502 must be retryable');
  assert.equal(isRetryableStatus(503), true, '503 must be retryable');
  assert.equal(isRetryableStatus(599), true, '599 must be retryable');
});

test('isRetryableStatus: does NOT cover deterministic failures', () => {
  // Retrying 401/403/404 cannot succeed and burns rate limit (AGENTS.md).
  assert.equal(isRetryableStatus(200), false);
  assert.equal(isRetryableStatus(400), false);
  assert.equal(isRetryableStatus(401), false);
  assert.equal(isRetryableStatus(403), false);
  assert.equal(isRetryableStatus(404), false);
  assert.equal(isRetryableStatus(422), false);
});
