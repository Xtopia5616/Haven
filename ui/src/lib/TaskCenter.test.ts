import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import TaskCenter from './TaskCenter.svelte';

const commonProps = {
	actionStatusLabel: (status: string) => (status === 'completed' ? '已完成' : status),
	sessionTitleFor: ({ sessionId }: { sessionId?: string }) =>
		sessionId === 'ses-1' ? '研究会话' : '',
	actionDuration: () => '3s',
	scheduledActionCountdown: () => '2分后',
	formatHistoryTime: () => '刚刚',
};

describe('TaskCenter', () => {
	it('shows a useful empty state and starts a new session', async () => {
		const onNewSession = vi.fn();
		render(TaskCenter, { ...commonProps, onNewSession });

		expect(screen.getByRole('heading', { name: '任务' })).toBeTruthy();
		expect(screen.getByText('暂无任务')).toBeTruthy();
		expect(screen.getByText('任务负责执行与进度')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '开始新会话' }));
		expect(onNewSession).toHaveBeenCalledTimes(1);
	});

	it('opens task detail and routes source-session actions', async () => {
		const onOpenSession = vi.fn();
		const onCancel = vi.fn();
		render(TaskCenter, {
			...commonProps,
			runningBackgroundActions: [
				{
					id: 'act-1',
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

		expect(screen.getByRole('heading', { name: '后台与定时任务' })).toBeTruthy();
		expect(screen.getAllByText('整理下载目录').length).toBeGreaterThan(0);
		expect(screen.getAllByText('研究会话').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '查看调用工具详情' }));
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(screen.getByRole('heading', { name: '调用工具' })).toBeTruthy();
		expect(screen.getByText('执行命令')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '打开来源会话' }));
		expect(onOpenSession).toHaveBeenCalledWith('ses-1');
		await fireEvent.click(screen.getByRole('button', { name: '查看调用工具详情' }));
		await fireEvent.click(screen.getByRole('button', { name: '停止任务' }));
		expect(onCancel).toHaveBeenCalledWith('act-1', 'background');
	});

	it('distinguishes pending scheduled tasks from fired history', () => {
		render(TaskCenter, {
			...commonProps,
			pendingScheduledActions: [{ id: 'act-pending', kind: 'scheduled', body: '稍后继续' }],
			completedActions: [{ id: 'act-fired', kind: 'scheduled', body: '已经触发' }],
		});

		expect(screen.getByRole('heading', { name: '后台与定时任务' })).toBeTruthy();
		expect(screen.getByRole('heading', { name: '执行记录' })).toBeTruthy();
		expect(screen.getAllByText('待执行').length).toBeGreaterThan(0);
		expect(screen.getAllByText('已执行').length).toBeGreaterThan(0);
	});
});
