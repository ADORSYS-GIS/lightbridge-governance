// Bundles the integration suite with `vscode` aliased to the test stub, then
// runs it. The alias is why this needs a build step: `vscode` only exists
// inside an extension host, so without it the provider cannot be imported at
// all and none of its logic is testable outside an editor.
import { spawnSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import esbuild from 'esbuild';

const here = fileURLToPath(new URL('.', import.meta.url));
const out = `${here}../dist-test/integration.mjs`;

mkdirSync(`${here}../dist-test`, { recursive: true });

await esbuild.build({
  entryPoints: [`${here}integration.ts`],
  bundle: true,
  outfile: out,
  alias: { vscode: `${here}support/vscode-stub.mjs` },
  platform: 'node',
  format: 'esm',
  target: 'node20',
  logLevel: 'warning',
});

const res = spawnSync(process.execPath, [out], {
  stdio: 'inherit',
  env: { ...process.env, LB_TEST_SUPPORT: `${here}support/` },
});
process.exit(res.status ?? 1);
