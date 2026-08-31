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

/**
 * The fetch currently in flight, if any, keyed by gateway.
 *
 * The result cache alone is not enough. It is only populated *after* the
 * await, so N calls that arrive before the first one resolves all miss it and
 * all hit the gateway — and VS Code queries
 * `provideLanguageModelChatInformation` from several contexts at once, so that
 * is the normal access pattern rather than a synthetic one. Measured: five
 * concurrent calls produced five catalogue fetches (and five
 * `governance-auth token` spawns) against a five-minute TTL.
 *
 * That matters more here than it would elsewhere. This repository already
 * disabled a per-request lookup in the Authorino path in production for being
 * on a hot path; the model picker is the editor's equivalent.
 */
let inflight: { readonly gatewayUrl: string; readonly promise: Promise<LightbridgeModel[]> } | undefined;

export function invalidateCatalogue(): void {
  cache = undefined;
  // Deliberately does NOT cancel an in-flight fetch. Its callers are already
  // awaiting it and cancelling would fail them for no reason; it simply stops
  // being reused, and its result is discarded by the gateway check below.
  inflight = undefined;
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

  // Join the fetch already running for this gateway rather than starting a
  // second one. Keyed on the URL so a settings change cannot make a caller
  // receive the previous gateway's catalogue.
  if (inflight && inflight.gatewayUrl === gatewayUrl) {
    return inflight.promise;
  }

  const promise = fetchFresh(config, gatewayUrl).finally(() => {
    if (inflight?.promise === promise) {
      inflight = undefined;
    }
  });
  inflight = { gatewayUrl, promise };
  return promise;
}

async function fetchFresh(config: Config, gatewayUrl: string): Promise<LightbridgeModel[]> {
  const token = await getToken(config);
  // `/v1/models/info`, verified live against the gateway. The first version of
  // this file used `/models/info`, which 404s — and a 404 here presents as an
  // empty picker, not as an error anyone traces to a URL.
  const url = `${gatewayUrl}/v1/models/info`;

  const res = await fetch(url, {
    headers: { authorization: `Bearer ${token}`, accept: 'application/json' },
  });

  if (!res.ok) {
    throw new Error(`Model catalogue at ${redact(url)} returned ${res.status}.`);
  }

  const body = (await res.json()) as CatalogueResponse;
  const entries = body.data ?? [];
  const models: LightbridgeModel[] = [];

  for (const entry of entries) {
    const model = toModel(entry);
    if (model) {
      models.push(model);
    }
  }

  // Entries came back but none mapped: say so loudly. This almost always means
  // the catalogue schema moved and this file is reading fields that no longer
  // exist — which is exactly what happened when it was written against an
  // assumed LiteLLM shape. Without this the symptom is a bare
  // "Catalogue: 0 model(s)", an empty picker, and no hint that the fault is
  // here rather than at the gateway.
  if (entries.length > 0 && models.length === 0) {
    log().error(
      `Catalogue at ${redact(url)} returned ${entries.length} entr(ies) but NONE could be ` +
        `mapped. Expected OpenRouter-shaped fields (id, context_length, ` +
        `top_provider.max_completion_tokens). This is likely a schema mismatch in the ` +
        `extension, not a gateway fault.`,
    );
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
  const id = entry.id;

  if (!id) {
    return undefined;
  }

  // Prefer the tighter of the nominal and serving-provider windows. They agree
  // today, but when they diverge the serving provider is the one that will
  // actually refuse the request, and picking the larger would advertise
  // capacity that does not exist.
  const windows = [entry.context_length, entry.top_provider?.context_length].filter(
    (n): n is number => typeof n === 'number' && n > 0,
  );
  const contextLength = windows.length > 0 ? Math.min(...windows) : undefined;
  const maxOutputTokens = entry.top_provider?.max_completion_tokens;

  if (typeof contextLength !== 'number' || typeof maxOutputTokens !== 'number') {
    log().warn(
      `Skipping model '${id}': the gateway catalogue reports no usable context window ` +
        `(needs context_length and top_provider.max_completion_tokens). ` +
        `Add them there rather than assuming a window here.`,
    );
    return undefined;
  }

  // VS Code treats maxInputTokens + maxOutputTokens as the model's TOTAL
  // context window and renders their sum in the picker. `context_length` is
  // that whole window; `max_completion_tokens` is the reply's share of it, not
  // an addition to it.
  //
  // Reporting the window as maxInputTokens therefore ADDS the output budget on
  // top and over-advertises — measured in the picker as 264k for a 200k model
  // before this was fixed. VS Code would then pack a full-window prompt while
  // reserving output on top, and the request would be refused upstream. That is
  // the same silent-truncation failure the skip-don't-default rule above exists
  // to prevent, arrived at from the other direction.
  //
  // Subtracting keeps the displayed sum equal to the real window.
  const usableInputTokens = contextLength - maxOutputTokens;

  if (usableInputTokens <= 0) {
    log().warn(
      `Skipping model '${id}': max_completion_tokens (${maxOutputTokens}) leaves no input ` +
        `budget within a context_length of ${contextLength}.`,
    );
    return undefined;
  }

  const supportedParameters = entry.supported_parameters ?? [];

  return {
    id,
    // The catalogue's human-readable name when it has one; the id is a slug.
    name: entry.name ?? id,
    family: id,
    version: 'unknown',
    maxInputTokens: usableInputTokens,
    maxOutputTokens,
    capabilities: {
      // Modality and tool support are declared by the catalogue, not inferred
      // from the id. Absent means absent, never assumed true.
      imageInput: entry.architecture?.input_modalities?.includes('image') ?? false,
      toolCalling: supportedParameters.includes('tools'),
    },
    detail: 'Lightbridge',
    tooltip: `${entry.name ?? id} — served through the Lightbridge governed gateway`,
    upstreamId: id,
    supportedParameters,
  };
}

/** Log a catalogue failure without letting it escape as a model list. */
export function logCatalogueFailure(err: unknown): void {
  log().error(`Model catalogue unavailable: ${errorMessage(err)}`);
}
