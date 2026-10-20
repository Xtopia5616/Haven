import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionHeader from './SessionHeader.svelte';

describe('SessionHeader', () => {
	it('renders new, delete, and end actions as labeled text buttons', async () => {
		const onNew = vi.fn();
		const onDelete = vi.fn();
		const onEnd = vi.fn();
		render(SessionHeader as any, { onNew, onDelete, onEnd, hasSession: true });
		expect(document.querySelector('.session-header__status')).toBeNull();

		const newButton = screen.getByRole('button', { name: '新建会话' });
		const deleteButton = screen.getByRole('button', { name: '删除会话' });
		const endButton = screen.getByRole('button', { name: '完成会话' });
		for (const button of [newButton, deleteButton, endButton]) {
			expect(button.classList.contains('md-btn')).toBe(true);
			expect(button.getAttribute('data-width')).toBe('compact');
		}
		expect(deleteButton.classList.contains('md-btn--danger')).toBe(true);
		expect(endButton.classList.contains('md-btn--success')).toBe(true);

		await fireEvent.click(newButton);
		expect(onNew).toHaveBeenCalledTimes(1);
		await fireEvent.click(deleteButton);
		expect(onDelete).toHaveBeenCalledTimes(1);
		await fireEvent.click(endButton);
		expect(onEnd).toHaveBeenCalledTimes(1);
	});

	it('uses a success action for completing an active conversation', () => {
		render(SessionHeader as any, { hasSession: true });

		expect(
			screen.getByRole('button', { name: '完成会话' }).classList.contains('md-btn--success'),
		).toBe(true);
		expect(screen.queryByRole('button', { name: '删除会话' })).toBeTruthy();
	});

	it('hides the delete action when there is no active session', () => {
		render(SessionHeader as any);

		expect(screen.queryByRole('button', { name: '删除会话' })).toBeNull();
	});

	it('selects a saved workspace and removes the selected shortcut on request', async () => {
		const onSelectExecutionWorkspace = vi.fn();
		const onRemoveExecutionWorkspace = vi.fn();
		render(SessionHeader as any, {
			executionWorkspaceRoot: 'C:\\Projects\\Haven',
			savedExecutionWorkspaces: ['C:\\Projects\\Haven', 'D:\\Shared'],
			onSelectExecutionWorkspace,
			onRemoveExecutionWorkspace,
		});

		await fireEvent.click(screen.getByRole('combobox', { name: '执行工作区' }));
		await fireEvent.click(screen.getByRole('option', { name: 'D:\\Shared' }));
		expect(onSelectExecutionWorkspace).toHaveBeenCalledWith('D:\\Shared');

		await fireEvent.click(
			screen.getByRole('button', { name: '从工作区快捷列表移除 C:\\Projects\\Haven' }),
		);
		expect(onRemoveExecutionWorkspace).toHaveBeenCalledWith('C:\\Projects\\Haven');
		expect(
			screen
				.getByRole('button', {
					name: '从工作区快捷列表移除 C:\\Projects\\Haven',
				})
				.getAttribute('data-variant'),
		).toBe('danger-text');
	});

	it('opens the native directory picker through the add workspace action', async () => {
		const onAddExecutionWorkspace = vi.fn();
		render(SessionHeader as any, { onAddExecutionWorkspace });

		await fireEvent.click(screen.getByRole('button', { name: '选择目录并保存为工作区' }));
		expect(onAddExecutionWorkspace).toHaveBeenCalledOnce();
	});
});
