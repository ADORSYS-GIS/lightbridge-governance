import * as vscode from 'vscode';

import { invalidateAuthState } from './auth-state.js';
import { registerSignInCommand } from './auth-ui.js';
import { invalidateCatalogue } from './catalogue.js';
import { onConfigChange } from './config.js';
import { initLog, log } from './log.js';
import { registerManageCommand } from './manage.js';
import { LightbridgeChatProvider } from './provider.js';
import { registerStatusBar } from './status-bar.js';

/** Must match `contributes.languageModelChatProviders[].vendor` in package.json. */
const VENDOR = 'lightbridge';

export function activate(context: vscode.ExtensionContext): void {
  initLog();
  log().info('Lightbridge Governance activating.');

  const provider = new LightbridgeChatProvider();

  context.subscriptions.push(
    provider,
    vscode.lm.registerLanguageModelChatProvider(VENDOR, provider),
    registerManageCommand(provider),
    registerSignInCommand(provider),
    registerStatusBar(),
    vscode.commands.registerCommand('lightbridge.showLog', () => log().show()),
    vscode.commands.registerCommand('lightbridge.refreshCatalogue', () => {
      invalidateCatalogue();
      invalidateAuthState();
      provider.refresh();
    }),
    // The gateway URL and the auth binary both change which models exist and
    // whether they can be served, so a settings edit has to drop the cache as
    // well as re-advertise. Re-advertising alone would serve the old gateway's
    // catalogue from cache against the new gateway's credential.
    //
    // The auth state goes with them: `governanceAuthPath` is literally the
    // question the cached answer was an answer to, so keeping it across an edit
    // would report the old binary's verdict about the new one.
    onConfigChange(() => {
      invalidateCatalogue();
      invalidateAuthState();
      provider.refresh();
    }),
  );
}

export function deactivate(): void {
  // Everything is in context.subscriptions; VS Code disposes it.
}
