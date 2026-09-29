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

export type RequestPolicyDraft = Required<RequestPolicyInput>;

/** Mutable UI draft for the model settings surface before command validation. */
export type SettingsLlmState = Omit<LlmConfigInput, 'providers' | 'models' | 'request_policies'> & {
	providers: ProviderDraft[];
	models: ModelDraft[];
	request_policies: RequestPolicyDraft[];
	max_concurrent_requests: number;
};
