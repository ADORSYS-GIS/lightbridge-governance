import * as vscode from 'vscode';

import { resolveAuthState } from './auth.js';
import { authFailureAdvice } from './auth-failure.js';
import { invalidateAuthState } from './auth-state.js';
import { invalidateCatalogue } from './catalogue.js';
import { readConfig } from './config.js';
import { log } from './log.js';
import type { LightbridgeChatProvider } from './provider.js';

/**
 * The `managementCommand` from package.json — the entry VS Code puts next to
 * this provider in the model picker.
 *
 * There is no declarative settings schema for a provider, so this is hand-built.
 * It deliberately does not offer to enter an API key: the credential is
 * `governance-auth`'s and this extension never holds one. Signing in means
 * running `governance-auth login` in a terminal, which is also the only place
 * a browser flow can legitimately start from.
 */
export function registerManageCommand(provider: LightbridgeChatProvider): vscode.Disposable {
  return vscode.commands.registerCommand('lightbridge.manage', async () => {
    const config = readConfig();
    const auth = await resolveAuthState(config);
    // Names the actual reason rather than "not signed in" for all nine of them.
    // A developer whose editor cannot find the binary is not signed out, and
    // telling them to log in is a loop with no exit.
    const problem = auth.signedIn
      ? undefined
      : authFailureAdvice(auth.kind ?? 'unknown', config.governanceAuthPath);

    const picked = await vscode.window.showQuickPick(
      [
        {
          label: problem ? `$(warning) ${problem.title}` : '$(pass) Signed in',
          detail:
            problem?.fixCommand !== undefined
              ? `Opens a terminal on '${problem.fixCommand}'.`
              : (problem?.detail ?? `Credential supplied by '${config.governanceAuthPath} token'.`),
          id: 'signin',
        },
        {
          label: '$(gear) Open settings',
          detail: config.gatewayUrl ?? 'No gateway URL configured — no models will be offered.',
          id: 'settings',
        },
        {
          label: '$(refresh) Refresh model catalogue',
          detail: 'Re-fetch the model list and its context windows from the gateway.',
          id: 'refresh',
        },
        { label: '$(output) Show log', detail: 'Open the Lightbridge output channel.', id: 'log' },
      ],
      { title: 'Lightbridge Governance' },
    );

    switch (picked?.id) {
      case 'signin':
        // Delegated so the terminal, the command it carries and the cache
        // invalidations are written once. Two copies of "how do you sign in"
        // is how one of them ends up stale.
        await vscode.commands.executeCommand('lightbridge.signIn');
        break;
      case 'settings':
        await vscode.commands.executeCommand('workbench.action.openSettings', 'lightbridge');
        break;
      case 'refresh':
        invalidateCatalogue();
        // The session is re-checked too: "refresh" from a developer who has
        // just signed in elsewhere must not be answered from a cached "no".
        invalidateAuthState();
        provider.refresh();
        log().info('Model catalogue invalidated by user request.');
        break;
      case 'log':
        log().show();
        break;
      default:
        break;
    }
  });
}
