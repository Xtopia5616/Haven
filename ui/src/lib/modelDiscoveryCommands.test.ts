import { beforeEach, describe, expect, it, vi } from 'vitest';
import { discoverModels } from './modelDiscoveryCommands.ts';
import type { TauriCommandRequest } from './contracts/generatedCommands.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

const wireModel = {
	id: 'vendor/model:v2',
	provider: 'custom-provider',
	name: 'Audio Model',
	context_window: 0,
	supports_streaming: false,
	supports_tools: false,
	supports_vision: false,
	cost_per_1k_input_tokens: 0.25,
	cost_per_1k_output_tokens: 0.75,
};

describe('model discovery command boundary', () => {
	beforeEach(() => invoke.mockReset());

	it('forwards the flat request and projects provider metadata to the renderer view', async () => {
		const request: TauriCommandRequest<'discover_models'> = {
			baseUrl: 'https://models.example/v1',
			apiKey: 'key',
			providerName: 'custom-provider',
			requestKind: 'transcription',
		};
		invoke.mockResolvedValue([{ ...wireModel, provider_metadata: { tier: 'custom' } }]);

		await expect(discoverModels(request)).resolves.toEqual([
			{
				providerModelId: 'vendor/model:v2',
				modelProvider: 'custom-provider',
				displayName: 'Audio Model',
				contextWindow: 0,
				supportsStreaming: false,
				supportsTools: false,
				supportsVision: false,
				costPer1kInputTokens: 0.25,
				costPer1kOutputTokens: 0.75,
			},
		]);
		expect(invoke).toHaveBeenCalledWith('discover_models', request);
	});

	it('preserves an empty model catalog as a successful response', async () => {
		invoke.mockResolvedValue([]);

		await expect(
			discoverModels({ baseUrl: 'http://localhost/v1', apiKey: '' }),
		).resolves.toEqual([]);
		expect(invoke).toHaveBeenCalledWith('discover_models', {
			baseUrl: 'http://localhost/v1',
			apiKey: '',
		});
	});

	it.each([
		'not-an-array',
		[null],
		[{ ...wireModel, id: '' }],
		[{ ...wireModel, context_window: -1 }],
		[{ ...wireModel, context_window: 0x1_0000_0000 }],
		[{ ...wireModel, supports_tools: 'yes' }],
		[{ ...wireModel, cost_per_1k_input_tokens: Number.POSITIVE_INFINITY }],
		[{ ...wireModel, cost_per_1k_output_tokens: -0.01 }],
	])('rejects malformed discovery response %j', async (response) => {
		invoke.mockResolvedValue(response);

		await expect(
			discoverModels({ baseUrl: 'https://models.example/v1', apiKey: '' }),
		).rejects.toThrow();
	});

	it('rejects duplicate provider model IDs while preserving opaque IDs exactly', async () => {
		invoke.mockResolvedValue([wireModel, { ...wireModel, name: 'Duplicate' }]);
		await expect(
			discoverModels({ baseUrl: 'https://models.example/v1', apiKey: '' }),
		).rejects.toThrow('Duplicate provider model ID');

		invoke.mockResolvedValue([{ ...wireModel, id: ' Vendor/Model :v2 ' }]);
		await expect(
			discoverModels({ baseUrl: 'https://models.example/v1', apiKey: '' }),
		).resolves.toMatchObject([{ providerModelId: ' Vendor/Model :v2 ' }]);
	});
});
