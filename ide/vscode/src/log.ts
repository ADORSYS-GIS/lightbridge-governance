import * as vscode from 'vscode';

/**
 * The output channel.
 *
 * There is exactly one rule about this module, and it is the repository's:
 * never log a token, a signed URL, or a request/response body. Nothing here
 * takes a message body, and `redact()` — in ./redact.js, kept vscode-free so
 * it is unit-testable — exists so a URL that picked up a query string cannot
 * leak one by accident.
 *
 * Prompt and completion text is a body. It does not get logged either, at any
 * level, even when debugging: a governance product that ships a transcript of
 * its users' prompts to an output channel is not one we can sell.
 */
let channel: vscode.LogOutputChannel | undefined;

export function initLog(): vscode.LogOutputChannel {
  channel ??= vscode.window.createOutputChannel('Lightbridge Governance', { log: true });
  return channel;
}

export function log(): vscode.LogOutputChannel {
  return initLog();
}

export { errorMessage, redact } from './redact.js';
