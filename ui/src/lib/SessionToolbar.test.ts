import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionToolbar from './SessionToolbar.svelte';

describe('SessionToolbar', () => {
	it('shows the shared toolbar switcher when parallel sessions exist', async () => {
		const onToggleSessionMenu = vi.fn();
		render(SessionToolbar, {
			showSessionMenu: true,
			menuSessions: [
				{ id: 'ses-1', status: 'running', title: '当前会话' },
				{ id: 'ses-2', status: 'paused', title: '另一个会话' },
			],
			onToggleSessionMenu,
		});

		const button = screen.getByRole('button', { name: '切换会话' });
		expect(button.classList.contains('md-btn')).toBe(true);
		expect(button.classList.contains('md-icon-btn')).toBe(false);
		expect(button.getAttribute('aria-haspopup')).toBe('menu');
		expect(button.querySelector('.session-switch-badge')?.textContent).toBe('2');

		await fireEvent.click(button);
		expect(onToggleSessionMenu).toHaveBeenCalledTimes(1);
		expect(screen.queryByText('新建会话')).toBeNull();
	});

	it('does not render duplicate new or end controls in the normal toolbar', () => {
		render(SessionToolbar, { activeSessionId: 'ses-1' });

		expect(screen.queryByRole('button', { name: '新建会话' })).toBeNull();
		expect(screen.queryByRole('button', { name: '结束会话' })).toBeNull();
	});

	it('opens detailed token usage on demand', async () => {
		render(SessionToolbar, {
			tokenStats: {
				cumulativePromptTokens: 800,
				cumulativeCompletionTokens: 200,
				cumulativeTotalTokens: 1000,
			},
			tokenUsageDetails: {
				currentPromptTokens: 200,
				currentCompletionTokens: 50,
				currentTotalTokens: 250,
				currentCachedTokens: 100,
				currentCacheCreationTokens: 0,
				currentCacheMissTokens: 100,
				currentCacheRatePercent: 50,
				contextTokens: 200,
				contextWindow: 1000,
				contextRatePercent: 20,
				cumulativePromptTokens: 800,
				cumulativeCompletionTokens: 200,
				cumulativeTotalTokens: 1000,
				cumulativeCachedTokens: 100,
				cumulativeCacheCreationTokens: 0,
				cumulativeCacheMissTokens: 700,
				cumulativeCacheRatePercent: 50,
				callCount: 4,
				mediaCallCount: 0,
				mediaTotalTokens: 0,
				mediaCostUsd: null,
				toolCallCount: 0,
				toolTotalTokens: 0,
				toolCostUsd: null,
				model: 'model-a',
				costUsd: 0.12,
			},
		});

		const tokenButton = screen.getByRole('button', { name: '打开 token 使用明细' });
		expect(tokenButton.classList.contains('selected')).toBe(false);
		expect(tokenButton.hasAttribute('data-context-tone')).toBe(false);
		expect(tokenButton.querySelector('.token-context')?.textContent).toBe('200');
		expect(tokenButton.querySelector('.token-unit')?.textContent).toBe('ctx');
		const cacheBar = tokenButton.querySelector('.token-budget');
		expect(cacheBar?.getAttribute('data-cache-tone')).toBe('medium');
		expect(cacheBar?.getAttribute('aria-label')).toBe('缓存命中 50%');
		expect(cacheBar?.querySelector('.token-budget-fill')?.getAttribute('style')).toContain(
			'width: 50%',
		);
		expect(screen.queryByText('缓存 50%')).toBeNull();

		await fireEvent.click(tokenButton);
		expect(tokenButton.classList.contains('selected')).toBe(true);
		expect(screen.getByRole('dialog', { name: 'Token 使用明细' })).toBeTruthy();
		expect(screen.getByText('命中率')).toBeTruthy();
		expect(screen.getAllByText('50%')).toHaveLength(1);
		expect(screen.queryByText('20%')).toBeNull();
		expect(screen.getByText('当前请求')).toBeTruthy();

		await fireEvent.click(document.body);
		expect(tokenButton.classList.contains('selected')).toBe(false);
	});

	it('shows cache diagnostics and unknown counts in usage details', async () => {
		render(SessionToolbar, {
			tokenStats: { cumulativeTotalTokens: 100 },
			tokenUsageDetails: {
				currentPromptTokens: 100,
				currentCompletionTokens: 0,
				currentTotalTokens: 100,
				currentCachedTokens: 0,
				currentCacheCreationTokens: 0,
				currentCacheMissTokens: 0,
				currentCacheRatePercent: null,
				currentCacheKnown: false,
				currentCacheDiagnostics: {
					mode: 'key',
					provider: 'openai-compatible',
					outcome: 'unknown',
					downgraded: true,
					usageSource: 'unavailable',
				},
				contextTokens: 100,
				contextWindow: 1000,
				contextRatePercent: 10,
				cumulativePromptTokens: 100,
				cumulativeCompletionTokens: 0,
				cumulativeTotalTokens: 100,
				cumulativeCachedTokens: 0,
				cumulativeCacheCreationTokens: 0,
				cumulativeCacheMissTokens: 0,
				cumulativeCacheRatePercent: null,
				cumulativeCacheKnown: false,
				callCount: 1,
				mediaCallCount: 0,
				mediaTotalTokens: 0,
				mediaCostUsd: null,
				toolCallCount: 0,
				toolTotalTokens: 0,
				toolCostUsd: null,
				model: 'gateway-model',
				costUsd: null,
			},
		});

		await fireEvent.click(screen.getByRole('button', { name: '打开 token 使用明细' }));
		expect(screen.getByText('策略 / 结果')).toBeTruthy();
		expect(screen.getByText('缓存 key / 未知')).toBeTruthy();
		expect(screen.getByText('openai-compatible')).toBeTruthy();
		expect(screen.getByText('未提供')).toBeTruthy();
		expect(screen.getByText('是')).toBeTruthy();
		expect(screen.getByText('本次缓存 token 用量未知')).toBeTruthy();
		expect(screen.getByText('累计缓存统计未知')).toBeTruthy();
	});

	it('shows media inference separately from Agent totals', async () => {
		render(SessionToolbar, {
			tokenStats: { cumulativeTotalTokens: 100 },
			tokenUsageDetails: {
				currentPromptTokens: 100,
				currentCompletionTokens: 20,
				currentTotalTokens: 120,
				currentCachedTokens: 50,
				currentCacheCreationTokens: 0,
				currentCacheMissTokens: 50,
				currentCacheRatePercent: 50,
				contextTokens: 100,
				contextWindow: 1000,
				contextRatePercent: 10,
				cumulativePromptTokens: 100,
				cumulativeCompletionTokens: 20,
				cumulativeTotalTokens: 120,
				cumulativeCachedTokens: 50,
				cumulativeCacheCreationTokens: 0,
				cumulativeCacheMissTokens: 50,
				cumulativeCacheRatePercent: 50,
				callCount: 1,
				mediaCallCount: 1,
				mediaTotalTokens: 900,
				mediaCostUsd: 0.02,
				toolCallCount: 0,
				toolTotalTokens: 0,
				toolCostUsd: null,
				model: 'agent-model',
				costUsd: 0.01,
			},
		});

		await fireEvent.click(screen.getByRole('button', { name: '打开 token 使用明细' }));
		expect(screen.getByText('媒体推理（不计入 Agent 累计）')).toBeTruthy();
		expect(screen.getByText('900 tokens')).toBeTruthy();
		expect(screen.getByText('0.0200 USD')).toBeTruthy();
	});

	it('allows selecting a completed historical session', async () => {
		const onSwitchSession = vi.fn();
		render(SessionToolbar, {
			showSessionMenu: true,
			sessionMenuOpen: true,
			menuSessions: [{ id: 'ses-old', status: 'completed', title: '旧绘画会话' }],
			onSwitchSession,
		});

		await fireEvent.click(screen.getByRole('menuitemradio'));

		expect(onSwitchSession).toHaveBeenCalledWith('ses-old');
	});
});
