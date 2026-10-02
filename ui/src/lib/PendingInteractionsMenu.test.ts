import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import PendingInteractionsMenu from './PendingInteractionsMenu.svelte';

describe('PendingInteractionsMenu', () => {
	it('lists pending asks and confirmations and opens the selected request', async () => {
		const onSelect = vi.fn();
		render(PendingInteractionsMenu, {
			items: [
				{
					id: 'ask-1',
					kind: 'ask',
					title: '待回答 · 测试会话',
					detail: '选择部署方式',
				},
				{
					id: 'conf-1',
					kind: 'confirm',
					title: '权限确认 · 测试会话',
					detail: '写入文件',
				},
			],
			onSelect,
		});

		const toggle = screen.getByRole('button', { name: '待操作 2' });
		expect(toggle.getAttribute('aria-haspopup')).toBe('menu');
		await fireEvent.click(toggle);
		expect(
			screen.getByRole('menuitem', { name: '待回答 · 测试会话：选择部署方式' }),
		).toBeTruthy();
		await fireEvent.click(
			screen.getByRole('menuitem', { name: '权限确认 · 测试会话：写入文件' }),
		);
		expect(onSelect).toHaveBeenCalledWith('conf-1');
		expect(screen.queryByRole('menu')).toBeNull();
	});
});
