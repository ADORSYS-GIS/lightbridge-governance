// Unit tests for the token-count logic.
import assert from 'node:assert/strict';
import test from 'node:test';

import { LightbridgeChatProvider, extractText } from '../provider.ts';
import { LanguageModelTextPart } from '../../tests/support/vscode-stub.mjs';

test('extractText: returns empty string for empty message', () => {
  assert.equal(extractText({ content: [] } as any), '');
});

test('extractText: concatenates LanguageModelTextPart parts', () => {
  const message = {
    content: [
      new LanguageModelTextPart('Hello'),
      new LanguageModelTextPart(' world'),
    ],
  };
  assert.equal(extractText(message as any), 'Hello world');
});

test('extractText: ignores unknown parts', () => {
  const message = {
    content: [
      new LanguageModelTextPart('Hello'),
      { type: 'image' },
      new LanguageModelTextPart(' world'),
    ],
  };
  assert.equal(extractText(message as any), 'Hello world');
});

test('extractText: duck-types structurally matching text parts', () => {
  // A plain object, e.g. deserialized from JSON, that is not an `instanceof`
  // LanguageModelTextPart, but has a string `value`.
  const message = {
    content: [
      { value: 'Structural ' },
      { value: 'match' },
      { value: 123 }, // Ignored, not a string
    ],
  };
  assert.equal(extractText(message as any), 'Structural match');
});

test('provideTokenCount: estimates tokens by dividing text length by 3.5', async () => {
  const provider = new LightbridgeChatProvider();
  // 17 chars / 3.5 = 4.85 -> Math.ceil -> 5
  const count = await provider.provideTokenCount(
    '12345678901234567',
    {} as any,
  );
  assert.equal(count, 5);
});
