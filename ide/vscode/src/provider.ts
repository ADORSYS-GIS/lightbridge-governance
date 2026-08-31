import * as vscode from 'vscode';

import { getToken, hasCredential } from './auth.js';
import { fetchCatalogue, logCatalogueFailure } from './catalogue.js';
import { readConfig } from './config.js';
import { errorMessage, log, redact } from './log.js';
import { toWireMessages, toWireToolChoice, toWireTools } from './messages.js';
import { pumpStream } from './stream.js';
import type { LightbridgeModel } from './types.js';

export class LightbridgeChatProvider implements vscode.LanguageModelChatProvider<LightbridgeModel> {
  private readonly changed = new vscode.EventEmitter<void>();

  readonly onDidChangeLanguageModelChatInformation = this.changed.event;

  /** Re-advertise the model list, e.g. after sign-in or a settings change. */
  refresh(): void {
    this.changed.fire();
  }

  dispose(): void {
    this.changed.dispose();
  }

  /**
   * Advertise the models this provider can serve.
   *
   * Called with `silent: true` in contexts where no UI may be shown. In that
   * case an absent credential means we contribute nothing — we do not prompt,
   * and we do not list models we would then fail to serve. A model in the
   * picker that errors on first use is worse than one that was never offered.
   */
  async provideLanguageModelChatInformation(
    options: { readonly silent: boolean },
    _token: vscode.CancellationToken,
  ): Promise<LightbridgeModel[]> {
    const config = readConfig();

    if (config.gatewayUrl === undefined) {
      return [];
    }

    if (options.silent && !(await hasCredential(config))) {
      log().info('No cached session; contributing no models in silent mode.');
      return [];
    }

    try {
      return await fetchCatalogue(config);
    } catch (err) {
      // Withhold rather than guess. There is no cached-model fallback here on
      // purpose: serving a stale catalogue after the gateway has stopped
      // answering is how a model that policy has withdrawn stays selectable.
      logCatalogueFailure(err);
      return [];
    }
  }

  async provideLanguageModelChatResponse(
    model: LightbridgeModel,
    messages: readonly vscode.LanguageModelChatRequestMessage[],
    options: vscode.ProvideLanguageModelChatResponseOptions,
    progress: vscode.Progress<vscode.LanguageModelResponsePart>,
    token: vscode.CancellationToken,
  ): Promise<void> {
    const config = readConfig();

    if (config.gatewayUrl === undefined) {
      throw vscode.LanguageModelError.NotFound('No lightbridge.gatewayUrl is configured.');
    }

    // Throws rather than proceeding anonymously. See auth.ts.
    const accessToken = await getToken(config);

    const controller = new AbortController();
    const cancel = token.onCancellationRequested(() => controller.abort());
    const timeout = setTimeout(() => controller.abort(), config.requestTimeoutMs);

    const url = `${config.gatewayUrl}/v1/chat/completions`;
    const body = {
      // Caller-supplied modelOptions win over the configured defaults. VS Code
      // never populates modelOptions from its own UI, so in the Copilot Chat
      // path this is settings-only — see lightbridge.modelOptionDefaults.
      ...config.modelOptionDefaults,
      ...(options.modelOptions ?? {}),
      model: model.upstreamId,
      messages: toWireMessages(messages),
      stream: true,
      ...(options.tools && options.tools.length > 0
        ? { tools: toWireTools(options.tools), tool_choice: toWireToolChoice(options.toolMode) }
        : {}),
    };

    try {
      const res = await fetch(url, {
        method: 'POST',
        headers: {
          authorization: `Bearer ${accessToken}`,
          'content-type': 'application/json',
          accept: 'text/event-stream',
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });

      if (res.status === 401 || res.status === 403) {
        throw vscode.LanguageModelError.NoPermissions(
          `The gateway refused this request (${res.status}). Run 'governance-auth login'.`,
        );
      }
      if (!res.ok) {
        // The status, not the body: an error body can echo the prompt back.
        throw new Error(`${redact(url)} returned ${res.status}.`);
      }
      if (!res.body) {
        throw new Error(`${redact(url)} returned no response body.`);
      }

      await pumpStream(res.body, progress, token);
    } catch (err) {
      if (controller.signal.aborted && token.isCancellationRequested) {
        return; // A user cancellation is not a failure.
      }
      if (err instanceof vscode.LanguageModelError) {
        throw err;
      }
      throw new Error(`Chat request failed: ${errorMessage(err)}`);
    } finally {
      clearTimeout(timeout);
      cancel.dispose();
    }
  }

  /**
   * Estimate the token count for a piece of text.
   *
   * This is an estimate and is documented as one. The real tokenizer lives with
   * the model, and this extension has no access to it; the alternative — a
   * network round trip to the gateway per call — sits on a path VS Code invokes
   * while building every prompt.
   *
   * The ratio deliberately **over**-counts. The two errors are not symmetric:
   * over-counting costs a little unused context, while under-counting means VS
   * Code packs a prompt the model then rejects, which surfaces to the developer
   * as a failed request with no obvious cause.
   */
  async provideTokenCount(
    _model: LightbridgeModel,
    text: string | vscode.LanguageModelChatRequestMessage,
    _token: vscode.CancellationToken,
  ): Promise<number> {
    const value = typeof text === 'string' ? text : extractText(text);
    return Math.ceil(value.length / 3.5);
  }
}

function extractText(message: vscode.LanguageModelChatRequestMessage): string {
  const chunks: string[] = [];
  for (const part of message.content) {
    if (part instanceof vscode.LanguageModelTextPart) {
      chunks.push(part.value);
    }
  }
  return chunks.join('');
}
