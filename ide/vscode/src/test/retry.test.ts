// Unit tests for the retry utility.
//
// Mirrors the Rust tests in `crates/governance-copilot/src/client.rs`
// (`retry_delay_*`, `is_retryable_status_*`) so the same guarantees hold
// on both sides of the gateway boundary.
//
// No network, no `vscode` — pure function coverage.
import assert from 'node:assert/strict';
import test from 'node:test';

import { fetchWithRetry, isRetryableStatus, retryDelay } from '../retry.ts';

/**
 * Replace `globalThis.fetch` with a stub that calls `handler` per attempt.
 *
 * Returns a `restore` function whose return value is the number of fetch calls
 * made. The handler may throw (a transport error) or return a `Response`.
 */
function stubFetch(handler: (call: number) => Response): () => number {
  let calls = 0;
  const original = globalThis.fetch;
  globalThis.fetch = ((_input: unknown, _init?: RequestInit) => {
    calls++;
    return Promise.resolve(handler(calls));
  }) as typeof fetch;
  return () => {
    globalThis.fetch = original;
    return calls;
  };
}

function res(status: number, headers: Record<string, string> = {}): Response {
  return new Response('{}', { status, headers });
}

function abortError(): Error {
  return new DOMException('The operation was aborted.', 'AbortError');
}

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

test('fetchWithRetry: retries a 429 then returns the successful response', async () => {
  const calls: Response[] = [];
  const restore = stubFetch((n) => {
    const r = n === 1 ? res(429, { 'retry-after': '1' }) : res(200);
    calls.push(r);
    return r;
  });
  try {
    const out = await fetchWithRetry('https://gw/v1/models/info', {}, { retryOn: 'transient' });
    assert.equal(out.status, 200);
    assert.equal(calls.length, 2, 'expected a single retry after the 429');
  } finally {
    restore();
  }
});

test('fetchWithRetry: honours Retry-After over the computed backoff', async () => {
  // Header says 1s; the attempt-1 backoff would be 500ms. Only actually reading
  // the header can produce an elapsed time at or above 1s — passing `null`
  // instead would stay ~500ms and fail the lower bound. This is the PR's
  // headline behaviour made falsifiable (port of the Rust retry.rs test).
  const restore = stubFetch((n) => (n === 1 ? res(429, { 'retry-after': '1' }) : res(200)));
  try {
    const start = Date.now();
    await fetchWithRetry('https://gw/v1/models/info', {}, { retryOn: 'transient' });
    const elapsed = Date.now() - start;
    assert.ok(elapsed >= 950, `expected the header's 1s wait, waited ${elapsed}ms`);
    assert.ok(elapsed < 2000, `retry took implausibly long: ${elapsed}ms`);
  } finally {
    restore();
  }
});

test('fetchWithRetry: does NOT retry a 5xx under throttle-only', async () => {
  const restore = stubFetch(() => res(503));
  try {
    const out = await fetchWithRetry('https://gw/v1/chat/completions', {}, { retryOn: 'throttle-only' });
    assert.equal(out.status, 503, 'a 5xx must surface immediately on the chat path');
    assert.equal(restore(), 1, 'throttle-only must not retry a 5xx');
  } finally {
    restore();
  }
});

test('fetchWithRetry: retries a 5xx under transient (idempotent GET)', async () => {
  const restore = stubFetch((n) => (n === 1 ? res(503) : res(200)));
  try {
    const out = await fetchWithRetry('https://gw/v1/models/info', {}, { retryOn: 'transient' });
    assert.equal(out.status, 200);
    assert.equal(restore(), 2, 'transient must retry a 5xx on the catalogue path');
  } finally {
    restore();
  }
});

test('fetchWithRetry: retries a transport error under transient', async () => {
  const restore = stubFetch((n) => {
    if (n === 1) {
      throw new TypeError('fetch failed');
    }
    return res(200);
  });
  try {
    const out = await fetchWithRetry('https://gw/v1/models/info', {}, { retryOn: 'transient' });
    assert.equal(out.status, 200);
    assert.equal(restore(), 2, 'a connect/timeout-style rejection must be retried');
  } finally {
    restore();
  }
});

test('fetchWithRetry: never retries an AbortError', async () => {
  const restore = stubFetch(() => {
    throw abortError();
  });
  try {
    await assert.rejects(
      fetchWithRetry('https://gw/v1/models/info', {}, { retryOn: 'transient' }),
      (err: unknown) => (err as Error).name === 'AbortError',
    );
    assert.equal(restore(), 1, 'an AbortError must never be retried');
  } finally {
    restore();
  }
});

test('fetchWithRetry: an abort mid-back-off interrupts the sleep immediately', async () => {
  const controller = new AbortController();
  // Retry-After of 2 s is long enough that a non-interruptible sleep would
  // visibly stall; the abort at ~20 ms must cut it short and issue no attempt 2.
  const restore = stubFetch(() => res(429, { 'retry-after': '2' }));
  try {
    const start = Date.now();
    const pending = fetchWithRetry(
      'https://gw/v1/models/info',
      { signal: controller.signal },
      { retryOn: 'transient' },
    );
    setTimeout(() => controller.abort(), 20);
    await assert.rejects(
      pending,
      (err: unknown) => (err as Error).name === 'AbortError',
      'an abort during back-off must reject with AbortError',
    );
    assert.ok(Date.now() - start < 500, 'the abort must interrupt the sleep, not wait it out');
    assert.equal(restore(), 1, 'no further attempt must be issued after the abort');
  } finally {
    restore();
  }
});
