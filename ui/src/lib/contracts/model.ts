/** Model metadata returned by the Rust `ModelInfo` wire DTO. */
export interface ModelInfo {
	id: string;
	provider: string;
	name: string;
	context_window: number;
	supports_streaming: boolean;
	supports_tools: boolean;
	supports_vision: boolean;
	cost_per_1k_input_tokens?: number;
	cost_per_1k_output_tokens?: number;
	[key: string]: unknown;
}

/** Discovered model lists keyed by the configured provider name. */
export type DiscoveredModelMap = Record<string, ModelInfo[]>;
