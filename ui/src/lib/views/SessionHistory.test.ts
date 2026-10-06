import { fireEvent, render, screen } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SessionHistory from './SessionHistory.svelte';
import type { SessionHistoryRow } from '$lib/contracts/sessionHistory.ts';

const commonProps = {
	statusOptions: [{ value: '', label: '全部状态' }],
	displayTitle: (_session: SessionHistoryRow) => '研究会话',
	statusVariant: (_status: string): 'success' => 'success',
	formatMessageTime: (_value: string) => '刚刚',
};

function setViewport(width: number, height: number) {
	Object.defineProperty(window, 'innerWidth', { configurable: true, value: width });
	Object.defineProperty(window, 'innerHeight', { configurable: true, value: height });
}

describe('SessionHistory actions', () => {
	beforeEach(() => setViewport(1024, 768));

	it('shows the history count in the shared filter bar', () => {
		render(SessionHistory, { ...commonProps, totalCount: 3 });

		const count = document.querySelector('.filter-bar > .count-chip');
		expect(count?.textContent).toBe('共 3 条历史');
	});

	it('offers a direct next step when there is no history', async () => {
		const onNewSession = vi.fn();
		render(SessionHistory, { ...commonProps, onNewSession });

		expect(screen.getByRole('heading', { name: '暂无会话' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '开始新会话' }));
		expect(onNewSession).toHaveBeenCalledTimes(1);
	});

	it('opens a session by clicking its row and keeps delete available', async () => {
		setViewport(839, 769);
		const onResume = vi.fn();
		const onDeleteRequest = vi.fn();
		const session: SessionHistoryRow = {
			id: 'ses-1',
			status: 'completed',
			created_at: '2026-09-06T03:00:00Z',
			updated_at: '2026-09-06T03:00:00Z',
			title: null,
			input_text: '整理研究资料',
		};
		render(SessionHistory, {
			...commonProps,
			sessions: [session],
			onResume,
			onDeleteRequest,
		});

		expect(screen.getByText('已完成')).toBeTruthy();
		expect(screen.getByText('打开')).toBeTruthy();
		expect(document.querySelector('.session-item.workspace-item-card')).toBeTruthy();
		expect(document.querySelector('.session-item .workspace-item-card-actions')).toBeTruthy();
		await fireEvent.click(document.querySelector('.session-item')!);
		expect(onResume).toHaveBeenCalledWith(session);
		await fireEvent.click(screen.getByRole('button', { name: '删除' }));
		expect(onDeleteRequest).toHaveBeenCalledWith(session);
		expect(onResume).toHaveBeenCalledTimes(1);
	});

	it('opens a session directly at the expanded-width breakpoint', async () => {
		setViewport(840, 900);
		const onResume = vi.fn();
		const session: SessionHistoryRow = {
			id: 'ses-1',
			status: 'completed',
			created_at: '2026-09-06T03:00:00Z',
			updated_at: '2026-09-06T03:00:00Z',
			title: null,
			input_text: '整理研究资料',
		};
		render(SessionHistory, { ...commonProps, sessions: [session], onResume });

		expect(screen.queryByRole('complementary', { name: '会话预览' })).toBeNull();
		expect(screen.getByRole('button', { name: '打开并继续会话：研究会话' })).toBeTruthy();
		await fireEvent.click(document.querySelector('.session-item')!);
		expect(onResume).toHaveBeenCalledTimes(1);
		expect(onResume).toHaveBeenCalledWith(session);
	});

	it('does not show the unused bulk export controls', () => {
		render(SessionHistory, {
			...commonProps,
			sessions: [
				{
					id: 'ses-1',
					status: 'completed',
					created_at: '2026-09-06T03:00:00Z',
					updated_at: '2026-09-06T03:00:00Z',
					title: null,
					input_text: '',
				},
			],
		});

		expect(screen.queryByRole('button', { name: '导出' })).toBeNull();
		expect(screen.queryByRole('button', { name: /导出选中/ })).toBeNull();
		expect(document.querySelector('.history-actions')).toBeNull();
	});

	it('keeps the native context menu available while renaming', () => {
		const onContextMenu = vi.fn();
		const session: SessionHistoryRow = {
			id: 'ses-1',
			status: 'completed',
			created_at: '2026-09-06T03:00:00Z',
			updated_at: '2026-09-06T03:00:00Z',
			title: null,
			input_text: '',
		};
		render(SessionHistory, {
			...commonProps,
			sessions: [session],
			editingTitle: session.id,
			renameValue: '研究会话',
			onContextMenu,
		});

		const input = screen.getByRole('textbox', { name: '' });
		const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
		input.dispatchEvent(event);

		expect(event.defaultPrevented).toBe(false);
		expect(onContextMenu).not.toHaveBeenCalled();
	});
});
