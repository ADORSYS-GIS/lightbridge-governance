import * as vscode from 'vscode';

import { authFailureAdvice } from './auth-failure.js';
import { authStateGeneration, cachedAuthState, invalidateAuthState } from './auth-state.js';
import { invalidateCatalogue } from './catalogue.js';
import { readConfig } from './config.js';
import { log } from './log.js';
import type { AuthAction, AuthFailureAdvice, AuthFailureKind } from './auth-failure.js';
import type { Config } from './config.js';
import type { LightbridgeChatProvider } from './provider.js';

/**
 * Turning a classified credential failure into something a developer can act
 * on without leaving the editor.
 *
 * The rule this module exists to enforce: a failure the developer *caused by
 * doing something* gets a notification with the fix attached. An output-channel
 * line is not that. Before this file, a signed-out developer opened the model
 * picker, saw no Lightbridge models, and had no way to learn why short of
 * knowing an output channel existed and going to read it.
 */

/**
 * Show one notification per transition, not one per failed call.
 *
 * VS Code invokes the provider from several contexts, so a single signed-out
 * moment can produce a handful of failures within a second. The generation
 * counter steps only when the state actually changes, so a burst collapses to
 * one toast; the time floor lets it speak again if the developer is still stuck
 * much later.
 */
const RENOTIFY_AFTER_MS = 5 * 60_000;
let lastNotified: { readonly generation: number; readonly at: number } | undefined;

/** Reset in tests, and after a sign-in attempt, so the next failure is heard. */
export function resetAuthNotifications(): void {
  lastNotified = undefined;
}

/**
 * Report a credential failure at the point the developer hit it.
 *
 * Returns without showing anything for a cancellation — a request the developer
 * cancelled is not a fault — and for a repeat of an already-reported state.
 */
export async function reportAuthFailure(kind: AuthFailureKind, config: Config): Promise<void> {
  if (kind === 'cancelled') {
    return;
  }

  const advice = authFailureAdvice(kind, config.governanceAuthPath);
  log().error(`Credential unavailable (${kind}). ${advice.title}`);

  const generation = authStateGeneration();
  const now = Date.now();
  if (lastNotified && lastNotified.generation === generation && now - lastNotified.at < RENOTIFY_AFTER_MS) {
    return;
  }
  lastNotified = { generation, at: now };

  const labels = advice.actions.map((action) => actionLabel(action, advice));
  const picked = await vscode.window.showErrorMessage(advice.title, ...labels);
  if (picked === undefined) {
    return;
  }

  const chosen = advice.actions[labels.indexOf(picked)];
  if (chosen !== undefined) {
    await runAction(chosen);
  }
}

function actionLabel(action: AuthAction, advice: AuthFailureAdvice): string {
  switch (action) {
    case 'sign-in':
      // A command carrying placeholders is offered, never run: pressing Enter
      // on `--issuer <your-issuer-url>` fails in a way that looks like our bug.
      return advice.runnable ? 'Sign in' : 'Open terminal';
    case 'open-settings':
      return 'Open settings';
    case 'show-log':
      return 'Show log';
  }
}

async function runAction(action: AuthAction): Promise<void> {
  switch (action) {
    case 'sign-in':
      await vscode.commands.executeCommand('lightbridge.signIn');
      break;
    case 'open-settings':
      await vscode.commands.executeCommand('workbench.action.openSettings', 'lightbridge');
      break;
    case 'show-log':
      log().show();
      break;
  }
}

/**
 * `lightbridge.signIn` — put the fix in front of the developer, in a terminal.
 *
 * A terminal rather than a spawned browser, for the same reason `manage.ts`
 * already used one: `governance-auth login` owns the flow, and a developer
 * needs to see its output to act on a failure. This extension never runs the
 * OAuth flow itself and never holds the credential (ADR-0010).
 *
 * Both caches are dropped up front rather than after: the developer is about to
 * change the answer, and a picker that still says "no models" once they have
 * signed in is the same silent failure this branch set out to remove.
 */
export function registerSignInCommand(provider: LightbridgeChatProvider): vscode.Disposable {
  return vscode.commands.registerCommand('lightbridge.signIn', () => {
    const config = readConfig();
    const advice = signInAdvice(config);

    invalidateAuthState();
    invalidateCatalogue();
    resetAuthNotifications();
    provider.refresh();

    const terminal = vscode.window.createTerminal('governance-auth');
    terminal.show();
    if (advice.fixCommand !== undefined) {
      // `runnable: false` means the command has placeholders in it. It still
      // goes into the terminal — typed, not executed — so the developer edits
      // a real command instead of retyping one from a notification.
      terminal.sendText(advice.fixCommand, advice.runnable);
    }
    log().info(`Sign-in requested; '${advice.fixCommand ?? config.governanceAuthPath}' offered in a terminal.`);
  });
}

/**
 * Which command the sign-in terminal should carry.
 *
 * Read from the last known failure, so a developer whose problem is a missing
 * issuer gets the flags rather than a bare `login` that fails the same way
 * again. Anything else — including a state we have not observed at all — gets
 * the plain login, which is the right command in every remaining case where a
 * terminal helps.
 *
 * `Infinity` on purpose: this is a phrasing decision, not an authorization one,
 * and an aged observation is still the best guess available. Nothing here
 * decides whether a request may be sent.
 */
function signInAdvice(config: Config): AuthFailureAdvice {
  const kind = cachedAuthState(Number.POSITIVE_INFINITY)?.kind;
  return authFailureAdvice(kind === 'not-configured' ? kind : 'signed-out', config.governanceAuthPath);
}
