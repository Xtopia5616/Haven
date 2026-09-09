import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SessionHistory from './SessionHistory.svelte';

const commonProps = {
	statusOptions: [{ value: '', label: '全部状态' }],
	displayTitle: () => '研究会话',
	statusVariant: () => 'success',
	formatMessageTime: () => '刚刚',
};

describe('SessionHistory actions', () => {
	it('shows the history count in the shared filter bar', () => {
		render(SessionHistory, { ...commonProps, totalCount: 3 });

		expect(document.querySelector('.filter-bar .count-chip')?.textContent).toBe('共 3 条历史');
	});

	it('offers a direct next step when there is no history', async () => {
		const onNewSession = vi.fn();
		render(SessionHistory, { ...commonProps, onNewSession });

		expect(screen.getByRole('heading', { name: '暂无会话' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '开始新会话' }));
		expect(onNewSession).toHaveBeenCalledTimes(1);
	});

	it('opens a session by clicking its row and keeps delete available', async () => {
		const onResume = vi.fn();
		const onDeleteRequest = vi.fn();
		const session = {
			id: 'ses-1',
			status: 'completed',
			created_at: '2026-09-06T03:00:00Z',
			transcript: '整理研究资料',
		};
		render(SessionHistory, {
			...commonProps,
			sessions: [session],
			onResume,
			onDeleteRequest,
		});

		expect(screen.getByText('已完成')).toBeTruthy();
		expect(screen.getByText('打开')).toBeTruthy();
		await fireEvent.click(document.querySelector('.session-item')!);
		expect(onResume).toHaveBeenCalledWith(session);
		await fireEvent.click(screen.getByRole('button', { name: '删除' }));
		expect(onDeleteRequest).toHaveBeenCalledWith(session);
		expect(onResume).toHaveBeenCalledTimes(1);
	});

	it('places bulk history actions below the filter bar', async () => {
		const onEnterSelectMode = vi.fn();
		const onOpenClearDialog = vi.fn();
		render(SessionHistory, {
			...commonProps,
			sessions: [{ id: 'ses-1', status: 'completed', created_at: '2026-09-06T03:00:00Z' }],
			onEnterSelectMode,
			onOpenClearDialog,
		});

		const actions = document.querySelector('.history-actions');
		expect(actions).toBeTruthy();
		expect(actions?.previousElementSibling?.classList.contains('filter-bar')).toBe(true);
		await fireEvent.click(screen.getByRole('button', { name: '导出' }));
		await fireEvent.click(screen.getByRole('button', { name: '清空会话' }));
		expect(onEnterSelectMode).toHaveBeenCalledTimes(1);
		expect(onOpenClearDialog).toHaveBeenCalledTimes(1);
	});
});
