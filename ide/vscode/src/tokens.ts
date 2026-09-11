/**
 * Token estimation and message-text extraction.
 *
 * No `vscode` import — the same rule `retry.ts` follows: this runs on paths
 * unit tests exercise from plain node, where the `vscode` module does not
 * exist. The provider-facing wrapper lives in `provider.ts`; the
 * provider-shaped path (message objects, cancellation tokens) is covered by
 * the integration suite, which bundles `vscode` to the test stub.
 */

/** Characters per token used for the estimate. Chosen to over-count. */
const CHARS_PER_TOKEN = 3.5;

/** The slice of a chat request message this module needs. */
export interface MessageContent {
  readonly content: readonly unknown[];
}

/**
 * Extracts plain text from a chat message's content parts.
 *
 * Accepts a `LanguageModelTextPart` — or anything structurally matching one,
 * i.e. an object with a string `value`. The structural check is what makes
 * this testable and realm-safe: unit tests construct part-like objects that
 * are deliberately not `instanceof` the real class, and a part deserialized
 * across a module realm would fail that check too. Parts without a string
 * `value` (tool calls, data and image parts) carry no prompt text and are
 * dropped.
 */
export function extractText(message: MessageContent): string {
  const chunks: string[] = [];
  for (const part of message.content) {
    if (
      typeof part === 'object' &&
      part !== null &&
      'value' in part &&
      typeof (part as { value: unknown }).value === 'string'
    ) {
      chunks.push((part as { value: string }).value);
    }
  }
  return chunks.join('');
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
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / CHARS_PER_TOKEN);
}
