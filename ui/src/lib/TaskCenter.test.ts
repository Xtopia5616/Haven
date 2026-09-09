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
	it('keeps the empty state focused on tasks', () => {
		render(TaskCenter, { ...commonProps });

		expect(screen.queryByRole('heading', { name: '任务' })).toBeNull();
		expect(screen.getByText('暂无任务')).toBeTruthy();
		expect(screen.queryByText('当前会话')).toBeNull();
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

		expect(screen.getByRole('heading', { name: '进行中与待执行' })).toBeTruthy();
		expect(screen.getAllByText('整理下载目录').length).toBeGreaterThan(0);
		expect(screen.getAllByText('研究会话').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '停止后台任务' }));
		expect(onCancel).toHaveBeenCalledWith('act-1', 'background');
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

		expect(screen.getByRole('heading', { name: '进行中与待执行' })).toBeTruthy();
		expect(screen.getByRole('heading', { name: '执行记录' })).toBeTruthy();
		expect(screen.getAllByText('待执行').length).toBeGreaterThan(0);
		expect(screen.getAllByText('已执行').length).toBeGreaterThan(0);
	});

	it('uses shared count chips for the filtered total and lifecycle groups', () => {
		render(TaskCenter, {
			...commonProps,
			runningBackgroundActions: [
				{ id: 'act-running', kind: 'background', status: 'running' },
			],
			completedActions: [{ id: 'act-completed', kind: 'background', status: 'completed' }],
		});

		expect(screen.getByText('共 2 项任务')).toBeTruthy();
		expect(screen.getAllByText('共 1 项任务')).toHaveLength(2);
	});
});
