import type {
	CapabilityInput,
	LlmConfig,
	LlmConfigInput,
	ModelConfig,
	ModelConfigInput,
	ProviderConfigInput,
	RequestPolicyInput,
} from './contracts/generatedCommands.ts';

export type ModelDraft = Omit<ModelConfigInput, 'id' | 'provider_name' | 'model' | 'capabilities'> & {
	id: string;
	providerName: string;
	model: string;
	capabilities: CapabilityInput[];
};

/** Project the Rust-owned `provider_name` wire key into the settings editor name. */
export function modelDraftFromConfig(model: ModelConfig): ModelDraft {
	const { provider_name: providerName, ...fields } = model;
	return { ...fields, providerName };
}

/** Restore the Rust-owned `provider_name` key at the update_settings boundary. */
export function modelConfigInputFromDraft(model: ModelDraft): ModelConfigInput {
	const { providerName, ...fields } = model;
	return { ...fields, provider_name: providerName };
}

/** Convert a loaded wire config once before it enters the settings editor. */
export function settingsLlmStateFromConfig(config: LlmConfig): SettingsLlmState {
	return {
		...config,
		models: config.models.map(modelDraftFromConfig),
	};
}

export type ProviderDraft = ProviderConfigInput & {
	name: string;
	base_url: string;
};

/** UI-only provider editor state; `proxy_mode` is projected into persisted proxy fields on save. */
export interface ProviderDialogForm {
	name: string;
	api_style: string;
	base_url: string;
	api_key: string;
	proxy_mode: 'system' | 'direct' | 'custom';
	proxy_url: string;
	no_proxy: string;
}

/** Fields needed to decide whether a provider can make authenticated requests. */
export type ProviderKeyCheckInput = Pick<
	ProviderDraft,
	'name' | 'provider' | 'api_style' | 'api_key' | 'api_key_ref'
>;

/** Model settings fields that the UI lets a user override per model. */
export type ModelOverrideField =
	| 'temperature'
	| 'context_window'
	| 'cost_per_1k_input_tokens'
	| 'cost_per_1k_output_tokens'
	| 'cost_per_1k_cache_read_tokens'
	| 'cost_per_1k_cache_write_tokens';

export type RequestPolicyDraft = Required<RequestPolicyInput>;

/** Mutable UI draft for the model settings surface before command validation. */
export type SettingsLlmState = Omit<LlmConfigInput, 'providers' | 'models' | 'request_policies'> & {
	providers: ProviderDraft[];
	models: ModelDraft[];
	request_policies: RequestPolicyDraft[];
	max_concurrent_requests: number;
};

/** Convert editor state back to the stable Rust-owned command shape. */
export function settingsLlmInputFromState(config: SettingsLlmState): LlmConfigInput {
	return {
		...config,
		models: config.models.map(modelConfigInputFromDraft),
	};
}
