import * as vscode from 'vscode';

import { authFailureAdvice } from './auth-failure.js';
import { cachedAuthState, onAuthStateChange } from './auth-state.js';
import { onConfigChange, readConfig } from './config.js';

/**
 * A status-bar entry that appears only when something is wrong.
 *
 * The silent path of `provideLanguageModelChatInformation` may not show UI —
 * VS Code says so, and prompting from it would be a popup nobody asked for. But
 * "may not prompt" is not "must say nothing", and the failure that path
 * produces is invisible by construction: the provider contributes no models, so
 * the developer never reaches a point of use where an error could be raised.
 * This is the affordance that covers that gap.
 *
 * It is hidden while signed in, and hidden when no gateway is configured — an
 * extension installed but not in use should not occupy the status bar to say
 * so. Restraint here is not decoration: an indicator that is always present is
 * one nobody reads when it changes.
 */
export function registerStatusBar(): vscode.Disposable {
  const item = vscode.window.createStatusBarItem(
    'lightbridge.authStatus',
    vscode.StatusBarAlignment.Right,
    100,
  );
  item.name = 'Lightbridge Governance';
  item.command = 'lightbridge.manage';

  const render = (): void => {
    const config = readConfig();
    // Ignoring the TTL is the point. This is a display, not an authorization
    // decision — no request is permitted or refused by what it shows. Reading
    // the freshness window instead would make the warning disappear on its own
    // 30 seconds after it appeared, while the developer was still signed out.
    const state = cachedAuthState(Number.POSITIVE_INFINITY);

    if (config.gatewayUrl === undefined || state === undefined || state.signedIn) {
      item.hide();
      return;
    }

    const advice = authFailureAdvice(state.kind ?? 'unknown', config.governanceAuthPath);
    item.text = state.kind === 'signed-out' ? '$(warning) Lightbridge: signed out' : '$(warning) Lightbridge';
    item.tooltip = new vscode.MarkdownString(`**${advice.title}**\n\n${advice.detail}`);
    item.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
    item.show();
  };

  render();

  const subscriptions = [item, onAuthStateChange(render), onConfigChange(render)];
  return {
    dispose(): void {
      for (const subscription of subscriptions) {
        subscription.dispose();
      }
    },
  };
}
