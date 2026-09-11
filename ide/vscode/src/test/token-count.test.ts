// Unit tests for the token-count logic.
//
// Targets `tokens.ts`, which is deliberately vscode-free (the same rule
// `retry.ts` follows): `provider.ts` imports the `vscode` module, which does
// not exist outside the extension host, so a unit test that imported it could
// not even load. The provider-shaped path (`provideTokenCount` with a real
// message and cancellation token) is covered by the integration suite, which
// bundles `vscode` to the test stub.
//
// Part-like objects here are deliberately plain objects, not class instances:
// that is the duck-typing contract `extractText` exists to honour.
import assert from 'node:assert/strict';
import test from 'node:test';

import { estimateTokens, extractText } from '../tokens.ts';

test('extractText: returns empty string for empty message', () => {
  assert.equal(extractText({ content: [] }), '');
});

test('extractText: concatenates text parts', () => {
  const message = {
    content: [{ value: 'Hello' }, { value: ' world' }],
  };
  assert.equal(extractText(message), 'Hello world');
});

test('extractText: accepts class instances that carry a string value', () => {
  // What a real LanguageModelTextPart looks like structurally. Written as a
  // plain field, not a constructor parameter property: strip-only type
  // stripping (node --experimental-strip-types) cannot transform those.
  class TextPart {
    value: string;
    constructor(value: string) {
      this.value = value;
    }
  }
  const message = { content: [new TextPart('Hello'), new TextPart(' world')] };
  assert.equal(extractText(message), 'Hello world');
});

test('extractText: ignores parts that carry no prompt text', () => {
  const message = {
    content: [
      { value: 'Hello' },
      { type: 'image' }, // no `value` at all
      { value: 123 }, // `value` is not a string
      { value: ' world' },
    ],
  };
  assert.equal(extractText(message), 'Hello world');
});

test('extractText: duck-types structurally matching parts from another realm', () => {
  // A plain object, e.g. deserialized from JSON, that is not an `instanceof`
  // LanguageModelTextPart but has a string `value`.
  const message = {
    content: [{ value: 'Structural ' }, { value: 'match' }],
  };
  assert.equal(extractText(message), 'Structural match');
});

test('estimateTokens: divides text length by 3.5 and rounds up', () => {
  // 17 chars / 3.5 = 4.85 -> Math.ceil -> 5
  assert.equal(estimateTokens('12345678901234567'), 5);
  // 35 / 3.5 = 10 exactly — still asserted as the integration scenario's value
  assert.equal(estimateTokens('This is exactly 35 characters long!'), 10);
});

test('estimateTokens: never returns zero for non-empty text', () => {
  // Under-counting is the asymmetric failure (see tokens.ts): one character
  // must still budget one token.
  assert.equal(estimateTokens('x'), 1);
});

test('estimateTokens: empty text costs nothing', () => {
  assert.equal(estimateTokens(''), 0);
});
