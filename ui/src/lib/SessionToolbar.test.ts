import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionToolbar from './SessionToolbar.svelte';

describe('SessionToolbar', () => {
	it('shows the shared toolbar switcher when parallel sessions exist', async () => {
		const onToggleSessionMenu = vi.fn();
		render(SessionToolbar, { showSessionMenu: true, onToggleSessionMenu });

		const button = screen.getByRole('button', { name: '切换会话' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');

		await fireEvent.click(button);
		expect(onToggleSessionMenu).toHaveBeenCalledTimes(1);
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
				model: 'model-a',
				costUsd: 0.12,
			},
		});

		const tokenButton = screen.getByRole('button', { name: '打开 token 使用明细' });
		expect(tokenButton.classList.contains('selected')).toBe(false);
		expect(tokenButton.getAttribute('data-context-tone')).toBe('low');
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
		expect(screen.getByText('当前请求')).toBeTruthy();

		await fireEvent.click(document.body);
		expect(tokenButton.classList.contains('selected')).toBe(false);
	});
});
