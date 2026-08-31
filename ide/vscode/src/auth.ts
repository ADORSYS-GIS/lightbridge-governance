import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

// From ./redact.js rather than ./log.js, which would pull `vscode` into this
// module and make the credential path un-testable outside an extension host.
import { errorMessage } from './redact.js';
import type { Config } from './config.js';

const run = promisify(execFile);

/** Thrown when no usable credential could be obtained. Never carries the token. */
export class NoCredentialError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'NoCredentialError';
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
    // Deliberately does not include stderr verbatim. It is a diagnostic from a
    // credential tool and is not a place to be casual about what gets echoed.
    throw new NoCredentialError(
      `'${config.governanceAuthPath} token' failed (${errorMessage(err)}). ` +
        `Run 'governance-auth login', or set lightbridge.governanceAuthPath to an absolute path.`,
    );
  }

  const token = (typeof stdout === 'string' ? stdout : stdout.toString('utf8')).trim();
  if (token === '') {
    throw new NoCredentialError(
      `'${config.governanceAuthPath} token' exited 0 but printed nothing. Run 'governance-auth login'.`,
    );
  }

  return token;
}

/**
 * Whether a credential is currently obtainable.
 *
 * Used by the silent path of `provideLanguageModelChatInformation`, which must
 * not prompt. Swallows the reason on purpose — the caller only gets to decide
 * whether to show models, and a boolean is the whole of that decision.
 */
export async function hasCredential(config: Config): Promise<boolean> {
  try {
    await getToken(config);
    return true;
  } catch {
    return false;
  }
}
