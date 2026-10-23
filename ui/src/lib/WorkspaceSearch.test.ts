import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import WorkspaceSearch from './WorkspaceSearch.svelte';

describe('WorkspaceSearch', () => {
	it('reports typing and clearing and returns focus to the search field', async () => {
		const onInput = vi.fn();
		render(WorkspaceSearch, { label: '搜索会话', value: '会议', onInput });
		const input = screen.getByRole('searchbox', { name: '搜索会话' });
		await fireEvent.input(input, { target: { value: '计划' } });
		expect(onInput).toHaveBeenLastCalledWith('计划');
		await fireEvent.click(screen.getByRole('button', { name: '清空搜索会话' }));
		expect(onInput).toHaveBeenLastCalledWith('');
		expect(document.activeElement).toBe(input);
	});

	it('preserves settings help and results associations while loading', () => {
		render(WorkspaceSearch, {
			label: '搜索设置',
			disabled: true,
			describedBy: 'help',
			controls: 'results',
		});
		const input = screen.getByRole('searchbox', { name: '搜索设置' });
		expect(input.hasAttribute('disabled')).toBe(true);
		expect(input.getAttribute('aria-describedby')).toBe('help');
		expect(input.getAttribute('aria-controls')).toBe('results');
		expect(screen.queryByRole('button')).toBeNull();
	});
});
