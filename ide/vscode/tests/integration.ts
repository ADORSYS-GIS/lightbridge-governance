// Drives the REAL provider against an in-process fake gateway, with `vscode`
// aliased to a stub at bundle time (see tests/run.mjs).
//
// Covers what unit tests cannot reach without an extension host: catalogue
// mapping, streaming, tool-call reassembly, the modelOptions allowlist, and
// the fail-closed branches.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { __settings, __ui } from 'vscode';

import { invalidateAuthState } from '../src/auth-state.ts';
import { registerSignInCommand, resetAuthNotifications } from '../src/auth-ui.ts';
import { NoCredentialError, getToken, resolveAuthState } from '../src/auth.ts';
import { invalidateCatalogue } from '../src/catalogue.ts';
import { readConfig } from '../src/config.ts';
import { LightbridgeChatProvider } from '../src/provider.ts';
import { registerStatusBar } from '../src/status-bar.ts';
import { startGateway } from './support/fake-gateway.ts';

// Supplied by tests/run.mjs. NOT derived from import.meta.url: this file is
// bundled into dist-test/, so a self-relative path resolves to a directory
// that does not contain the auth doubles.
const SUPPORT = process.env.LB_TEST_SUPPORT;
if (!SUPPORT) throw new Error('LB_TEST_SUPPORT is not set; run via tests/run.mjs');
const token = { isCancellationRequested: false, onCancellationRequested: () => ({ dispose() {} }) };

const gw = await startGateway();

// The UI affordances are registered the way `activate()` registers them, so the
// suite drives them through `executeCommand` and the status-bar item exactly as
// a notification button and VS Code itself would.
registerSignInCommand(new LightbridgeChatProvider());
registerStatusBar();

function configure(authScript: string, gatewayUrl = gw.url) {
  invalidateCatalogue();
  // The auth state is cached across calls now, so a scenario that did not
  // clear it would be answered by the previous scenario's session.
  invalidateAuthState();
  resetAuthNotifications();
  __ui.errorMessages.length = 0;
  __ui.terminals.length = 0;
  __ui.errorMessageResponse = undefined;
  __settings.gatewayUrl = gatewayUrl;
  __settings.governanceAuthPath = `${SUPPORT}${authScript}`;
  __settings.requestTimeoutMs = 15000;
  __settings.catalogueTtlMs = 0;
  __settings.modelOptionDefaults = { temperature: 0.2, top_k: 40 };
}

/** How many times a counting auth double recorded that it ran. */
function countLines(path: string): number {
  return readFileSync(path, 'utf8').split('\n').filter(Boolean).length;
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

await scenario("CREDENTIAL: governance-auth's stderr never reaches the error message", async () => {
  configure('bad-auth.sh');
  // The load-bearing test on this path. Node builds an `execFile` rejection's
  // message as `Command failed: <argv>\n<stderr>`, so passing that message
  // through — which this file did, under a comment claiming it did not — puts
  // every byte a credential tool printed into the output channel and, on the
  // chat path, the chat transcript.
  let thrown: unknown;
  try {
    await getToken(readConfig());
  } catch (err) {
    thrown = err;
  }

  assert.ok(thrown instanceof NoCredentialError, 'expected a NoCredentialError');
  assert.equal((thrown as NoCredentialError).kind, 'signed-out');
  const message = (thrown as Error).message;
  for (const leak of ['Command failed', 'no cached session', 'issuer/client', SUPPORT]) {
    assert.ok(!message.includes(leak), `stderr leaked into the message: ${JSON.stringify(message)}`);
  }
  assert.match(message, /signed out/i, 'says nothing useful of its own');
});

await scenario('CREDENTIAL: the cached state answers without spawning governance-auth again', async () => {
  configure('counting-auth.sh');
  const log = join(mkdtempSync(join(tmpdir(), 'lb-auth-')), 'spawns');
  writeFileSync(log, '');
  process.env.LB_SPAWN_LOG = log;
  try {
    const config = readConfig();
    for (let i = 0; i < 5; i++) {
      assert.equal((await resolveAuthState(config)).signedIn, true);
    }
    const spawns = countLines(log);
    // Measured at 5 before the cache existed — one process per picker query,
    // on top of the one the catalogue fetch already makes.
    assert.equal(spawns, 1, `expected 1 spawn for 5 state queries, got ${spawns}`);

    // What the sign-in command and a settings change do: the developer has just
    // changed the answer, and waiting out a TTL to notice would be its own bad
    // experience.
    invalidateAuthState();
    await resolveAuthState(config);
    assert.equal(countLines(log), 2, 'an invalidated state was reused instead of re-probed');
  } finally {
    delete process.env.LB_SPAWN_LOG;
  }
});

async function chatWhileSignedOut(): Promise<{ thrown: unknown; parts: unknown[] }> {
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
  return { thrown, parts };
}

await scenario('FAIL CLOSED: a chat request without a credential throws before any request', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  const { thrown, parts } = await chatWhileSignedOut();
  assert.ok(thrown, 'the request did not throw');
  assert.equal(parts.length, 0, 'content was streamed without a credential');
  // That it is OUR credential refusal rather than "the gateway said no" is
  // proved by the permissive probe never being contacted — asserted at the
  // bottom of this file for every scenario at once.
});

await scenario('SIGNED OUT: chat fails as a LanguageModelError naming the fix', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  const { thrown } = await chatWhileSignedOut();
  const vscode = await import('vscode');

  // A bare Error from a provider renders in chat as a generic "something went
  // wrong"; a LanguageModelError gets a first-class failure presentation. This
  // is the point of use, so it has to be the latter.
  assert.ok(
    thrown instanceof vscode.LanguageModelError,
    `expected a LanguageModelError, got ${(thrown as Error).name}`,
  );
  assert.equal((thrown as { code: string }).code, 'NoPermissions');

  const message = (thrown as Error).message;
  // (a) what happened, (b) it is not their code, (c) the exact command.
  assert.match(message, /no valid `governance-auth` session/i, 'does not say what happened');
  assert.match(message, /not a fault in your code/i, 'does not absolve the developer');
  // The configured binary path, not the literal word `governance-auth`: the
  // message names the binary this editor will actually run, which is the point
  // of naming it at all when `governanceAuthPath` has been overridden.
  assert.match(message, /bad-auth\.sh login/, 'does not name the command that fixes it');
  // And still no diagnostic from the credential tool.
  assert.ok(!message.includes('no cached session'), "governance-auth's stderr reached the chat error");
});

await scenario('SIGNED OUT: an empty picker is explained, with a button that fixes it', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  // `silent: false` is the developer asking out loud — they opened the picker.
  // Before this branch they got an empty list and a line in an output channel
  // they had no reason to open.
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );
  assert.equal(models.length, 0, 'models were offered without a credential');

  const shown = __ui.errorMessages.at(-1);
  assert.ok(shown, 'nothing was shown to the developer');
  assert.match(shown!.message, /signed out/i, `unhelpful notification: ${shown!.message}`);
  assert.ok(shown!.actions.includes('Sign in'), `no Sign in action: ${shown!.actions.join(', ')}`);
});

await scenario('SIGNED OUT: silent mode stays silent, but the status bar says so', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: true },
    token as never,
  );
  // No prompt: VS Code forbids UI here, and a popup nobody asked for is the
  // other way to get this wrong.
  assert.equal(__ui.errorMessages.length, 0, 'silent mode showed a notification');
  // But not invisible either — the silent path contributes no models, so the
  // developer never reaches a point of use where an error could be raised.
  assert.equal(__ui.statusBar.visible, true, 'the status bar did not report the problem');
  assert.match(__ui.statusBar.text, /signed out/i);
  assert.match(__ui.statusBar.tooltip, /bad-auth\.sh login/, 'the tooltip does not carry the fix');
});

await scenario('SIGNED OUT: the Sign in button puts the real command in a terminal', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  __ui.errorMessageResponse = 'Sign in';
  await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );

  const terminal = __ui.terminals.at(-1);
  assert.ok(terminal, 'no terminal was opened');
  assert.equal(terminal!.name, 'governance-auth');
  assert.equal(terminal!.shown, true, 'the terminal was created but not revealed');
  const sent = terminal!.sent.at(-1);
  assert.ok(sent, 'nothing was typed into the terminal');
  assert.match(sent!.text, /bad-auth\.sh login$/, `unexpected command: ${sent!.text}`);
  assert.equal(sent!.execute, true, 'the login command was typed but not run');
});

await scenario('MISSING BINARY: is not reported as being signed out', async () => {
  // The failure a desktop-launched VS Code produces. Telling this developer to
  // run `governance-auth login` is a loop with no exit, so it must not be the
  // message they get.
  configure('not-installed-anywhere.sh', gw.permissiveUrl);
  const models = await new LightbridgeChatProvider().provideLanguageModelChatInformation(
    { silent: false },
    token as never,
  );
  assert.equal(models.length, 0);

  const shown = __ui.errorMessages.at(-1);
  assert.ok(shown, 'nothing was shown to the developer');
  assert.match(shown!.message, /was not found/i, `wrong diagnosis: ${shown!.message}`);
  assert.ok(!/signed out/i.test(shown!.message), 'a missing binary was reported as being signed out');
  assert.ok(
    shown!.actions.includes('Open settings'),
    `the fix here is a setting, not a login: ${shown!.actions.join(', ')}`,
  );
});

await scenario('SIGNED OUT: one transition produces one notification, not one per call', async () => {
  configure('bad-auth.sh', gw.permissiveUrl);
  const provider = new LightbridgeChatProvider();
  for (let i = 0; i < 4; i++) {
    await provider.provideLanguageModelChatInformation({ silent: false }, token as never);
  }
  assert.equal(
    __ui.errorMessages.length,
    1,
    `expected 1 notification for 4 queries, got ${__ui.errorMessages.length}`,
  );
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
