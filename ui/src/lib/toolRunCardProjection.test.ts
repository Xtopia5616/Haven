import { describe, expect, it } from 'vitest';
import { projectToolRunCard } from './toolRunCardProjection.ts';

const projectionOptions = {
	toolRunStatusLabel: (status: string) => {
		if (status === 'running') return '运行中';
		if (status === 'completed') return '已完成';
		return status;
	},
	sessionTitleFor: (toolRun: { sessionId?: string }) =>
		toolRun.sessionId === 'ses-1' ? '研究会话' : '',
	toolRunDuration: () => '3s',
	scheduledToolRunCountdown: (dueAt?: string) => (dueAt ? '2分后' : ''),
};

describe('projectToolRunCard', () => {
	it('projects background cards with the shared fields and background details', () => {
		const card = projectToolRunCard(
			{
				toolRunId: 'toolrun-background',
				kind: 'background',
				status: 'completed',
				sessionId: 'ses-1',
				command: '整理下载目录',
				output: '已移动 3 个文件',
				error: 'diagnostic detail',
				errorReason: 'summary',
				exitCode: 0,
				preview: '当前进度',
			},
			projectionOptions,
		);

		expect(card).toMatchObject({
			toolRunId: 'toolrun-background',
			kind: 'background',
			status: 'completed',
			sessionId: 'ses-1',
			title: '调用工具',
			searchText: '研究会话',
			statusLabel: '已完成',
			tone: 'success',
			summary: '整理下载目录',
			context: '研究会话',
			timing: '3s',
			details: {
				command: '整理下载目录',
				output: '已移动 3 个文件',
				error: 'diagnostic detail',
				errorReason: 'summary',
				exitCode: 0,
				preview: '当前进度',
			},
		});
		expect(card.details.dueAt).toBeUndefined();
	});

	it('projects scheduled cards with the same structure and scheduled details', () => {
		const card = projectToolRunCard(
			{
				toolRunId: 'toolrun-scheduled',
				kind: 'scheduled',
				status: 'completed',
				sessionId: 'ses-1',
				dueAt: '2026-09-27T12:00:00Z',
				title: '稍后整理',
				body: '整理下载目录',
				mode: 'continue',
			},
			projectionOptions,
		);

		expect(card).toMatchObject({
			toolRunId: 'toolrun-scheduled',
			kind: 'scheduled',
			status: 'completed',
			sessionId: 'ses-1',
			title: '稍后整理',
			searchText: '继续会话',
			statusLabel: '已完成',
			tone: 'success',
			summary: '整理下载目录',
			context: '继续会话',
			timing: '2分后',
			details: {
				dueAt: '2026-09-27T12:00:00Z',
				title: '稍后整理',
				body: '整理下载目录',
				mode: 'continue',
			},
		});
		expect(card.details.command).toBeUndefined();
	});
});
