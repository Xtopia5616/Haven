import { describe, expect, it, vi } from 'vitest';
import { createChatModelSync } from './chatModelSync.ts';
import type { ModelInfo } from './contracts/model.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

function settingsFor(baseUrl: string, providerName: string) {
	return {
		llm: {
			request_policies: [{ request: 'chat', primary: 'chat-slot' }],
			models: [{ id: 'chat-slot', provider: providerName, model: 'default-model' }],
			providers: [{ name: providerName, base_url: baseUrl }],
		},
	};
}

function createSync(setModelOptions: (models: ModelInfo[]) => void) {
	return createChatModelSync({
		isDead: () => false,
		setModelOptions,
		setCurrentModelId: vi.fn(),
		setCurrentModelName: vi.fn(),
		setCurrentEffort: vi.fn(),
		setCurrentWebSearch: vi.fn(),
		setWebSearchSupported: vi.fn(),
		setCurrentApiStyle: vi.fn(),
	});
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

describe('chat model discovery sync', () => {
	it('coalesces same-URL requests and caches an empty model list', async () => {
		const pending = deferred<ModelInfo[]>();
		invoke.mockReset().mockReturnValue(pending.promise);
		const firstOptions = vi.fn();
		const secondOptions = vi.fn();
		const baseUrl = 'https://coalesce.example/v1';

		createSync(firstOptions).applyDefaultModelFromSettings(settingsFor(baseUrl, 'first'));
		createSync(secondOptions).applyDefaultModelFromSettings(settingsFor(baseUrl, 'second'));
		expect(invoke).toHaveBeenCalledTimes(1);
		expect(invoke).toHaveBeenCalledWith('discover_models', {
			baseUrl,
			apiKey: '',
			provider: 'first',
			role: 'chat',
		});

		pending.resolve([]);
		await vi.waitFor(() => expect(firstOptions).toHaveBeenCalledWith([]));
		await vi.waitFor(() => expect(secondOptions).toHaveBeenCalledWith([]));

		const cachedOptions = vi.fn();
		createSync(cachedOptions).applyDefaultModelFromSettings(settingsFor(baseUrl, 'third'));
		expect(cachedOptions).toHaveBeenCalledWith([]);
		expect(invoke).toHaveBeenCalledTimes(1);
	});

	it('ignores a stale response after the default provider URL changes', async () => {
		const stale = deferred<ModelInfo[]>();
		const current = deferred<ModelInfo[]>();
		invoke
			.mockReset()
			.mockImplementation((_command: string, args?: { baseUrl?: string }) =>
				args?.baseUrl === 'https://old.example/v1' ? stale.promise : current.promise,
			);
		const modelOptions = vi.fn();
		const sync = createSync(modelOptions);
		const oldUrl = 'https://old.example/v1';
		const newUrl = 'https://new.example/v1';
		const currentModels: Array<ModelInfo & { provider_metadata: { source: string } }> = [
			{
				id: 'new-model',
				provider: 'new-provider',
				name: 'New Model',
				context_window: 0,
				supports_streaming: false,
				supports_tools: false,
				supports_vision: false,
				provider_metadata: { source: 'provider' },
			},
		];

		sync.applyDefaultModelFromSettings(settingsFor(oldUrl, 'old-provider'));
		sync.applyDefaultModelFromSettings(settingsFor(newUrl, 'new-provider'));
		stale.resolve([]);
		await Promise.resolve();
		expect(modelOptions).not.toHaveBeenCalled();

		current.resolve(currentModels);
		await vi.waitFor(() => expect(modelOptions).toHaveBeenCalledWith(currentModels));
		expect(modelOptions).not.toHaveBeenCalledWith([]);
		expect(currentModels[0].provider_metadata).toEqual({ source: 'provider' });
	});
});
