import { describe, expect, it, vi } from 'vitest';
import { createChatModelSync } from './chatModelSync.ts';
import type { ChatModelOption } from './chatModelOperations.ts';
import type { SettingsPayload } from './contracts/settings.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

function settingsFor(options: {
	primary?: string;
	models?: Array<Record<string, unknown>>;
	providers?: Array<Record<string, unknown>>;
} = {}) {
	return {
		llm: {
			request_policies: options.primary
				? [{ request: 'chat', primary: options.primary }]
				: [],
			models: options.models || [],
			providers: options.providers || [],
		},
	} as unknown as SettingsPayload;
}

function createSync(setModelOptions: (models: ChatModelOption[]) => void) {
	const state = {
		modelIds: [] as string[],
		modelNames: [] as string[],
		efforts: [] as string[],
		webSearch: [] as string[],
		webSearchSupported: [] as boolean[],
		apiStyles: [] as string[],
	};
	const sync = createChatModelSync({
		isDead: () => false,
		setModelOptions,
		setCurrentModelId: (value) => state.modelIds.push(value),
		setCurrentModelName: (value) => state.modelNames.push(value),
		setCurrentEffort: (value) => state.efforts.push(value),
		setCurrentWebSearch: (value) => state.webSearch.push(value),
		setWebSearchSupported: (value) => state.webSearchSupported.push(value),
		setCurrentApiStyle: (value) => state.apiStyles.push(value),
	});
	return { sync, state };
}

describe('chat model route sync', () => {
	it('lists configured chat profiles and selects the route primary by profile id', () => {
		invoke.mockReset();
		const modelOptions = vi.fn();
		const { sync, state } = createSync(modelOptions);

		sync.applyDefaultModelFromSettings(
			settingsFor({
				primary: 'chat-profile',
				providers: [
					{ name: 'cloud', api_style: 'openai-responses' },
					{ name: 'local', api_style: 'openai-chat' },
				],
				models: [
					{
						id: 'chat-profile',
						provider_name: 'cloud',
						model: 'gpt-5',
						capabilities: ['chat'],
						reasoning_effort: 'high',
						web_search: 'auto',
					},
					{
						id: 'local-chat',
						provider_name: 'local',
						model: 'llama3',
						capabilities: ['chat'],
					},
					{
						id: 'fast-only',
						provider_name: 'cloud',
						model: 'gpt-5-mini',
						capabilities: ['fast_chat'],
					},
					{
						id: 'incomplete-chat',
						provider_name: 'cloud',
						model: '',
						capabilities: ['chat'],
					},
				],
			}),
		);

		expect(modelOptions).toHaveBeenCalledWith([
			{
				id: 'chat-profile',
				name: 'chat-profile',
				providerName: 'cloud',
				model: 'gpt-5',
				reasoningEffort: 'high',
				webSearch: 'auto',
				apiStyle: 'openai-responses',
				webSearchSupported: true,
			},
			{
				id: 'local-chat',
				name: 'local-chat',
				providerName: 'local',
				model: 'llama3',
				reasoningEffort: '',
				webSearch: 'off',
				apiStyle: 'openai-chat',
				webSearchSupported: false,
			},
		]);
		expect(state.modelIds).toEqual(['chat-profile']);
		expect(state.modelNames).toEqual(['chat-profile']);
		expect(state.efforts).toEqual(['high']);
		expect(state.webSearch).toEqual(['auto']);
		expect(state.webSearchSupported).toEqual([true]);
		expect(state.apiStyles).toEqual(['openai-responses']);
		expect(invoke).not.toHaveBeenCalledWith('discover_models', expect.anything());
	});

	it('keeps selectable profiles visible when no chat primary is configured', () => {
		let capturedOptions: ChatModelOption[] = [];
		const { sync, state } = createSync((models) => (capturedOptions = models));

		sync.applyDefaultModelFromSettings(
			settingsFor({
				providers: [{ name: 'cloud', api_style: 'openai-responses' }],
				models: [
					{
						id: 'chat-profile',
						provider_name: 'cloud',
						model: 'gpt-5',
						capabilities: ['chat'],
					},
				],
			}),
		);

		expect(capturedOptions.map((model) => model.id)).toEqual(['chat-profile']);
		expect(state.modelIds).toEqual(['']);
		expect(state.modelNames).toEqual(['']);
	});

	it('clears a stale web-search mode on an unsupported selected profile', () => {
		invoke.mockReset().mockResolvedValue(undefined);
		const { sync, state } = createSync(vi.fn());

		sync.applyDefaultModelFromSettings(
			settingsFor({
				primary: 'chat-profile',
				providers: [{ name: 'cloud', api_style: 'openai-chat' }],
				models: [
					{
						id: 'chat-profile',
						provider_name: 'cloud',
						model: 'gpt-5',
						capabilities: ['chat'],
						web_search: 'always',
					},
				],
			}),
		);

		expect(state.webSearch).toEqual(['off']);
		expect(state.webSearchSupported).toEqual([false]);
		expect(invoke).toHaveBeenCalledWith('set_web_search', { requestKind: 'chat', mode: 'off' });
	});

	it('normalizes Gemini always search mode to auto', () => {
		invoke.mockReset().mockResolvedValue(undefined);
		const { sync, state } = createSync(vi.fn());

		sync.applyDefaultModelFromSettings(
			settingsFor({
				primary: 'chat-profile',
				providers: [{ name: 'gemini', api_style: 'gemini' }],
				models: [
					{
						id: 'chat-profile',
						provider_name: 'gemini',
						model: 'gemini-2.5-pro',
						capabilities: ['chat'],
						web_search: 'always',
					},
				],
			}),
		);

		expect(state.webSearch).toEqual(['auto']);
		expect(state.webSearchSupported).toEqual([true]);
		expect(invoke).toHaveBeenCalledWith('set_web_search', { requestKind: 'chat', mode: 'auto' });
	});
});
