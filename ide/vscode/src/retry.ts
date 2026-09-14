/**
 * Bounded exponential back-off with `Retry-After` preference.
 *
 * Ported from `crates/governance-copilot/src/client.rs` (`retry_delay`,
 * `is_retryable_status`, `send_with_retry`). The shape is intentionally
 * the same: a 429 is the gateway answering clearly — "ask again later" —
 * and the caller's job is to honour that, not to treat it as a hard failure.
 *
 * No `vscode` import. Kept vscode-free so unit tests can import it without
 * the stub; a logger is injected by callers for the same reason.
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
 * Which transient conditions warrant a retry for a given call site.
 *
 * `'transient'` (the catalogue, an idempotent `GET`) retries `429`, `5xx` and
 * transport errors. `'throttle-only'` (the chat `POST`) retries `429` only —
 * a retried `5xx` or transport failure could re-send a request the gateway
 * already accepted and billed, duplicating a completion and its audit record.
 */
export type RetryMode = 'transient' | 'throttle-only';

/** Details passed to `onRetry` before each sleep, for a logged retry trail. */
export interface RetryAttempt {
  readonly attempt: number;
  readonly delayMs: number;
  readonly status: number | undefined;
  readonly error: unknown;
}

/** Call-site policy and observability hooks for `fetchWithRetry`. */
export interface FetchWithRetryOptions {
  /** Defaults to `MAX_ATTEMPTS` (3). Useful for tests. */
  readonly maxAttempts?: number;
  /** Which conditions to retry. Defaults to `'transient'`. */
  readonly retryOn?: RetryMode;
  /** Called before each retry sleep. Lets a caller surface a retry trail. */
  readonly onRetry?: (attempt: RetryAttempt) => void;
}

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
 * Retries honouring the server's `Retry-After` header over the computed
 * backoff when present. What is retried is controlled by `retryOn` (see
 * `RetryMode`): `'throttle-only'` for the non-idempotent chat `POST`, and
 * `'transient'` (the default, for the idempotent catalogue `GET`) which also
 * retries `5xx` and transport errors. Returns the final `Response` (which the
 * caller checks for `ok`); deterministic failures are returned immediately —
 * retrying them cannot succeed.
 *
 * @param url   Request URL.
 * @param init  `RequestInit` passed to `fetch` on every attempt.
 * @param options Retry policy and observability hooks.
 */
export async function fetchWithRetry(
  url: string,
  init: RequestInit,
  options: FetchWithRetryOptions = {},
): Promise<Response> {
  const maxAttempts = options.maxAttempts ?? MAX_ATTEMPTS;
  const retryOn = options.retryOn ?? 'transient';
  let attempt = 1;

  for (;;) {
    const outcome = await attemptOnce(url, init);

    if (outcome.kind === 'response') {
      const res = outcome.response;
      // `isRetryableStatus` is the single source of truth for "is this status
      // transient" — the exported predicate the unit tests pin. The mode is
      // layered on top: a 429 is retried under either mode, a 5xx only under
      // 'transient'.
      const retryable =
        isRetryableStatus(res.status) &&
        (res.status === 429 || retryOn === 'transient') &&
        attempt < maxAttempts;
      if (!retryable) {
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
      options.onRetry?.({ attempt, delayMs: delay, status: res.status, error: undefined });
      await sleep(delay, init.signal);
      attempt++;
      continue;
    }

    // A rejected fetch: a connection that never established or a request that
    // timed out. Retry only under 'transient' — an `AbortError` is a decision
    // (user cancel or request timeout), not a transient fault, and must never
    // be retried.
    const err = outcome.error;
    if (retryOn !== 'transient' || isAbortError(err) || attempt >= maxAttempts) {
      throw err;
    }
    const delay = retryDelay(attempt, null);
    options.onRetry?.({ attempt, delayMs: delay, status: undefined, error: err });
    await sleep(delay, init.signal);
    attempt++;
  }
}

type AttemptResult = { readonly kind: 'response'; readonly response: Response } | { readonly kind: 'error'; readonly error: unknown };

async function attemptOnce(url: string, init: RequestInit): Promise<AttemptResult> {
  try {
    return { kind: 'response', response: await fetch(url, init) };
  } catch (err) {
    return { kind: 'error', error: err };
  }
}

function isAbortError(err: unknown): boolean {
  return (err as Error | undefined)?.name === 'AbortError';
}

/**
 * Promisified `setTimeout` that also resolves the moment `signal` aborts.
 *
 * Without the signal link, an abort requested during a back-off would only
 * take effect on the *next* `fetch`, so cancellation and `requestTimeoutMs`
 * would not actually bound retry time. Racing the wait against the signal
 * makes an abort interrupt the sleep immediately.
 */
function sleep(ms: number, signal?: AbortSignal | null): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(signal?.reason ?? new Error('The operation was aborted.'));
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}
