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
 * Extracts the text a chat message will put on the wire.
 *
 * The goal is to match `toWireMessages`/`flattenResult` in `messages.ts`, which
 * decide what is actually sent and therefore what counts against the context
 * window:
 *
 * - a text part's `value` is counted;
 * - a tool call (`LanguageModelToolCallPart`) sends its `name` and
 *   JSON-serialised `input` on the wire, so both are counted;
 * - a tool result (`LanguageModelToolResultPart`) carries its text one level
 *   down under `content` — that text IS sent on the wire, so it is counted too,
 *   by descending into the nested array;
 * - parts with no prompt text at all (data, image parts) are dropped.
 *
 * The structural check is deliberately wider than `instanceof`: it accepts any
 * object with a string `value`, which is what makes this realm-safe and
 * unit-testable. That also means a non-text part that happens to carry a string
 * `value` (e.g. a prompt-tsx part) is counted rather than guessed at — a
 * conservative over-estimate, which is the safe direction for a token budget.
 */
export function extractText(message: MessageContent): string {
  const chunks: string[] = [];
  for (const part of message.content) {
    if (typeof part !== 'object' || part === null) {
      continue;
    }
    const value = (part as { value?: unknown }).value;
    if (typeof value === 'string') {
      chunks.push(value);
      continue;
    }
    // A tool call is sent by toWireMessages as its name plus the JSON
    // serialisation of its input — often the largest payload in the turn (an
    // edit call carries replacement text). Neither lives under `value` or a
    // nested array, so it has to be counted explicitly or an agentic prompt
    // under-counts by exactly the text that dominates it.
    const name = (part as { name?: unknown }).name;
    if (typeof name === 'string' && 'input' in part) {
      chunks.push(name, JSON.stringify((part as { input?: unknown }).input ?? {}));
      continue;
    }
    // A tool result nests its text one level down, and that text is sent by
    // toWireMessages — so it has to be counted here too, or an agentic prompt
    // full of tool output would estimate as ~0 tokens.
    const nested = (part as { content?: unknown }).content;
    if (Array.isArray(nested)) {
      chunks.push(extractText({ content: nested }));
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
