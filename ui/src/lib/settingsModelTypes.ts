import type {
	CapabilityInput,
	LlmConfigInput,
	ModelConfigInput,
	ProviderConfigInput,
	RequestPolicyInput,
} from './contracts/generatedCommands.ts';

export type ModelDraft = Omit<ModelConfigInput, 'id' | 'provider' | 'model' | 'capabilities'> & {
	id: string;
	provider: string;
	model: string;
	capabilities: CapabilityInput[];
};

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
