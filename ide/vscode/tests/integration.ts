// Drives the REAL provider against an in-process fake gateway, with `vscode`
// aliased to a stub at bundle time (see tests/run.mjs).
//
// Covers what unit tests cannot reach without an extension host: catalogue
// mapping, streaming, tool-call reassembly, the modelOptions allowlist, and
// the fail-closed branches.
import assert from 'node:assert/strict';

import { __settings } from 'vscode';

import { invalidateCatalogue } from '../src/catalogue.ts';
import { LightbridgeChatProvider } from '../src/provider.ts';
import { startGateway } from './support/fake-gateway.ts';

// Supplied by tests/run.mjs. NOT derived from import.meta.url: this file is
// bundled into dist-test/, so a self-relative path resolves to a directory
// that does not contain the auth doubles.
const SUPPORT = process.env.LB_TEST_SUPPORT;
if (!SUPPORT) throw new Error('LB_TEST_SUPPORT is not set; run via tests/run.mjs');
const token = { isCancellationRequested: false, onCancellationRequested: () => ({ dispose() {} }) };

const gw = await startGateway();

function configure(authScript: string, gatewayUrl = gw.url) {
  invalidateCatalogue();
  __settings.gatewayUrl = gatewayUrl;
  __settings.governanceAuthPath = `${SUPPORT}${authScript}`;
  __settings.requestTimeoutMs = 15000;
  __settings.catalogueTtlMs = 0;
  __settings.modelOptionDefaults = { temperature: 0.2, top_k: 40 };
}

let failures = 0;
async function scenario(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    console.log(`  ok  ${name}`);
  } catch (err) {
    failures++;
    console.log(`FAIL  ${name}\n        ${err instanceof Error ? err.message : String(err)}`);
  }
}

await scenario('catalogue: a described model maps, with the window VS Code will display', async () => {
  configure('good-auth.sh');
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );
  assert.equal(models.length, 1, `expected 1 model, got ${models.length}`);
  const m = models[0]!;
  assert.equal(m.id, 'governed-sonnet');
  assert.equal(m.name, 'Governed Sonnet (200k context)');
  // VS Code renders maxInputTokens + maxOutputTokens as the total window, so
  // the input budget EXCLUDES the output reserve. Passing context_length
  // through verbatim advertised 264k for a 200k model — observed in the picker.
  assert.equal(m.maxInputTokens, 136000, 'input budget must exclude the output reserve');
  assert.equal(m.maxOutputTokens, 64000);
  assert.equal(m.maxInputTokens + m.maxOutputTokens, 200000, 'displayed sum must equal the window');
  assert.equal(m.capabilities.imageInput, true);
  assert.equal(m.capabilities.toolCalling, true);
});

await scenario('catalogue: a model with no context window is SKIPPED, not defaulted', async () => {
  configure('good-auth.sh');
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );
  assert.ok(!models.some((m) => m.id === 'no-window-model'), 'no-window-model must be skipped');
});

await scenario('catalogue: concurrent calls result in ONE fetch', async () => {
  configure('good-auth.sh');
  __settings.catalogueTtlMs = 300000;
  const before = gw.requests.filter((r) => r.method === 'GET').length;
  const provider = new LightbridgeChatProvider();
  await Promise.all(
    Array.from({ length: 5 }, () =>
      provider.provideLanguageModelChatInformation({ silent: false }, token as never),
    ),
  );
  const fetched = gw.requests.filter((r) => r.method === 'GET').length - before;
  // The result cache is only populated AFTER the await, so without in-flight
  // de-duplication all five miss it. Measured at 5 before the fix.
  assert.equal(fetched, 1, `expected 1 catalogue fetch for 5 concurrent calls, got ${fetched}`);
});

await scenario('streaming: text arrives and a fragmented tool call is reassembled', async () => {
  configure('good-auth.sh');
  const provider = new LightbridgeChatProvider();
  const models = await provider.provideLanguageModelChatInformation({ silent: false }, token as never);

  const parts: unknown[] = [];
  await provider.provideLanguageModelChatResponse(
    models[0]!,
    [{ role: 1, content: [new (await import('vscode')).LanguageModelTextPart('hi')] }] as never,
    { toolMode: 1, tools: [] } as never,
    { report: (p: unknown) => parts.push(p) },
    token as never,
  );

  const vscode = await import('vscode');
  const text = parts
    .filter((p) => p instanceof vscode.LanguageModelTextPart)
    .map((p) => (p as { value: string }).value)
    .join('');
  assert.equal(text, 'Hello.', `text was ${JSON.stringify(text)}`);

  const calls = parts.filter((p) => p instanceof vscode.LanguageModelToolCallPart) as Array<{
    callId: string;
    name: string;
    input: unknown;
  }>;
  assert.equal(calls.length, 1, `expected 1 tool call, got ${calls.length}`);
  assert.equal(calls[0]!.name, 'read_file');
  assert.equal(calls[0]!.callId, 'call_1');
  // Three JSON fragments, one of which straddled the frame terminator.
  assert.deepEqual(calls[0]!.input, { path: 'src/a.ts' });
});

await scenario('modelOptions: supported params pass, Copilot internals are dropped', async () => {
  configure('good-auth.sh');
  const provider = new LightbridgeChatProvider();
  const models = await provider.provideLanguageModelChatInformation({ silent: false }, token as never);
  await provider.provideLanguageModelChatResponse(
    models[0]!,
    [] as never,
    {
      toolMode: 1,
      tools: [],
      modelOptions: {
        temperature: 0.9,
        // Exactly what Copilot Chat was observed sending. None of it is in the
        // model's supported_parameters, so none of it may reach the gateway —
        // these are Copilot's internal telemetry identifiers.
        _capturingTokenCorrelationId: 'LEAKCANARY-correlation',
        _otelTraceContext: { traceId: 'LEAKCANARY-trace', spanId: 'x', traceFlags: 1 },
        _telemetryTurn: 1,
        _enableThinking: true,
      },
    } as never,
    { report: () => {} },
    token as never,
  );

  const post = gw.requests.filter((r) => r.method === 'POST').at(-1);
  assert.ok(post, 'no chat request reached the gateway');
  const body = post!.body as Record<string, unknown>;
  assert.equal(body.temperature, 0.9, 'caller modelOptions must win over the configured default');
  assert.equal(body.top_k, 40, 'configured default must survive');
  assert.equal(body.stream, true);
  assert.equal(body.model, 'governed-sonnet');
  const leaked = Object.keys(body).filter((k) => k.startsWith('_'));
  assert.deepEqual(leaked, [], `Copilot internals reached the wire: ${leaked.join(', ')}`);
  assert.ok(!JSON.stringify(body).includes('LEAKCANARY'), 'leak canary found in the request body');
});

// The fail-closed scenarios point at the PERMISSIVE probe, which accepts
// anything. Against the strict gateway they passed even with a fail-closed
// bypass injected, because the gateway's own 401 produced the empty list and
// the throw they asserted on.
await scenario('FAIL CLOSED: no credential, no models — even from a gateway that says yes', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: true },
    token as never,
  );
  assert.equal(models.length, 0, 'models were offered without a credential');
});

await scenario('FAIL CLOSED: a chat request without a credential throws before any request', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  const parts: unknown[] = [];
  let thrown: unknown;
  try {
    await new LightbridgeChatProvider().provideLanguageModelChatResponse(
      { id: 'governed-sonnet', upstreamId: 'governed-sonnet', supportedParameters: [] } as never,
      [] as never,
      { toolMode: 1 } as never,
      { report: (p: unknown) => parts.push(p) },
      token as never,
    );
  } catch (err) {
    thrown = err;
  }
  assert.ok(thrown, 'the request did not throw');
  // Specifically the credential error, not "the gateway said no".
  assert.equal(
    (thrown as Error).name,
    'NoCredentialError',
    `expected NoCredentialError, got ${(thrown as Error).name}: ${(thrown as Error).message}`,
  );
  assert.equal(parts.length, 0, 'content was streamed without a credential');
});

await scenario('FAIL CLOSED: an unreachable gateway offers no models', async () => {
  configure('good-auth.sh', 'http://127.0.0.1:9');
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );
  assert.equal(models.length, 0);
});

await scenario('FAIL CLOSED: no gateway configured offers no models', async () => {
  configure('good-auth.sh');
  __settings.gatewayUrl = undefined;
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );
  assert.equal(models.length, 0);
});

// The strongest form of the fail-closed assertion: the permissive probe must
// never have been contacted at all.
if (gw.permissiveHits.length > 0) {
  failures++;
  console.log(`FAIL  fail-closed breach: contacted the permissive gateway:`);
  for (const h of gw.permissiveHits) console.log(`        ${h}`);
} else {
  console.log('  ok  control probe never contacted (0 requests without a credential)');
}

await gw.close();
console.log(failures === 0 ? '\nintegration: all scenarios passed' : `\nintegration: ${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
