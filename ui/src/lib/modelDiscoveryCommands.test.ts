import { beforeEach, describe, expect, it, vi } from 'vitest';
import { discoverModels } from './modelDiscoveryCommands.ts';
import type { DiscoverModelsRequest } from './contracts/commands.ts';
import type { ModelInfo } from './contracts/model.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

describe('model discovery command boundary', () => {
	beforeEach(() => invoke.mockReset());

	it('forwards the flat request and preserves model response extensions', async () => {
		const request: DiscoverModelsRequest = {
			baseUrl: 'https://models.example/v1',
			apiKey: 'key',
			providerName: 'custom-provider',
			requestKind: 'transcription',
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

	it('preserves an empty model catalog as a successful response', async () => {
		const emptyModels: ModelInfo[] = [];
		invoke.mockResolvedValue(emptyModels);

		await expect(discoverModels({ baseUrl: 'http://localhost/v1', apiKey: '' })).resolves.toBe(
			emptyModels,
		);
		expect(invoke).toHaveBeenCalledWith('discover_models', {
			baseUrl: 'http://localhost/v1',
			apiKey: '',
		});
	});
});
