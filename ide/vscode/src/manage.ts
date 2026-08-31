import * as vscode from 'vscode';

import { hasCredential } from './auth.js';
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
    const signedIn = await hasCredential(config);

    const picked = await vscode.window.showQuickPick(
      [
        {
          label: signedIn ? '$(pass) Signed in' : '$(warning) Not signed in',
          detail: signedIn
            ? `Credential supplied by '${config.governanceAuthPath} token'.`
            : `Run 'governance-auth login' to sign in.`,
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
        // A terminal, not a spawned browser: `governance-auth login` owns the
        // flow, and a developer needs to see its output to act on a failure.
        vscode.window.createTerminal('governance-auth').show();
        vscode.window.showInformationMessage(
          `Run '${config.governanceAuthPath} login' in the terminal, then refresh the catalogue.`,
        );
        break;
      case 'settings':
        await vscode.commands.executeCommand('workbench.action.openSettings', 'lightbridge');
        break;
      case 'refresh':
        invalidateCatalogue();
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
