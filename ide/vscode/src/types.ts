import type { LanguageModelChatInformation } from 'vscode';

/**
 * Our own model type, carried through VS Code and handed back to us.
 *
 * `LanguageModelChatProvider<T>` is generic and `provideLanguageModelChatResponse`
 * receives the same object we returned from `provideLanguageModelChatInformation`,
 * so anything we hang here is available at request time without a second lookup.
 */
export interface LightbridgeModel extends LanguageModelChatInformation {
  /** The id to send upstream, which need not equal the id VS Code displays. */
  readonly upstreamId: string;
  /**
   * The parameter names this model declares support for, from the catalogue.
   * Empty means "the catalogue did not say", which is treated as "forward
   * nothing" rather than "forward everything".
   */
  readonly supportedParameters: readonly string[];
}

/**
 * One entry of the gateway's model catalogue.
 *
 * This is the OpenRouter-shaped schema the gateway actually serves — verified
 * against the live response, after a first version of this file was written
 * against an assumed LiteLLM shape (`model_name`, `model_info.max_input_tokens`)
 * that exists nowhere in it. That mismatch is silent in the worst way: every
 * entry fails to map, every model is skipped, and the log says the catalogue
 * "reports no context window" rather than "this code is reading the wrong
 * fields".
 *
 * Deliberately all-optional: this is somebody else's wire format and a missing
 * field is a fact to handle, not a parse failure.
 */
export interface CatalogueEntry {
  readonly id?: string;
  readonly name?: string;
  /** Nominal context window for the model. */
  readonly context_length?: number;
  readonly architecture?: {
    readonly input_modalities?: readonly string[];
    readonly output_modalities?: readonly string[];
  };
  /** The serving provider's actual limits, which can be tighter than nominal. */
  readonly top_provider?: {
    readonly context_length?: number;
    readonly max_completion_tokens?: number;
  };
  /**
   * Sampling and request parameters this model accepts, e.g. `temperature`,
   * `top_k`, `tools`. Used both to advertise tool calling and as the allowlist
   * for what may be forwarded from `modelOptions`.
   */
  readonly supported_parameters?: readonly string[];
  /**
   * Present in the live response as decimal strings. Deliberately NOT read:
   * cost is the gateway's to compute in integer micro-USD (ADR-0008), and
   * parsing these into a float here would put a float next to a monetary value
   * in the one place the rule forbids it.
   */
  readonly pricing?: unknown;
}

export interface CatalogueResponse {
  readonly data?: readonly CatalogueEntry[];
}

/** An OpenAI-compatible chat message, as sent upstream. */
export interface WireMessage {
  role: 'system' | 'user' | 'assistant' | 'tool';
  content: string | null;
  tool_call_id?: string;
  tool_calls?: WireToolCall[];
}

export interface WireToolCall {
  id: string;
  type: 'function';
  function: { name: string; arguments: string };
}

/** A `tools[]` entry, as sent upstream. */
export interface WireTool {
  type: 'function';
  function: { name: string; description: string; parameters: object };
}

/** One `chat.completion.chunk` streamed back from the gateway. */
export interface WireChunk {
  choices?: ReadonlyArray<{
    delta?: {
      content?: string | null;
      tool_calls?: ReadonlyArray<{
        index?: number;
        id?: string;
        function?: { name?: string; arguments?: string };
      }>;
    };
    finish_reason?: string | null;
  }>;
}
