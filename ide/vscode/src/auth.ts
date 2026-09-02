import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

// From ./auth-failure.js and ./auth-state.js rather than ./log.js, which would
// pull `vscode` into this module and make the credential path un-testable
// outside an extension host.
import { authFailureAdvice, classifyAuthFailure } from './auth-failure.js';
import { AUTH_STATE_TTL_MS, cachedAuthState, recordAuthOutcome } from './auth-state.js';
import type { AuthFailureKind } from './auth-failure.js';
import type { AuthState } from './auth-state.js';
import type { Config } from './config.js';

const run = promisify(execFile);

/**
 * Thrown when no usable credential could be obtained. Never carries the token,
 * and — see `failure()` below — never carries `governance-auth`'s own stderr
 * either. `kind` is what callers branch on: an editor cannot offer a
 * useful next step from a string.
 */
export class NoCredentialError extends Error {
  readonly kind: AuthFailureKind;

  constructor(kind: AuthFailureKind, message: string) {
    super(message);
    this.name = 'NoCredentialError';
    this.kind = kind;
  }
}

/**
 * Get a currently-valid access token by shelling out to `governance-auth token`.
 *
 * This extension deliberately implements no OAuth of its own. `governance-auth`
 * already runs authorization_code + PKCE against the IdP, holds the refresh
 * token at `0600`, takes a per-(issuer, client-id) lock so concurrent callers
 * do not both refresh, and refreshes inside the expiry skew (ADR-0010,
 * ADR-0012). A second credential path in TypeScript would be a second thing to
 * get wrong, and it would be the one holding the long-lived secret.
 *
 * Its contract is what makes it safe to depend on: one line on stdout, non-zero
 * exit with nothing on stdout when the session is unusable, and no browser
 * launch ever. It never emits a stale credential.
 *
 * Fail-closed, and this is the load-bearing property of the whole extension:
 * every path out of here is either a token or a throw. There is no branch that
 * returns `undefined` and lets the caller proceed unauthenticated, because that
 * is how an outage becomes an authorization bypass — the gateway would see an
 * anonymous request, and anonymous is precisely the posture this product exists
 * to eliminate.
 *
 * Both outcomes are recorded in the auth-state cache. That is deliberate: it
 * means the cheap answer to "are we signed in?" is a by-product of work the
 * extension was doing anyway, rather than an extra process spawn.
 */
export async function getToken(config: Config, signal?: AbortSignal): Promise<string> {
  // `execFile`'s promisified signature widens stdout to string | Buffer once
  // options are passed, so the encoding is normalized below rather than assumed.
  let stdout: string | Buffer;

  try {
    const opts: Parameters<typeof run>[2] = {
      timeout: 15_000,
      windowsHide: true,
      maxBuffer: 1024 * 64,
      ...(signal ? { signal } : {}),
    };
    ({ stdout } = await run(config.governanceAuthPath, ['token'], opts));
  } catch (err) {
    throw failure(config, classifyAuthFailure(err));
  }

  const token = (typeof stdout === 'string' ? stdout : stdout.toString('utf8')).trim();
  if (token === '') {
    throw failure(config, 'empty-output');
  }

  recordAuthOutcome(true);
  return token;
}

/**
 * Build the error, and record the state, from a classified kind.
 *
 * The message is **ours**: the `title` this extension wrote for that kind, not
 * anything Node or `governance-auth` produced. That is a change from the first
 * version of this file, whose comment claimed stderr was excluded while
 * `errorMessage(err)` embedded it — Node's `execFile` builds its message as
 * `Command failed: <argv>\n<stderr>`, so every byte the credential tool printed
 * was reaching the output channel and, on the chat path, the transcript.
 */
function failure(config: Config, kind: AuthFailureKind): NoCredentialError {
  recordAuthOutcome(false, kind);
  return new NoCredentialError(kind, authFailureAdvice(kind, config.governanceAuthPath).title);
}

/**
 * The extension's current belief about whether it is signed in.
 *
 * Answers from the cache when it can and spawns `governance-auth token` only
 * when it cannot. Callers get the reason as well as the boolean, because every
 * caller here has a UI affordance to fill in and "false" alone cannot say
 * whether the fix is a login or a `PATH`.
 *
 * The probe throws nothing: an unusable session is an answer, not an error.
 */
export async function resolveAuthState(
  config: Config,
  maxAgeMs: number = AUTH_STATE_TTL_MS,
): Promise<AuthState> {
  const cached = cachedAuthState(maxAgeMs);
  if (cached !== undefined) {
    return cached;
  }

  try {
    await getToken(config);
    return recordAuthOutcome(true);
  } catch (err) {
    return recordAuthOutcome(
      false,
      err instanceof NoCredentialError ? err.kind : classifyAuthFailure(err),
    );
  }
}
