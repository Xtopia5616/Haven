import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ModelSettings from './ModelSettings.svelte';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

function props(
	providers: Array<Record<string, unknown>> = [],
	roles: Array<Record<string, unknown>> = [],
) {
	return {
		section: 'models',
		llmConfig: { providers, roles },
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
		await waitFor(() => expect(screen.getByText('1 个模型')).toBeTruthy());
		expect(invoke).toHaveBeenCalledWith('discover_models', {
			baseUrl: provider.base_url,
			apiKey: provider.api_key,
			provider: provider.name,
		});
	});

	it('saves a provider through the dialog without exposing its API key', async () => {
		const config = { providers: [], roles: [] };
		renderSettings({ ...props(config.providers, config.roles), llmConfig: config });

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

	it('clears role bindings when deleting their provider', async () => {
		const provider = {
			name: 'local',
			provider: 'ollama',
			api_style: 'openai-chat',
			base_url: 'http://127.0.0.1:11434/v1',
			api_key: '',
		};
		const config = {
			providers: [provider],
			roles: [{ role: 'default_model', provider: 'local', model: 'llama3' }],
		};
		renderSettings({ ...props(config.providers, config.roles), llmConfig: config });

		await fireEvent.click(screen.getByRole('button', { name: '删除' }));

		expect(config.providers).toHaveLength(0);
		expect(config.roles[0]).toMatchObject({ provider: '', model: '' });
	});
});
