import { getToken } from './auth.js';
import { errorMessage, log, redact } from './log.js';
import type { Config } from './config.js';
import type { CatalogueEntry, CatalogueResponse, LightbridgeModel } from './types.js';

interface Cached {
  readonly models: LightbridgeModel[];
  readonly at: number;
  readonly gatewayUrl: string;
}

let cache: Cached | undefined;

export function invalidateCatalogue(): void {
  cache = undefined;
}

/**
 * Fetch the model catalogue from the gateway and map it to VS Code's shape.
 *
 * Capability data — context window, vision, tool calling — comes from the
 * gateway on every fetch and is never compiled in. RFC-0003 §7 is explicit
 * about why: a capability table baked into a plugin rots silently the first
 * time a model changes, and the observable symptom is not "wrong metadata" but
 * a truncated conversation nobody traces back to the plugin.
 */
export async function fetchCatalogue(config: Config): Promise<LightbridgeModel[]> {
  const gatewayUrl = config.gatewayUrl;
  if (gatewayUrl === undefined) {
    log().warn('No lightbridge.gatewayUrl configured; contributing no models.');
    return [];
  }

  if (cache && cache.gatewayUrl === gatewayUrl && Date.now() - cache.at < config.catalogueTtlMs) {
    return cache.models;
  }

  const token = await getToken(config);
  const url = `${gatewayUrl}/models/info`;

  const res = await fetch(url, {
    headers: { authorization: `Bearer ${token}`, accept: 'application/json' },
  });

  if (!res.ok) {
    throw new Error(`Model catalogue at ${redact(url)} returned ${res.status}.`);
  }

  const body = (await res.json()) as CatalogueResponse;
  const models: LightbridgeModel[] = [];

  for (const entry of body.data ?? []) {
    const model = toModel(entry);
    if (model) {
      models.push(model);
    }
  }

  log().info(`Catalogue: ${models.length} model(s) from ${redact(url)}.`);
  cache = { models, at: Date.now(), gatewayUrl };
  return models;
}

/**
 * Map one catalogue entry, or drop it.
 *
 * A model whose context window the gateway does not report is **skipped**, not
 * defaulted. Substituting an assumed window is the exact defect behind the
 * "unrecognised model" warning in issue #151 — an assumed 200k window presented
 * as a model that works, right up until a long conversation is silently
 * truncated. Refusing to offer the model is the honest failure, and it puts the
 * fix where RFC-0003 §7 says it belongs: in the gateway's catalogue.
 */
function toModel(entry: CatalogueEntry): LightbridgeModel | undefined {
  const id = entry.model_name;
  const info = entry.model_info;

  if (!id) {
    return undefined;
  }

  const maxInputTokens = info?.max_input_tokens;
  const maxOutputTokens = info?.max_output_tokens;

  if (typeof maxInputTokens !== 'number' || typeof maxOutputTokens !== 'number') {
    log().warn(
      `Skipping model '${id}': the gateway catalogue reports no context window. ` +
        `Add max_input_tokens/max_output_tokens there rather than assuming one here.`,
    );
    return undefined;
  }

  return {
    id,
    name: id,
    family: entry.litellm_params?.model ?? id,
    version: info?.version ?? 'unknown',
    maxInputTokens,
    maxOutputTokens,
    capabilities: {
      imageInput: info?.supports_vision ?? false,
      toolCalling: info?.supports_function_calling ?? false,
    },
    detail: 'Lightbridge',
    tooltip: `${id} — served through the Lightbridge governed gateway`,
    upstreamId: id,
  };
}

/** Log a catalogue failure without letting it escape as a model list. */
export function logCatalogueFailure(err: unknown): void {
  log().error(`Model catalogue unavailable: ${errorMessage(err)}`);
}
