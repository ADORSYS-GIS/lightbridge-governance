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
}

/**
 * One entry of the gateway's `/models/info` response.
 *
 * Deliberately all-optional below `model_name`: this is somebody else's wire
 * format and a missing field is a fact to handle, not a parse failure.
 */
export interface CatalogueEntry {
  readonly model_name?: string;
  readonly model_info?: {
    readonly max_input_tokens?: number;
    readonly max_output_tokens?: number;
    readonly supports_vision?: boolean;
    readonly supports_function_calling?: boolean;
    readonly version?: string;
  };
  readonly litellm_params?: {
    readonly model?: string;
  };
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
