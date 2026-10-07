import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import ToolRunCenter from './ToolRunCenter.svelte';

const commonProps = {
	toolRunStatusLabel: (status: string) => (status === 'completed' ? '已完成' : status),
	sessionTitleFor: ({ sessionId }: { sessionId?: string }) =>
		sessionId === 'ses-1' ? '研究会话' : '',
	toolRunDuration: () => '3s',
	scheduledToolRunCountdown: () => '2分后',
};

describe('ToolRunCenter', () => {
	it('keeps the empty state focused on tasks', () => {
		render(ToolRunCenter, { ...commonProps });

		expect(screen.queryByRole('heading', { name: '任务' })).toBeNull();
		expect(screen.getByText('暂无任务')).toBeTruthy();
		expect(screen.queryByText('当前会话')).toBeNull();
	});

	it('opens the source session from the task card', async () => {
		const onOpenSession = vi.fn();
		const onCancel = vi.fn();
		render(ToolRunCenter, {
			...commonProps,
			runningBackgroundToolRuns: [
				{
					toolRunId: 'toolrun-1',
					kind: 'background',
					status: 'running',
					sessionId: 'ses-1',
					startedAt: '2026-09-03T10:00:00Z',
					command: '整理下载目录',
				},
			],
			onOpenSession,
			onCancel,
		});

		expect(screen.getByRole('heading', { name: '进行中' })).toBeTruthy();
		expect(screen.getAllByText('整理下载目录').length).toBeGreaterThan(0);
		expect(screen.getAllByText('研究会话').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '打开调用工具对应会话' }));
		expect(onOpenSession).toHaveBeenCalledWith('ses-1');
		await fireEvent.click(screen.getByRole('button', { name: '停止后台任务' }));
		expect(onCancel).toHaveBeenCalledWith('toolrun-1', 'background');
	});

	it('shows only pending scheduled tasks', () => {
		render(ToolRunCenter, {
			...commonProps,
			pendingScheduledToolRuns: [{ toolRunId: 'toolrun-pending', kind: 'scheduled', body: '稍后继续' }],
		});

		expect(screen.getByRole('heading', { name: '进行中' })).toBeTruthy();
		expect(screen.queryByRole('heading', { name: '执行记录' })).toBeNull();
		expect(screen.getAllByText('待执行').length).toBeGreaterThan(0);
	});

	it('shows triggered scheduled tasks as running and keeps them cancellable', async () => {
		const onCancel = vi.fn();
		render(ToolRunCenter, {
			...commonProps,
			pendingScheduledToolRuns: [
				{
					toolRunId: 'toolrun-running-scheduled',
					kind: 'scheduled',
					status: 'running',
					body: '已触发',
					startedAt: '2026-09-03T10:00:00Z',
				},
			],
			onCancel,
		});

		expect(screen.getAllByText('执行中').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '取消此定时任务' }));
		expect(onCancel).toHaveBeenCalledWith('toolrun-running-scheduled', 'scheduled');
	});

	it('uses one card structure for both kinds and preserves search and cancel behavior', async () => {
		const onCancel = vi.fn();
		const { container } = render(ToolRunCenter, {
			...commonProps,
			runningBackgroundToolRuns: [
				{
					toolRunId: 'toolrun-background',
					kind: 'background',
					status: 'running',
					sessionId: 'ses-1',
					command: '整理下载目录',
				},
			],
			pendingScheduledToolRuns: [
				{
					toolRunId: 'toolrun-scheduled',
					kind: 'scheduled',
					status: 'waiting',
					title: '稍后整理',
					body: '整理下载目录',
					mode: 'continue',
					dueAt: '2026-09-27T12:00:00Z',
				},
			],
			onCancel,
		});

		let cards = Array.from(container.querySelectorAll('.task-card'));
		expect(cards).toHaveLength(2);
		expect(cards[0].textContent).toContain('后台任务');
		expect(cards[0].textContent).toContain('running');
		expect(cards[0].textContent).toContain('整理下载目录');
		expect(cards[0].textContent).toContain('研究会话');
		expect(cards[0].textContent).toContain('3s');
		expect(cards[1].textContent).toContain('定时任务');
		expect(cards[1].textContent).toContain('待执行');
		expect(cards[1].textContent).toContain('稍后整理');
		expect(cards[1].textContent).toContain('继续会话');
		expect(cards[1].textContent).toContain('2分后');

		await fireEvent.input(screen.getByPlaceholderText('搜索任务标题、来源或命令'), {
			target: { value: '继续会话' },
		});
		cards = Array.from(container.querySelectorAll('.task-card'));
		expect(cards).toHaveLength(1);
		expect(cards[0].textContent).toContain('toolrun-scheduled');
		await fireEvent.click(screen.getByRole('button', { name: '取消此定时任务' }));
		expect(onCancel).toHaveBeenCalledWith('toolrun-scheduled', 'scheduled');
	});

	it('uses shared count chips for the filtered total and lifecycle groups', () => {
		render(ToolRunCenter, {
			...commonProps,
			runningBackgroundToolRuns: [
				{ toolRunId: 'toolrun-running', kind: 'background', status: 'running' },
			],
		});

		expect(screen.getByText('共 1 项任务')).toBeTruthy();
		expect(screen.getByText('共 1 条记录')).toBeTruthy();
	});
});
