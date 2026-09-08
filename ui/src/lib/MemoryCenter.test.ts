import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import MemoryCenter from './views/MemoryCenter.svelte';

describe('MemoryCenter', () => {
	it('browses saved facts before a search is submitted', () => {
		render(MemoryCenter, {
			facts: [],
			factsLoaded: true,
			factSourceOptions: [{ value: '', label: '全部来源' }],
			memoryRecall: {
				query: '',
				kind: 'all',
				results: [],
				loading: false,
				searched: false,
			},
		});

		expect(screen.getByRole('heading', { name: '记忆' })).toBeTruthy();
		expect(screen.getByRole('heading', { name: '已保存的事实' })).toBeTruthy();
		expect(screen.getByRole('searchbox', { name: '搜索记忆' })).toBeTruthy();
	});

	it('forwards the selected memory scope and search action', async () => {
		const onRecallKindChange = vi.fn();
		const onRunRecall = vi.fn();
		const memoryRecall = {
			query: 'dark theme',
			kind: 'all',
			results: [],
			loading: false,
			searched: false,
		};
		render(MemoryCenter, {
			facts: [],
			factsLoaded: true,
			factSourceOptions: [{ value: '', label: '全部来源' }],
			memoryRecall,
			onRecallKindChange,
			onRunRecall,
		});

		await fireEvent.click(screen.getByRole('button', { name: '记忆范围' }));
		await fireEvent.click(screen.getByRole('option', { name: '过去的对话' }));
		await fireEvent.click(screen.getByRole('button', { name: '搜索' }));

		expect(onRecallKindChange).toHaveBeenCalledWith('episode');
		expect(onRunRecall).toHaveBeenCalledTimes(1);
	});
});
