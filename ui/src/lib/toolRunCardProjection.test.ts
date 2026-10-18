import { describe, expect, it } from 'vitest';
import { projectToolRunCard } from './toolRunCardProjection.ts';
import type { ToolRunStatus } from './contracts/toolRun.ts';

const projectionOptions = {
	toolRunStatusLabel: (status: ToolRunStatus | undefined) => {
		if (status === 'running') return '运行中';
		if (status === 'completed') return '已完成';
		return status || '';
	},
	sessionTitleFor: (toolRun: { sessionId?: string }) =>
		toolRun.sessionId === 'ses-00000000000000000000000000000001' ? '研究会话' : '',
	toolRunDuration: () => '3s',
};

describe('projectToolRunCard', () => {
	it('leaves a missing background status label blank', () => {
		const card = projectToolRunCard(
			{
				toolRunId: 'toolrun-058de138a89ad8e4eef21b1dab8e5527',
				kind: 'background',
				toolName: 'files.write',
			},
			projectionOptions,
		);

		expect(card.statusLabel).toBe('');
		expect(card.tone).toBe('neutral');
	});

	it('maps finite ToolRun states to the card tone vocabulary', () => {
		const toneFor = (status: ToolRunStatus) =>
			projectToolRunCard({
				toolRunId: 'toolrun-00000000000000000000000000000001',
				kind: 'background',
				toolName: 'files.write',
				status,
			}, projectionOptions).tone;

		expect(toneFor('running')).toBe('running');
		expect(toneFor('failed')).toBe('error');
		expect(toneFor('cancelled')).toBe('neutral');
	});

	it('projects background cards with the shared fields and background details', () => {
		const card = projectToolRunCard(
			{
				toolRunId: 'toolrun-058de138a89ad8e4eef21b1dab8e5527',
				kind: 'background',
				toolName: 'files.write',
				status: 'completed',
				sessionId: 'ses-00000000000000000000000000000001',
				output: '已移动 3 个文件',
				error: 'diagnostic detail',
				errorReason: 'summary',
				preview: '当前进度',
			},
			projectionOptions,
		);

		expect(card).toMatchObject({
			toolRunId: 'toolrun-058de138a89ad8e4eef21b1dab8e5527',
			kind: 'background',
			status: 'completed',
			sessionId: 'ses-00000000000000000000000000000001',
			title: '后台调用 · files.write',
			toolName: 'files.write',
			searchText: '研究会话 files.write',
			statusLabel: '已完成',
			tone: 'success',
			summary: '已移动 3 个文件',
			context: '研究会话',
			timing: '3s',
				details: {
				output: '已移动 3 个文件',
				error: 'diagnostic detail',
				preview: '当前进度',
			},
		});
	});

});
