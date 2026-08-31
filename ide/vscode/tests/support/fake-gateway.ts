// A stand-in for the Lightbridge gateway.
//
// Lives in tests/support/, never in src/ — a test double reachable from a
// production path is what the house rules forbid. Nothing in src/ imports this.
//
// Runs in-process and listens on port 0, so the suite has no fixed ports, no
// subprocess lifecycle and no log parsing: assertions read `requests` directly.
import http from 'node:http';

export interface CapturedRequest {
  method: string;
  url: string;
  body?: Record<string, unknown>;
}

/**
 * Mirrors the shape the live gateway serves at /v1/models/info, verified
 * against https://api.ai.camer.digital/v1/models/info.
 *
 * An earlier version of this fixture invented a LiteLLM shape (`model_name`,
 * `model_info.max_input_tokens`) that the gateway does not use, so the suite
 * was green against a schema that exists nowhere. Keep this in step with the
 * live response, not with what the mapper happens to expect.
 */
export const CATALOGUE = {
  data: [
    {
      id: 'governed-sonnet',
      name: 'Governed Sonnet (200k context)',
      context_length: 200000,
      architecture: { input_modalities: ['text', 'image'], output_modalities: ['text'] },
      pricing: { prompt: '0.00000300', completion: '0.00001500' },
      supported_parameters: ['tools', 'tool_choice', 'temperature', 'top_p', 'top_k', 'seed'],
      top_provider: { context_length: 200000, max_completion_tokens: 64000 },
    },
    {
      // No context_length and no top_provider: must be SKIPPED, not defaulted.
      id: 'no-window-model',
      name: 'Undescribed model',
      architecture: { input_modalities: ['text'], output_modalities: ['text'] },
      supported_parameters: ['temperature'],
    },
  ],
};

const listen = (server: http.Server): Promise<number> =>
  new Promise((resolve) =>
    server.listen(0, '127.0.0.1', () => resolve((server.address() as { port: number }).port)),
  );

/**
 * Start the strict gateway and a permissive control probe.
 *
 * The control probe accepts ANY request and records it. Fail-closed scenarios
 * point there because, against the strict gateway, they passed even with a
 * deliberate fail-closed bypass injected into auth.ts: the extension sent an
 * empty bearer, the gateway 401'd, and that 401 produced exactly the empty
 * list and the throw the assertions looked for. They were measuring the
 * gateway's strictness, not the extension's refusal. A correct extension never
 * contacts the probe at all.
 */
export async function startGateway() {
  const requests: CapturedRequest[] = [];
  const permissiveHits: string[] = [];

  const strict = http.createServer((req, res) => {
    const auth = req.headers.authorization;
    if (!auth || !auth.startsWith('Bearer ')) {
      res.writeHead(401, { 'content-type': 'application/json' });
      return res.end(JSON.stringify({ error: 'missing bearer' }));
    }

    if (req.method === 'GET' && req.url === '/v1/models/info') {
      requests.push({ method: 'GET', url: req.url ?? '' });
      res.writeHead(200, { 'content-type': 'application/json' });
      return res.end(JSON.stringify(CATALOGUE));
    }

    if (req.method === 'POST' && req.url === '/v1/chat/completions') {
      let raw = '';
      req.on('data', (c) => (raw += c));
      req.on('end', () => {
        const body = JSON.parse(raw);
        requests.push({ method: 'POST', url: req.url ?? '', body });

        // Offer a tool call only when no tool result is present yet. An
        // unconditional tool call sent Copilot's agent mode into an infinite
        // loop during manual testing — 51 requests before it was killed. A
        // fake that always says "call this tool" is a trap, not a fake.
        const alreadyCalled = ((body.messages ?? []) as Array<{ role?: string }>).some((m) => m.role === 'tool');

        res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
        const frame = (o: unknown) => `data: ${JSON.stringify(o)}\n\n`;

        // Split mid-word across events.
        res.write(frame({ choices: [{ delta: { content: 'Hel' } }] }));
        res.write(frame({ choices: [{ delta: { content: 'lo.' } }] }));

        if (alreadyCalled) {
          res.write('data: [DONE]\n\n');
          return res.end();
        }

        // Arguments arrive as JSON FRAGMENTS across three events, and the last
        // frame's terminator is split across two writes. This is the case the
        // SSE framer exists for.
        res.write(
          frame({
            choices: [
              {
                delta: {
                  tool_calls: [
                    { index: 0, id: 'call_1', function: { name: 'read_file', arguments: '{"pa' } },
                  ],
                },
              },
            ],
          }),
        );
        res.write(
          frame({
            choices: [
              { delta: { tool_calls: [{ index: 0, function: { arguments: 'th":"src/a' } }] } },
            ],
          }),
        );

        const tail = frame({
          choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: '.ts"}' } }] } }],
        });
        res.write(tail.slice(0, -1));
        setTimeout(() => {
          res.write(tail.slice(-1));
          res.write('data: [DONE]\n\n');
          res.end();
        }, 10);
      });
      return;
    }

    res.writeHead(404);
    res.end();
  });

  const permissive = http.createServer((req, res) => {
    permissiveHits.push(`${req.method} ${req.url}`);
    if (req.method === 'GET' && req.url === '/v1/models/info') {
      res.writeHead(200, { 'content-type': 'application/json' });
      return res.end(JSON.stringify(CATALOGUE));
    }
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write(`data: ${JSON.stringify({ choices: [{ delta: { content: 'leaked' } }] })}\n\n`);
    res.write('data: [DONE]\n\n');
    res.end();
  });

  const strictPort = await listen(strict);
  const permissivePort = await listen(permissive);

  return {
    url: `http://127.0.0.1:${strictPort}`,
    permissiveUrl: `http://127.0.0.1:${permissivePort}`,
    requests,
    permissiveHits,
    close: () =>
      Promise.all([
        new Promise((r) => strict.close(r)),
        new Promise((r) => permissive.close(r)),
      ]),
  };
}
