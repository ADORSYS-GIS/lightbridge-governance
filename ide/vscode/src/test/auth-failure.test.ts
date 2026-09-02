import assert from 'node:assert/strict';
import test from 'node:test';

import { authFailureAdvice, classifyAuthFailure } from '../auth-failure.ts';

/**
 * The stderr fixtures below are copied from `governance-auth`'s own strings —
 * `app/governance-auth/src/oauth/mod.rs` and `src/config.rs` — because the
 * whole classification rests on them. If one of those messages is reworded,
 * these are what notice.
 */

/** Shaped like the error Node's promisified `execFile` rejects with. */
function execError(fields: Record<string, unknown>): Error {
  return Object.assign(new Error('Command failed'), fields);
}

test('a non-zero exit with no cached session is "signed out"', () => {
  const kind = classifyAuthFailure(
    execError({
      code: 1,
      stderr: 'Error: no cached session for this issuer/client; run `governance-auth login` first\n',
    }),
  );
  assert.equal(kind, 'signed-out');
});

test('a cached session with no refresh token is "signed out"', () => {
  const kind = classifyAuthFailure(
    execError({ code: 1, stderr: 'cached session has no refresh token; run `governance-auth login` again' }),
  );
  assert.equal(kind, 'signed-out');
});

test('a failed refresh is NOT reported as signed out', () => {
  // Its message ends "run `governance-auth login` again if this persists", so
  // a naive match on the login command claims this is a logged-out session.
  // It is not: the session exists and the IdP was unreachable or revoked it,
  // and the two need different words.
  const kind = classifyAuthFailure(
    execError({
      code: 1,
      stderr:
        'Error: refreshing the access token; run `governance-auth login` again if this persists\n\n' +
        'Caused by:\n    error sending request for url (https://idp.example/token)\n',
    }),
  );
  assert.equal(kind, 'refresh-failed');
});

test('a missing issuer/client-id is "not configured", not "signed out"', () => {
  // This message also embeds a pasteable `governance-auth login --issuer …`
  // line, which is exactly why the generic login marker has to be last.
  const kind = classifyAuthFailure(
    execError({
      code: 1,
      stderr:
        '--issuer (or GOVERNANCE_AUTH_ISSUER, or `issuer` in a config file) and --client-id ' +
        '(or GOVERNANCE_AUTH_CLIENT_ID, or `client_id` in a config file) required.\n\n' +
        'First time here? governance-auth login \\\n    --issuer <your-issuer-url>\n',
    }),
  );
  assert.equal(kind, 'not-configured');
});

test('a binary that is not on PATH is distinguished from being signed out', () => {
  // The failure a desktop-launched VS Code produces: it spawns without a
  // shell, so `~/.local/bin` is often absent even though the terminal has it.
  // Telling this developer to run `governance-auth login` is a loop.
  assert.equal(classifyAuthFailure(execError({ code: 'ENOENT' })), 'binary-not-found');
  assert.equal(classifyAuthFailure(execError({ code: 'EACCES' })), 'binary-not-executable');
});

test('a killed spawn is a timeout, and an aborted one is a cancellation', () => {
  assert.equal(classifyAuthFailure(execError({ killed: true, signal: 'SIGTERM' })), 'timed-out');
  // Checked before `killed`, because an abort kills the child too.
  assert.equal(
    classifyAuthFailure(execError({ name: 'AbortError', code: 'ABORT_ERR', killed: true })),
    'cancelled',
  );
});

test('an unrecognised failure is "unknown", never "signed out"', () => {
  // Guessing "signed out" sends the developer to a login that succeeds and
  // changes nothing, which is worse than admitting we cannot tell.
  assert.equal(classifyAuthFailure(execError({ code: 2, stderr: 'segmentation fault' })), 'unknown');
  assert.equal(classifyAuthFailure(undefined), 'unknown');
  assert.equal(classifyAuthFailure('boom'), 'unknown');
});

test('a Buffer stderr classifies the same as a string one', () => {
  const kind = classifyAuthFailure(
    execError({ code: 1, stderr: Buffer.from('no cached session for this issuer/client') }),
  );
  assert.equal(kind, 'signed-out');
});

test('every kind offers a fix the developer can act on', () => {
  const kinds = [
    'signed-out',
    'refresh-failed',
    'not-configured',
    'binary-not-found',
    'binary-not-executable',
    'timed-out',
    'empty-output',
    'unknown',
  ] as const;

  for (const kind of kinds) {
    const advice = authFailureAdvice(kind, '/usr/local/bin/governance-auth');
    assert.ok(advice.title.length > 0, `${kind} has no title`);
    assert.ok(advice.actions.length > 0, `${kind} offers no action`);
    // (b) of the requirement: it must say this is not a bug in their code.
    assert.match(advice.detail, /not a fault in your code|cancelled/, `${kind} blames the developer`);
  }
});

test('the signed-out message names the exact command, with the configured path', () => {
  const advice = authFailureAdvice('signed-out', '/opt/gov/governance-auth');
  assert.equal(advice.fixCommand, '/opt/gov/governance-auth login');
  assert.equal(advice.runnable, true);
  assert.match(advice.detail, /governance-auth login/);
});

test('a command carrying placeholders is never marked runnable', () => {
  // It goes into the terminal typed, not executed. Pressing Enter on
  // `--issuer <your-issuer-url>` fails in a way that looks like our bug.
  const advice = authFailureAdvice('not-configured', 'governance-auth');
  assert.match(advice.fixCommand ?? '', /<your-issuer-url>/);
  assert.equal(advice.runnable, false);
});
