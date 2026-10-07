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
			providerName: provider.name,
			proxyUrl: null,
			noProxy: null,
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
			proxy_url: null,
			no_proxy: null,
		});
		expect(screen.queryByText('secret-value')).toBeNull();
	});

	it('saves a provider with environment proxies bypassed for direct routing', async () => {
		const config = { providers: [], models: [], request_policies: [] };
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'direct' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '代理路由' }));
		await fireEvent.click(screen.getByRole('option', { name: '直连（忽略环境代理）' }));
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		expect(config.providers[0]).toMatchObject({ proxy_url: '', no_proxy: null });
	});

	it('applies custom proxy and bypass hosts to model discovery', async () => {
		invoke.mockResolvedValue([{ id: 'gpt-added', name: 'GPT Added' }]);
		const config = { providers: [], models: [], request_policies: [] };
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: '添加 Provider' }));
		await fireEvent.input(screen.getByPlaceholderText('唯一名称，角色据此选择'), {
			target: { value: 'proxied' },
		});
		await fireEvent.input(screen.getByPlaceholderText('sk-...'), {
			target: { value: 'test-key' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '代理路由' }));
		await fireEvent.click(screen.getByRole('option', { name: '指定代理' }));
		await fireEvent.input(screen.getByPlaceholderText('http://127.0.0.1:7890'), {
			target: { value: 'http://127.0.0.1:7890' },
		});
		await fireEvent.input(screen.getByPlaceholderText('localhost, 127.0.0.1, .example.com'), {
			target: { value: 'myai.bupt.edu.cn, localhost' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '保存' }));

		expect(config.providers[0]).toMatchObject({
			proxy_url: 'http://127.0.0.1:7890',
			no_proxy: 'myai.bupt.edu.cn, localhost',
		});
		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('discover_models', {
				baseUrl: 'https://api.openai.com/v1',
				apiKey: 'test-key',
				providerName: 'proxied',
				authHeaderName: 'Authorization',
				authHeaderPrefix: 'Bearer',
				proxyUrl: 'http://127.0.0.1:7890',
				noProxy: 'myai.bupt.edu.cn, localhost',
			}),
		);
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
				providerName: 'primary',
				authHeaderName: 'Authorization',
				authHeaderPrefix: 'Bearer',
				proxyUrl: null,
				noProxy: null,
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
			models: [{ id: 'default', providerName: 'local', model: 'llama3', capabilities: ['chat'] }],
			request_policies: [],
		};
		renderSettings({ ...props(config.providers, config.models), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: '删除 Provider local' }));

		expect(config.providers).toHaveLength(1);
		expect(config.models[0]).toMatchObject({ providerName: 'local', model: 'llama3' });
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
					providerName: 'removed-provider',
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

		expect(config.models[0]).toMatchObject({ providerName: 'local', model: '' });
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
					providerName: 'primary',
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
			providerName: 'backup',
			model: '',
			context_window: null,
			cost_per_1k_input_tokens: null,
		});
	});
});
