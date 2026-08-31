import * as vscode from 'vscode';

import { log } from './log.js';
import { SseParser } from './sse.js';
import type { WireChunk } from './types.js';

interface PendingCall {
  id: string;
  name: string;
  args: string;
}

/**
 * Consume an SSE stream of `chat.completion.chunk` and emit response parts.
 *
 * Text is emitted as it arrives — the API requires parts be reported "as they
 * are received", and buffering to the end of the stream is what makes a
 * provider feel broken next to a native one even when it is not.
 *
 * Tool calls cannot be. Their arguments arrive as a JSON *fragment* per chunk,
 * so a call is only well-formed once the stream finishes; emitting early would
 * mean handing VS Code an unparseable `input`. They are accumulated by index
 * and flushed at the end.
 */
export async function pumpStream(
  body: ReadableStream<Uint8Array>,
  progress: vscode.Progress<vscode.LanguageModelResponsePart>,
  token: vscode.CancellationToken,
): Promise<void> {
  const reader = body.getReader();
  const parser = new SseParser();
  const pending = new Map<number, PendingCall>();

  try {
    while (!token.isCancellationRequested) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }

      for (const payload of parser.push(value)) {
        if (payload === '[DONE]') {
          return flush(pending, progress);
        }
        handlePayload(payload, progress, pending);
      }
    }
  } finally {
    reader.cancel().catch(() => {
      /* the consumer is gone; nothing here can act on a failure to tear down */
    });
  }

  flush(pending, progress);
}

function handlePayload(
  payload: string,
  progress: vscode.Progress<vscode.LanguageModelResponsePart>,
  pending: Map<number, PendingCall>,
): void {
  let chunk: WireChunk;
  try {
    chunk = JSON.parse(payload) as WireChunk;
  } catch {
    // A single malformed event should not abort a response that is otherwise
    // streaming fine. The payload is a body and is not logged.
    log().warn('Discarded an unparseable stream event.');
    return;
  }

  const delta = chunk.choices?.[0]?.delta;
  if (!delta) {
    return;
  }

  if (typeof delta.content === 'string' && delta.content !== '') {
    progress.report(new vscode.LanguageModelTextPart(delta.content));
  }

  for (const call of delta.tool_calls ?? []) {
    const index = call.index ?? 0;
    const entry = pending.get(index) ?? { id: '', name: '', args: '' };
    if (call.id) {
      entry.id = call.id;
    }
    if (call.function?.name) {
      entry.name = call.function.name;
    }
    if (call.function?.arguments) {
      entry.args += call.function.arguments;
    }
    pending.set(index, entry);
  }
}

function flush(
  pending: Map<number, PendingCall>,
  progress: vscode.Progress<vscode.LanguageModelResponsePart>,
): void {
  for (const call of pending.values()) {
    if (call.name === '') {
      continue;
    }

    let input: object;
    try {
      input = call.args === '' ? {} : (JSON.parse(call.args) as object);
    } catch {
      // The model produced arguments that are not JSON. Dropping the call is
      // the safe branch: forwarding a half-parsed object would have VS Code
      // invoke a tool with arguments nobody wrote.
      log().warn(`Dropped tool call '${call.name}': arguments were not valid JSON.`);
      continue;
    }

    progress.report(new vscode.LanguageModelToolCallPart(call.id, call.name, input));
  }

  pending.clear();
}
