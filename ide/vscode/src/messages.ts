import * as vscode from 'vscode';

import type { WireMessage, WireTool, WireToolCall } from './types.js';

/**
 * Map VS Code's messages onto the OpenAI-compatible wire format.
 *
 * Two shape mismatches worth knowing before reading this:
 *
 * 1. `LanguageModelChatMessageRole` has **only** `User` and `Assistant`. There
 *    is no System role in the provider API, so a system prompt arrives as a
 *    User message and there is nothing here that can recover the distinction.
 *    Do not invent one by sniffing content.
 * 2. A tool *result* arrives as a part inside a User message, but the wire
 *    format wants it as a standalone `role: "tool"` message. So one VS Code
 *    message can fan out into several wire messages, which is why this returns
 *    an array rather than mapping one-to-one.
 */
export function toWireMessages(
  messages: readonly vscode.LanguageModelChatRequestMessage[],
): WireMessage[] {
  const out: WireMessage[] = [];

  for (const message of messages) {
    const role = message.role === vscode.LanguageModelChatMessageRole.Assistant ? 'assistant' : 'user';
    const text: string[] = [];
    const toolCalls: WireToolCall[] = [];

    for (const part of message.content) {
      if (part instanceof vscode.LanguageModelTextPart) {
        text.push(part.value);
      } else if (part instanceof vscode.LanguageModelToolCallPart) {
        toolCalls.push({
          id: part.callId,
          type: 'function',
          function: { name: part.name, arguments: JSON.stringify(part.input ?? {}) },
        });
      } else if (part instanceof vscode.LanguageModelToolResultPart) {
        // Flushed before the result so ordering survives: the wire format is
        // positional and a result that precedes its own call is rejected.
        flush(out, role, text, toolCalls);
        out.push({ role: 'tool', tool_call_id: part.callId, content: flattenResult(part) });
      }
      // Other part kinds (images, prompt-tsx) are dropped rather than guessed
      // at. Vision is advertised from the catalogue, so a model that claims
      // imageInput and lands here is a gap to close deliberately, not to paper
      // over with a lossy encoding.
    }

    flush(out, role, text, toolCalls);
  }

  return out;
}

function flush(
  out: WireMessage[],
  role: 'user' | 'assistant',
  text: string[],
  toolCalls: WireToolCall[],
): void {
  if (text.length === 0 && toolCalls.length === 0) {
    return;
  }

  const message: WireMessage = {
    role,
    content: text.length > 0 ? text.join('') : null,
  };
  if (toolCalls.length > 0) {
    message.tool_calls = [...toolCalls];
  }

  out.push(message);
  text.length = 0;
  toolCalls.length = 0;
}

/** Reduce a tool result's content parts to the single string the wire wants. */
function flattenResult(part: vscode.LanguageModelToolResultPart): string {
  const chunks: string[] = [];
  for (const item of part.content) {
    if (item instanceof vscode.LanguageModelTextPart) {
      chunks.push(item.value);
    }
  }
  return chunks.join('');
}

/** Map the tools VS Code offers onto the wire's `tools[]`. */
export function toWireTools(tools: readonly vscode.LanguageModelChatTool[]): WireTool[] {
  return tools.map((tool) => ({
    type: 'function',
    function: {
      name: tool.name,
      description: tool.description,
      parameters: (tool.inputSchema as object | undefined) ?? { type: 'object', properties: {} },
    },
  }));
}

/**
 * Map the tool mode.
 *
 * The API says plainly that "the provider must implement respecting this", so
 * `Required` is forwarded as `required` rather than quietly downgraded to
 * `auto` — a caller that asked to be forced into a tool call and got prose back
 * has no way to tell that we ignored them.
 */
export function toWireToolChoice(mode: vscode.LanguageModelChatToolMode): 'auto' | 'required' {
  return mode === vscode.LanguageModelChatToolMode.Required ? 'required' : 'auto';
}
