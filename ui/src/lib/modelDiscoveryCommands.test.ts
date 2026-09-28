import { beforeEach, describe, expect, it, vi } from 'vitest';
import { discoverAllModels, discoverModels } from './modelDiscoveryCommands.ts';
import type { DiscoverModelsRequest } from './contracts/commands.ts';
import type { DiscoveredModelMap, ModelInfo } from './contracts/model.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

describe('model discovery command boundary', () => {
	beforeEach(() => invoke.mockReset());

	it('forwards the flat request and preserves model response extensions', async () => {
		const request: DiscoverModelsRequest = {
			baseUrl: 'https://models.example/v1',
			apiKey: 'key',
			provider: 'custom-provider',
			role: 'transcription',
		};
		const models: Array<ModelInfo & { provider_metadata: { tier: string } }> = [
			{
				id: 'audio-model',
				provider: 'custom-provider',
				name: 'Audio Model',
				context_window: 0,
				supports_streaming: false,
				supports_tools: false,
				supports_vision: false,
				provider_metadata: { tier: 'custom' },
			},
		];
		invoke.mockResolvedValue(models);

		await expect(discoverModels(request)).resolves.toBe(models);
		expect(invoke).toHaveBeenCalledWith('discover_models', request);
		expect(models[0].provider_metadata).toEqual({ tier: 'custom' });
	});

	it('preserves empty provider and model results', async () => {
		const emptyModels: ModelInfo[] = [];
		const emptyProviders: DiscoveredModelMap = { configured: emptyModels };
		invoke.mockResolvedValueOnce(emptyModels).mockResolvedValueOnce(emptyProviders);

		await expect(discoverModels({ baseUrl: 'http://localhost/v1', apiKey: '' })).resolves.toBe(
			emptyModels,
		);
		await expect(discoverAllModels()).resolves.toBe(emptyProviders);
		expect(invoke).toHaveBeenNthCalledWith(1, 'discover_models', {
			baseUrl: 'http://localhost/v1',
			apiKey: '',
		});
		expect(invoke).toHaveBeenNthCalledWith(2, 'discover_all_models');
	});
});
