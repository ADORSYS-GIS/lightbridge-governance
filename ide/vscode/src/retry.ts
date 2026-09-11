/**
 * Bounded exponential back-off with `Retry-After` preference.
 *
 * Ported from `crates/governance-copilot/src/client.rs` (`retry_delay`,
 * `is_retryable_status`, `send_with_retry`). The shape is intentionally
 * the same: a 429 is the gateway answering clearly — "ask again later" —
 * and the caller's job is to honour that, not to treat it as a hard failure.
 *
 * No `vscode` import. Kept vscode-free so unit tests can import it without
 * the stub.
 */

/** Backoff base for a transient failure that carried no `Retry-After` hint. */
const RETRY_BASE_DELAY_MS = 500;

/**
 * Upper bound on any computed delay.
 *
 * The gateway's `Retry-After` is capped here too: a secondary-rate-limit hint
 * of 3600s would stall the extension for an hour.
 */
const RETRY_MAX_DELAY_MS = 30_000;

/** Maximum number of send attempts. First attempt + 2 retries. */
const MAX_ATTEMPTS = 3;

/**
 * The delay in milliseconds before the next attempt.
 *
 * Uses the `Retry-After` header value (integer seconds) when the response
 * carried one, else exponential backoff from `attempt` (1-based).
 * Both are capped at `RETRY_MAX_DELAY_MS`.
 *
 * Mirrors `retry_delay()` in `crates/governance-copilot/src/client.rs`.
 */
export function retryDelay(attempt: number, retryAfterHeader?: string | null): number {
  // An empty or whitespace-only header is a *missing* hint, not a 0-second
  // one: Number('') is 0 and would otherwise pass the integer check, making
  // retries fire back-to-back with no delay at all.
  if (retryAfterHeader != null && retryAfterHeader.trim() !== '') {
    const secs = Number(retryAfterHeader.trim());
    if (Number.isInteger(secs) && secs >= 0) {
      return Math.min(secs * 1000, RETRY_MAX_DELAY_MS);
    }
  }
  // 500ms * 2^(attempt-1), capped.
  return Math.min(RETRY_BASE_DELAY_MS * Math.pow(2, attempt - 1), RETRY_MAX_DELAY_MS);
}

/**
 * Whether this HTTP status warrants a retry.
 *
 * `429` (rate limited) and `5xx` (server-side transient) are retryable.
 * Deterministic failures (`401`, `403`, `404`, etc.) are not — retrying
 * them burns rate limit budget without any hope of success.
 *
 * Mirrors `is_retryable_status()` in `crates/governance-copilot/src/client.rs`.
 */
export function isRetryableStatus(status: number): boolean {
  return status === 429 || (status >= 500 && status < 600);
}

/**
 * Fetch with bounded retry on transient failures.
 *
 * Retries on `429` and `5xx`, honouring the server's `Retry-After` header
 * over the computed backoff when present. Returns the final `Response`
 * (which the caller checks for `ok`). Deterministic failures are returned
 * immediately — retrying them cannot succeed.
 *
 * @param url        Request URL.
 * @param init       `RequestInit` passed to `fetch` on every attempt.
 * @param maxAttempts Defaults to `MAX_ATTEMPTS` (3). Useful for tests.
 */
export async function fetchWithRetry(
  url: string,
  init: RequestInit,
  maxAttempts = MAX_ATTEMPTS,
): Promise<Response> {
  let attempt = 1;
  for (;;) {
    const res = await fetch(url, init);
    if (!isRetryableStatus(res.status) || attempt >= maxAttempts) {
      return res;
    }
    // Release the abandoned response's body before sleeping: an unread body
    // keeps its socket pinned in undici, degrading connection reuse across
    // retries. Headers stay readable after cancel.
    try {
      await res.body?.cancel();
    } catch {
      // Body already consumed or errored — nothing to release.
    }
    const delay = retryDelay(attempt, res.headers.get('Retry-After'));
    await sleep(delay);
    attempt++;
  }
}

/** Promisified `setTimeout`. Extracted so tests can stub it. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
