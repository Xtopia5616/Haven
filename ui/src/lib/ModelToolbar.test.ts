import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import ModelToolbar from './ModelToolbar.svelte';

describe('ModelToolbar', () => {
	it('shows the active model in the compact switch control', () => {
		render(ModelToolbar, { currentModelName: 'GPT-5', modelMenuOpen: false });

		const button = screen.getByRole('button', { name: '切换模型：GPT-5' });
		expect(button.textContent).toContain('GPT-5');
		expect(button.getAttribute('aria-haspopup')).toBe('dialog');
		expect(button.getAttribute('aria-expanded')).toBe('false');
	});

	it('labels chat options by configured profile and provider model', () => {
		render(ModelToolbar, {
			currentModelId: 'chat-profile',
			modelMenuOpen: true,
			modelOptions: [
				{
					id: 'chat-profile',
					name: 'chat-profile',
					provider: 'primary',
					model: 'gpt-5',
					reasoningEffort: '',
					webSearch: 'off',
					apiStyle: 'openai-responses',
					webSearchSupported: true,
				},
			],
		});

		expect(screen.getByText('chat-profile')).toBeTruthy();
		expect(screen.getByText('primary / gpt-5')).toBeTruthy();
		expect(screen.getByRole('menuitemradio', { name: /chat-profile/ }).getAttribute('aria-checked'))
			.toBe('true');
	});

	it('uses shared collapsible and choice chips for thinking and web search options', async () => {
		const onEffortSelect = vi.fn();
		const onWebSearchSelect = vi.fn();
		render(ModelToolbar, {
			modelMenuOpen: true,
			effortOptions: [
				{ value: '', label: '默认' },
				{ value: 'medium', label: '中' },
			],
			currentEffort: 'medium',
			onEffortSelect,
			webSearchSupported: true,
			webSearchOptions: [
				{ value: 'off', label: '关闭' },
				{ value: 'auto', label: '自动' },
			],
			currentWebSearch: 'auto',
			onWebSearchSelect,
		});

		const toggle = screen.getByRole('button', { name: '思考与联网选项' });
		expect(toggle.getAttribute('aria-expanded')).toBe('false');
		expect(screen.queryByRole('group', { name: '思考强度' })).toBeNull();

		await fireEvent.click(toggle);

		const effortGroup = screen.getByRole('group', { name: '思考强度' });
		const webSearchGroup = screen.getByRole('group', { name: '联网搜索' });
		expect(toggle.getAttribute('aria-expanded')).toBe('true');
		expect(within(effortGroup).getByRole('button', { name: '中' }).getAttribute('aria-pressed'))
			.toBe('true');
		expect(within(webSearchGroup).getByRole('button', { name: '自动' }).getAttribute('aria-pressed'))
			.toBe('true');

		await fireEvent.click(within(effortGroup).getByRole('button', { name: '默认' }));
		await fireEvent.click(within(webSearchGroup).getByRole('button', { name: '关闭' }));
		expect(onEffortSelect).toHaveBeenCalledWith('');
		expect(onWebSearchSelect).toHaveBeenCalledWith('off');
	});
});
