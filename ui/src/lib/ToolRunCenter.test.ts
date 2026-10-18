import { render, screen, fireEvent } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import ToolRunCenter from './ToolRunCenter.svelte';
import type { ToolRunStatus } from './contracts/toolRun.ts';

const commonProps = {
	toolRunStatusLabel: (status: ToolRunStatus | undefined) =>
		status === 'completed' ? '已完成' : status || '',
	sessionTitleFor: ({ sessionId }: { sessionId?: string }) =>
		sessionId === 'ses-00000000000000000000000000000001' ? '研究会话' : '',
	toolRunDuration: () => '3s',
};

describe('ToolRunCenter', () => {
	it('keeps the empty state focused on tasks', () => {
		render(ToolRunCenter, { ...commonProps });

		expect(screen.queryByRole('heading', { name: '任务' })).toBeNull();
		expect(screen.getByText('暂无后台任务')).toBeTruthy();
		expect(screen.queryByText('当前会话')).toBeNull();
	});

	it('opens the source session from the task card', async () => {
		const onOpenSession = vi.fn();
		const onCancel = vi.fn();
		render(ToolRunCenter, {
			...commonProps,
			runningBackgroundToolRuns: [
				{
					toolRunId: 'toolrun-00000000000000000000000000000001',
					kind: 'background',
					toolName: 'files.write',
					status: 'running',
					sessionId: 'ses-00000000000000000000000000000001',
					startedAt: '2026-09-03T10:00:00Z',
				},
			],
			onOpenSession,
			onCancel,
		});

		expect(screen.getByRole('heading', { name: '进行中' })).toBeTruthy();
		expect(screen.getAllByText('后台调用 · files.write').length).toBeGreaterThan(0);
		expect(screen.getAllByText('研究会话').length).toBeGreaterThan(0);
		await fireEvent.click(screen.getByRole('button', { name: '打开后台调用 · files.write对应会话' }));
		expect(onOpenSession).toHaveBeenCalledWith('ses-00000000000000000000000000000001');
		await fireEvent.click(screen.getByRole('button', { name: '停止后台任务' }));
		expect(onCancel).toHaveBeenCalledWith('toolrun-00000000000000000000000000000001');
	});

	it('preserves search and cancel behavior for background tool calls', async () => {
		const onCancel = vi.fn();
		const { container } = render(ToolRunCenter, {
			...commonProps,
			runningBackgroundToolRuns: [
				{
					toolRunId: 'toolrun-058de138a89ad8e4eef21b1dab8e5527',
					kind: 'background',
					toolName: 'files.write',
					status: 'running',
					sessionId: 'ses-00000000000000000000000000000001',
				},
			],
			onCancel,
		});

		let cards = Array.from(container.querySelectorAll('.task-card'));
		expect(cards).toHaveLength(1);
		expect(cards[0].querySelector('.status-badge')?.getAttribute('data-tone')).toBe('info');
		expect(cards[0].textContent).toContain('后台任务');
		expect(cards[0].textContent).toContain('running');
		expect(cards[0].textContent).toContain('后台调用 · files.write');
		expect(cards[0].textContent).toContain('研究会话');
		expect(cards[0].textContent).toContain('3s');

		await fireEvent.input(screen.getByPlaceholderText('搜索工具、来源会话或结果'), {
			target: { value: '研究会话' },
		});
		cards = Array.from(container.querySelectorAll('.task-card'));
		expect(cards).toHaveLength(1);
		expect(cards[0].textContent).toContain('toolrun-058de138a89ad8e4eef21b1dab8e5527');
	});

	it('uses shared count chips for the filtered total and lifecycle groups', () => {
		render(ToolRunCenter, {
			...commonProps,
			runningBackgroundToolRuns: [
				{ toolRunId: 'toolrun-01dd24e903cb06789d16dc94a1a6256a', kind: 'background', toolName: 'files.write', status: 'running' },
			],
		});

		expect(screen.getByText('共 1 项任务')).toBeTruthy();
		expect(screen.getByText('共 1 条记录')).toBeTruthy();
	});
});
