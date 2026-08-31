/**
 * A minimal server-sent-events framer.
 *
 * Free of any `vscode` import so the chunk-boundary handling can be tested
 * directly. That handling is the whole reason this is a separate module: SSE
 * events are separated by a blank line and a JSON payload routinely straddles
 * two network chunks, so a parser that splits on newline alone works in
 * development and truncates intermittently under load.
 */
export class SseParser {
  private buffer = '';
  private readonly decoder = new TextDecoder();

  /**
   * Feed bytes and return the `data:` payloads that are now complete.
   *
   * A partial trailing event stays buffered until its terminator arrives.
   * `[DONE]` is returned like any other payload; recognising it is the
   * caller's business.
   */
  push(bytes: Uint8Array): string[] {
    this.buffer += this.decoder.decode(bytes, { stream: true });
    return this.drain();
  }

  /** Feed a string directly. Used by tests and by non-binary transports. */
  pushText(text: string): string[] {
    this.buffer += text;
    return this.drain();
  }

  private drain(): string[] {
    const payloads: string[] = [];

    let boundary = this.buffer.indexOf('\n\n');
    while (boundary !== -1) {
      const event = this.buffer.slice(0, boundary);
      this.buffer = this.buffer.slice(boundary + 2);

      for (const line of event.split('\n')) {
        if (line.startsWith('data:')) {
          payloads.push(line.slice(5).trim());
        }
      }

      boundary = this.buffer.indexOf('\n\n');
    }

    return payloads;
  }
}
