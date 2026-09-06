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
	it('offers a direct next step when there is no history', async () => {
		const onNewSession = vi.fn();
		render(SessionHistory, { ...commonProps, onNewSession });

		expect(screen.getByRole('heading', { name: '暂无会话' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '开始新会话' }));
		expect(onNewSession).toHaveBeenCalledTimes(1);
	});

	it('keeps opening and deleting sessions visible on each row', async () => {
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
		await fireEvent.click(screen.getByRole('button', { name: '打开' }));
		expect(onResume).toHaveBeenCalledWith(session);
		await fireEvent.click(screen.getByRole('button', { name: '删除' }));
		expect(onDeleteRequest).toHaveBeenCalledWith(session);
	});
});
