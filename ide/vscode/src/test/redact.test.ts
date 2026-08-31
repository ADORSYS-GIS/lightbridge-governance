import assert from 'node:assert/strict';
import test from 'node:test';

import { errorMessage, redact } from '../redact.ts';

test('a signed URL loses its query string', () => {
  assert.equal(
    redact('https://api.example/v1/chat/completions?token=super-secret&sig=abc'),
    'https://api.example/v1/chat/completions',
  );
});

test('a fragment is dropped too', () => {
  assert.equal(redact('https://api.example/models/info#tok=secret'), 'https://api.example/models/info');
});

test('an unparseable URL is not echoed back', () => {
  // The failure branch must not be the disclosing branch: returning the input
  // on a parse failure would leak exactly the strings most likely to be
  // malformed *because* something concatenated a credential into them.
  const out = redact('not a url ?token=super-secret');
  assert.equal(out, '<unparseable url>');
  assert.ok(!out.includes('super-secret'));
});

test('errorMessage keeps the message and drops everything else', () => {
  const err = new Error('gateway refused');
  (err as Error & { headers?: unknown }).headers = { authorization: 'Bearer super-secret' };

  const out = errorMessage(err);
  assert.equal(out, 'gateway refused');
  assert.ok(!out.includes('super-secret'));
});

test('a thrown non-Error is stringified rather than trusted', () => {
  assert.equal(errorMessage({ toString: () => 'odd' }), 'odd');
});
