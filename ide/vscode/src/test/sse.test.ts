import assert from 'node:assert/strict';
import test from 'node:test';

import { SseParser } from '../sse.ts';

const encode = (s: string) => new TextEncoder().encode(s);

test('a payload split across two chunks is not emitted until its terminator', () => {
  const parser = new SseParser();

  // The split lands mid-JSON, which is the case that matters: a parser that
  // splits on '\n' instead of '\n\n', or that does not retain the partial
  // tail, emits '{"choices":[{"delta":{"content":"he' here.
  assert.deepEqual(parser.push(encode('data: {"choices":[{"delta":{"content":"he')), []);
  assert.deepEqual(parser.push(encode('llo"}}]}\n\n')), [
    '{"choices":[{"delta":{"content":"hello"}}]}',
  ]);
});

test('a chunk ending on a single newline does not emit yet', () => {
  const parser = new SseParser();

  // This is the test that actually pins the framing to '\n\n'. The two tests
  // either side of it pass even if the terminator is weakened to '\n', because
  // their payloads contain no internal newline — verified by injecting that
  // exact bug and watching them stay green. Here a blank line has NOT yet
  // arrived, so a '\n'-framing parser emits the event one chunk early.
  assert.deepEqual(parser.pushText('data: {"a":1}\n'), []);
  assert.deepEqual(parser.pushText('\n'), ['{"a":1}']);
});

test('a data line is not emitted before the trailing lines of its own event', () => {
  const parser = new SseParser();

  // A well-formed event whose 'data:' line is followed by another field. A
  // '\n'-framing parser emits on the first newline, i.e. before the event is
  // complete.
  assert.deepEqual(parser.pushText('data: {"a":1}\nid: 7\n'), []);
  assert.deepEqual(parser.pushText('\n'), ['{"a":1}']);
});

test('several complete events in one chunk all come back, in order', () => {
  const parser = new SseParser();

  assert.deepEqual(parser.pushText('data: {"a":1}\n\ndata: {"a":2}\n\ndata: [DONE]\n\n'), [
    '{"a":1}',
    '{"a":2}',
    '[DONE]',
  ]);
});

test('a multi-byte character split across chunks survives', () => {
  const parser = new SseParser();
  const bytes = new TextEncoder().encode('data: {"c":"€"}\n\n');

  // Cutting a 3-byte UTF-8 sequence in half. Without the decoder's streaming
  // mode this yields a replacement character and the JSON is silently wrong.
  assert.deepEqual(parser.push(bytes.slice(0, 12)), []);
  assert.deepEqual(parser.push(bytes.slice(12)), ['{"c":"€"}']);
});

test('non-data lines are ignored', () => {
  const parser = new SseParser();

  assert.deepEqual(parser.pushText(': keep-alive\nevent: message\ndata: {"a":1}\n\n'), ['{"a":1}']);
});
