import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ModelSettings from './ModelSettings.svelte';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

function props(
	providers: Array<Record<string, unknown>> = [],
	models: Array<Record<string, unknown>> = [],
) {
	return {
		section: 'models',
		llmConfig: { providers, models, request_policies: [] },
		keyConfiguredProviders: {},
		loaded: false,
	};
}

function renderSettings(options: Record<string, unknown>) {
	return render(ModelSettings, options as never);
}

describe('ModelSettings provider surface', () => {
	beforeEach(() => {
		invoke.mockReset();
		invoke.mockResolvedValue([]);
	});

	it('renders configured providers and their discovered model count', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'discover_models') {
				return [{ id: 'gpt-test', name: 'GPT Test', context_window: 128000 }];
			}
			return [];
		});
		const provider = {
			name: 'openai-main',
			provider: 'openai',
			api_style: 'openai-chat',
			base_url: 'https://api.openai.com/v1',
			api_key: 'configured-key',
		};

		renderSettings({ ...props([provider]), loaded: true });

		expect(screen.getByText('openai-main')).toBeTruthy();
		await waitFor(() => expect(screen.getByText('服务目录 1 个')).toBeTruthy());
		expect(invoke).toHaveBeenCalledWith('discover_models', {
			baseUrl: provider.base_url,
			apiKey: provider.api_key,
			provider: provider.name,
		});
	});

	it('saves a provider through the dialog without exposing its API key', async () => {
		const config = { providers: [], models: [], request_policies: [] };
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'local' },
		});
		await fireEvent.input(screen.getByPlaceholderText('sk-...'), {
			target: { value: 'secret-value' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		expect(config.providers).toHaveLength(1);
		expect(config.providers[0]).toMatchObject({
			name: 'local',
			api_key: 'secret-value',
			api_style: 'openai-chat',
		});
		expect(screen.queryByText('secret-value')).toBeNull();
	});

	it('fetches models immediately with the selected auth scheme when adding a provider', async () => {
		invoke.mockResolvedValue([{ id: 'gpt-added', name: 'GPT Added' }]);
		const onProviderDiscoveryFailure = vi.fn();
		const config = { providers: [], models: [], request_policies: [] };
		renderSettings({
			...props(config.providers, config.models),
			llmConfig: config,
			onProviderDiscoveryFailure,
		});

		await fireEvent.click(screen.getByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'primary' },
		});
		await fireEvent.input(screen.getByPlaceholderText('sk-...'), {
			target: { value: 'new-key' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('discover_models', {
				baseUrl: 'https://api.openai.com/v1',
				apiKey: 'new-key',
				provider: 'primary',
				authHeaderName: 'Authorization',
				authHeaderPrefix: 'Bearer',
			}),
		);
		expect(onProviderDiscoveryFailure).not.toHaveBeenCalled();
	});

	it('keeps the provider and opens failure feedback when model discovery fails', async () => {
		invoke.mockRejectedValue(new Error('provider unavailable'));
		const onProviderDiscoveryFailure = vi.fn();
		const config = { providers: [], models: [], request_policies: [] };
		renderSettings({
			...props(config.providers, config.models),
			llmConfig: config,
			onProviderDiscoveryFailure,
		});

		await fireEvent.click(screen.getByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'primary' },
		});
		await fireEvent.input(screen.getByPlaceholderText('sk-...'), {
			target: { value: 'new-key' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		await waitFor(() =>
			expect(onProviderDiscoveryFailure).toHaveBeenCalledWith('primary', false),
		);
		expect(config.providers).toHaveLength(1);
	});

	it('keeps a provider while model configurations still reference it', async () => {
		const provider = {
			name: 'local',
			provider: 'ollama',
			api_style: 'openai-chat',
			base_url: 'http://127.0.0.1:11434/v1',
			api_key: '',
		};
		const config = {
			providers: [provider],
			models: [{ id: 'default', provider: 'local', model: 'llama3', capabilities: ['chat'] }],
			request_policies: [],
		};
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: '删除 Provider local' }));

		expect(config.providers).toHaveLength(1);
		expect(config.models[0]).toMatchObject({ provider: 'local', model: 'llama3' });
	});

	it('shows unbound models in a repair section and lets the user choose a provider', async () => {
		const config = {
			providers: [
				{
					name: 'local',
					provider: 'ollama',
					api_style: 'openai-chat',
					base_url: 'http://127.0.0.1:11434/v1',
					api_key: '',
				},
			],
			models: [
				{
					id: 'assistant',
					provider: 'removed-provider',
					model: 'old-model',
					capabilities: ['chat'],
				},
			],
			request_policies: [],
		};
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		expect(screen.getByText('需要修复的模型')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: /assistant Provider 不存在/ }));
		const providerSelect = screen.getByRole('button', {
			name: '为模型 assistant 选择 Provider',
		});
		await fireEvent.click(providerSelect);
		await fireEvent.click(screen.getByRole('option', { name: 'local' }));

		expect(config.models[0]).toMatchObject({ provider: 'local', model: '' });
	});

	it('moves a model to another provider and clears provider-specific metadata', async () => {
		const provider = (name: string) => ({
			name,
			provider: 'openai',
			api_style: 'openai-chat',
			base_url: 'https://example.test/v1',
			api_key: '',
		});
		const config = {
			providers: [provider('primary'), provider('backup')],
			models: [
				{
					id: 'assistant',
					provider: 'primary',
					model: 'model-a',
					capabilities: ['chat'],
					context_window: 128000,
					cost_per_1k_input_tokens: 0.1,
				},
			],
			request_policies: [],
		};
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: /assistant primary/ }));
		await fireEvent.click(
			screen.getByRole('button', { name: '为模型 assistant 选择 Provider' }),
		);
		await fireEvent.click(screen.getByRole('option', { name: 'backup' }));

		expect(config.models[0]).toMatchObject({
			provider: 'backup',
			model: '',
			context_window: null,
			cost_per_1k_input_tokens: null,
		});
	});
});
