import * as vscode from 'vscode';

export interface Config {
  readonly gatewayUrl: string | undefined;
  readonly governanceAuthPath: string;
  readonly requestTimeoutMs: number;
  readonly catalogueTtlMs: number;
  readonly modelOptionDefaults: Readonly<Record<string, unknown>>;
}

const SECTION = 'lightbridge';

/**
 * Read settings.
 *
 * `gatewayUrl` stays `undefined` when unset rather than acquiring a default.
 * A compiled-in fallback endpoint would mean a misconfigured install silently
 * talks to somebody else's gateway; the caller's job is to contribute no
 * models at all in that case.
 */
export function readConfig(): Config {
  const cfg = vscode.workspace.getConfiguration(SECTION);

  return {
    gatewayUrl: normalizeBase(cfg.get<string | null>('gatewayUrl') ?? undefined),
    governanceAuthPath: cfg.get<string>('governanceAuthPath') ?? 'governance-auth',
    requestTimeoutMs: cfg.get<number>('requestTimeoutMs') ?? 120_000,
    catalogueTtlMs: cfg.get<number>('catalogueTtlMs') ?? 300_000,
    modelOptionDefaults: cfg.get<Record<string, unknown>>('modelOptionDefaults') ?? {},
  };
}

/** Fires when any `lightbridge.*` setting changes. */
export function onConfigChange(listener: () => void): vscode.Disposable {
  return vscode.workspace.onDidChangeConfiguration((e) => {
    if (e.affectsConfiguration(SECTION)) {
      listener();
    }
  });
}

/**
 * Trim a trailing slash so path joins do not produce `//v1/models`.
 *
 * Some gateways route that as a distinct path and 404 it, which presents as
 * "the catalogue is empty" rather than as a URL bug.
 */
function normalizeBase(url: string | undefined): string | undefined {
  if (url === undefined || url.trim() === '') {
    return undefined;
  }
  return url.trim().replace(/\/+$/, '');
}
