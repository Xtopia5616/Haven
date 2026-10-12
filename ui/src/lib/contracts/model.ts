/** Provider model catalog projected into renderer naming and ownership terms. */
import type { ModelInfo as GeneratedModelInfo } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isFiniteNumber, isNonEmptyString, isString, isU32, isBoolean } from './valueGuards.ts';

export interface DiscoveredModelView {
	/** Opaque model identifier assigned by the provider; preserve it exactly. */
	providerModelId: string;
	modelProvider: string;
	displayName: string;
	contextWindow: number;
	supportsStreaming: boolean;
	supportsTools: boolean;
	supportsVision: boolean;
	costPer1kInputTokens?: number;
	costPer1kOutputTokens?: number;
}

type ValidatedModelInfo = GeneratedModelInfo;

function isOptionalPrice(value: unknown): value is number | undefined {
	return value === undefined || (isFiniteNumber(value) && value >= 0);
}

function isModelInfo(value: unknown): value is ValidatedModelInfo {
	return (
		isRecord(value) &&
		isNonEmptyString(value.id) &&
		isString(value.provider) &&
		isString(value.name) &&
		isU32(value.context_window) &&
		isBoolean(value.supports_streaming) &&
		isBoolean(value.supports_tools) &&
		isBoolean(value.supports_vision) &&
		isOptionalPrice(value.cost_per_1k_input_tokens) &&
		isOptionalPrice(value.cost_per_1k_output_tokens)
	);
}

function toDiscoveredModelView(model: ValidatedModelInfo): DiscoveredModelView {
	return {
		providerModelId: model.id,
		modelProvider: model.provider,
		displayName: model.name,
		contextWindow: model.context_window,
		supportsStreaming: model.supports_streaming,
		supportsTools: model.supports_tools,
		supportsVision: model.supports_vision,
		...(model.cost_per_1k_input_tokens === undefined
			? {}
			: { costPer1kInputTokens: model.cost_per_1k_input_tokens }),
		...(model.cost_per_1k_output_tokens === undefined
			? {}
			: { costPer1kOutputTokens: model.cost_per_1k_output_tokens }),
	};
}

/** Validate Rust model rows and map wire fields to an explicit renderer view. */
export function parseDiscoveredModels(value: unknown): DiscoveredModelView[] {
	if (!Array.isArray(value)) throw new Error('Invalid model discovery response');

	const modelIds = new Set<string>();
	return value.map((model: unknown) => {
		if (!isModelInfo(model)) throw new Error('Invalid discovered model');
		if (modelIds.has(model.id)) throw new Error('Duplicate provider model ID');
		modelIds.add(model.id);
		return toDiscoveredModelView(model);
	});
}

/** Last successful catalogs cached by configured Provider connection name. */
export type DiscoveredModelsByProviderName = Record<string, DiscoveredModelView[]>;
