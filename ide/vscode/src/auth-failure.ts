/**
 * Why `governance-auth token` refused, and what to tell the developer about it.
 *
 * Deliberately free of any `vscode` import, for the same reason `redact.ts` is:
 * this is the code that decides what a signed-out developer sees, and a
 * decision that can only be exercised by launching an editor is one that stops
 * being exercised.
 *
 * The classification reads stderr but **never renders it**. `governance-auth`
 * is a credential tool; its diagnostics are not a place to be casual about
 * what gets echoed into an output channel or a chat transcript. Matching a
 * marker and emitting our own prose keeps that boundary — see `failure()` in
 * ./auth.ts for the other half of it, and the stderr-containment scenario in
 * tests/integration.ts for what holds it in place.
 */

/**
 * The distinct things that can go wrong, kept distinct because they need
 * different fixes. "You are logged out" and "the binary isn't installed" both
 * present as a non-zero exit, and telling a developer to run
 * `governance-auth login` when the shell cannot find `governance-auth` sends
 * them round a loop that never terminates.
 */
export type AuthFailureKind =
  | 'signed-out'
  | 'refresh-failed'
  | 'not-configured'
  | 'binary-not-found'
  | 'binary-not-executable'
  | 'timed-out'
  | 'cancelled'
  | 'empty-output'
  | 'unknown';

/** A button we are willing to offer next to a failure. */
export type AuthAction = 'sign-in' | 'open-settings' | 'show-log';

export interface AuthFailureAdvice {
  /** One line. The notification body and the status-bar tooltip head. */
  readonly title: string;
  /**
   * The full explanation, shown where the developer hit the wall. It has to
   * carry three things every time: what happened, that their code is not at
   * fault, and the exact command that fixes it.
   */
  readonly detail: string;
  /** The command that fixes it, ready to run in a terminal. */
  readonly fixCommand?: string | undefined;
  /**
   * Whether `fixCommand` may be executed on the developer's behalf. False when
   * it carries placeholders they have to fill in first — typing a command with
   * `<your-issuer-url>` in it into a live shell is not a fix, it is an error
   * message with extra steps.
   */
  readonly runnable: boolean;
  readonly actions: readonly AuthAction[];
}

/** The subset of a Node `ExecFileException` this module reads. */
interface ExecFailure {
  readonly name?: string;
  readonly code?: string | number;
  readonly killed?: boolean;
  readonly stderr?: string | Buffer;
}

/**
 * Markers matched against `governance-auth`'s stderr, most specific first.
 *
 * Order is load-bearing and not cosmetic. The missing-configuration message
 * *contains* a pasteable `governance-auth login --issuer …` line, and the
 * refresh failure ends with "run `governance-auth login` again if this
 * persists" — so a bare /governance-auth login/ test would claim both are
 * "signed out" and send the developer to a command that cannot help them. The
 * generic marker is therefore last, and only catches what the specific ones
 * did not.
 */
const MARKERS: ReadonlyArray<readonly [RegExp, AuthFailureKind]> = [
  [/--issuer|--client-id|GOVERNANCE_AUTH_ISSUER|GOVERNANCE_AUTH_CLIENT_ID/, 'not-configured'],
  [/no cached session|has no refresh token/i, 'signed-out'],
  [/refreshing the access token|token exchange failed/i, 'refresh-failed'],
  [/governance-auth login/i, 'signed-out'],
];

/**
 * Reduce a thrown value from `execFile` to one of the kinds above.
 *
 * Unrecognised routes to `unknown`, never to `signed-out`. Guessing "signed
 * out" from an unreadable failure would tell a developer to run a login that
 * succeeds and changes nothing, which is worse than admitting we do not know.
 */
export function classifyAuthFailure(err: unknown): AuthFailureKind {
  if (typeof err !== 'object' || err === null) {
    return 'unknown';
  }
  const failure = err as ExecFailure;

  // Before the `killed` check: an aborted spawn is also a killed one, and a
  // cancellation is not a failure to report.
  if (failure.name === 'AbortError' || failure.code === 'ABORT_ERR') {
    return 'cancelled';
  }
  if (failure.code === 'ENOENT') {
    return 'binary-not-found';
  }
  if (failure.code === 'EACCES' || failure.code === 'EPERM') {
    return 'binary-not-executable';
  }
  if (failure.killed === true || failure.code === 'ETIMEDOUT') {
    return 'timed-out';
  }

  const stderr = typeof failure.stderr === 'string' ? failure.stderr : (failure.stderr?.toString('utf8') ?? '');
  for (const [pattern, kind] of MARKERS) {
    if (pattern.test(stderr)) {
      return kind;
    }
  }
  return 'unknown';
}

const NOT_YOUR_CODE = 'This is a sign-in problem on this machine, not a fault in your code or workspace.';

/**
 * What to show for a kind.
 *
 * `authPath` is the configured `lightbridge.governanceAuthPath` — a setting the
 * developer wrote, never a secret, so it is safe to name. Nothing else from the
 * failure reaches this text.
 */
export function authFailureAdvice(kind: AuthFailureKind, authPath: string): AuthFailureAdvice {
  const login = `${authPath} login`;

  switch (kind) {
    case 'signed-out':
      return {
        title: 'Lightbridge: you are signed out.',
        detail:
          `There is no valid \`governance-auth\` session, so no request can be sent to the ` +
          `governed gateway. ${NOT_YOUR_CODE} Run \`${login}\` in a terminal to sign in, then ` +
          `try again.`,
        fixCommand: login,
        runnable: true,
        actions: ['sign-in', 'show-log'],
      };

    case 'refresh-failed':
      return {
        title: 'Lightbridge: your session could not be refreshed.',
        detail:
          `\`governance-auth\` holds a session but could not renew it — the identity provider ` +
          `was unreachable, or the session was revoked. ${NOT_YOUR_CODE} Check your network, ` +
          `then run \`${login}\` to sign in again.`,
        fixCommand: login,
        runnable: true,
        actions: ['sign-in', 'show-log'],
      };

    case 'not-configured':
      return {
        title: 'Lightbridge: governance-auth has no issuer or client id.',
        detail:
          `\`governance-auth\` is installed but does not know which identity provider to use, ` +
          `so it cannot sign you in. ${NOT_YOUR_CODE} Run \`${login} --issuer <your-issuer-url> ` +
          `--client-id <your-client-id>\` once; it writes both to your config file and later ` +
          `commands need no flags.`,
        fixCommand: `${login} --issuer <your-issuer-url> --client-id <your-client-id>`,
        // Placeholders: put it in the terminal, do not press Enter on it.
        runnable: false,
        actions: ['sign-in', 'show-log'],
      };

    case 'binary-not-found':
      return {
        title: `Lightbridge: '${authPath}' was not found.`,
        detail:
          `The \`governance-auth\` binary could not be started at \`${authPath}\`. ` +
          `${NOT_YOUR_CODE} VS Code spawns it **without a shell**, so a desktop- or snap-launched ` +
          `editor often does not have \`~/.local/bin\` on \`PATH\` even though your terminal ` +
          `does. Set \`lightbridge.governanceAuthPath\` to an absolute path, or install ` +
          `\`governance-auth\` if it is missing.`,
        runnable: false,
        actions: ['open-settings', 'show-log'],
      };

    case 'binary-not-executable':
      return {
        title: `Lightbridge: '${authPath}' is not executable.`,
        detail:
          `\`${authPath}\` exists but this editor may not execute it. ${NOT_YOUR_CODE} ` +
          `Run \`chmod +x ${authPath}\`, or point \`lightbridge.governanceAuthPath\` at an ` +
          `installed copy.`,
        fixCommand: `chmod +x ${authPath}`,
        runnable: false,
        actions: ['open-settings', 'show-log'],
      };

    case 'timed-out':
      return {
        title: 'Lightbridge: governance-auth did not respond.',
        detail:
          `\`${authPath} token\` was still running after 15 seconds and was stopped. ` +
          `${NOT_YOUR_CODE} It usually means a token refresh is waiting on an unreachable ` +
          `identity provider. Try again, and run \`${login}\` if it keeps happening.`,
        fixCommand: login,
        runnable: true,
        actions: ['sign-in', 'show-log'],
      };

    case 'cancelled':
      return {
        title: 'Lightbridge: the request was cancelled.',
        detail: 'The credential lookup was cancelled before it finished.',
        runnable: false,
        actions: [],
      };

    case 'empty-output':
      return {
        title: 'Lightbridge: governance-auth returned no token.',
        detail:
          `\`${authPath} token\` reported success but printed no token, which breaks its own ` +
          `contract. ${NOT_YOUR_CODE} Run \`${login}\` to re-establish the session; if it ` +
          `recurs, this is a \`governance-auth\` bug worth filing.`,
        fixCommand: login,
        runnable: true,
        actions: ['sign-in', 'show-log'],
      };

    case 'unknown':
      return {
        title: 'Lightbridge: could not obtain a credential.',
        detail:
          `\`${authPath} token\` failed for a reason this extension does not recognise. ` +
          `${NOT_YOUR_CODE} The Lightbridge output channel records the exit status; run ` +
          `\`${authPath} token\` in a terminal to see its own diagnostic, which is deliberately ` +
          `not repeated here because it comes from a credential tool.`,
        runnable: false,
        actions: ['show-log'],
      };
  }
}
