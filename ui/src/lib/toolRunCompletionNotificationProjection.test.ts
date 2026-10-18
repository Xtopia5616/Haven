import { describe, expect, it } from 'vitest';
import {
	createToolRunCompletionNotificationGate,
	MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS,
	projectToolRunCompletionToast,
} from './toolRunCompletionNotificationProjection.ts';
describe('ToolRun completion toast projection', () => {
	it('waits for persisted notification settings and then preserves event order', () => {
		const delivered: string[] = [];
		const gate = createToolRunCompletionNotificationGate((payload) =>
			delivered.push(payload.toolRunId || ''),
		);
		const first = {
			sessionId: '',
			title: '后台任务已完成',
			body: 'first',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'background' as const,
			toolRunId: 'toolrun-00000000000000000000000000000001',
			toolRunStatus: 'completed' as const,
		};
		const second = { ...first, toolRunId: 'toolrun-00000000000000000000000000000002' };
		gate.notify(first);
		gate.notify(second);
		expect(delivered).toEqual([]);
		gate.settingsLoaded();
		expect(delivered).toEqual([
			'toolrun-00000000000000000000000000000001',
			'toolrun-00000000000000000000000000000002',
		]);
	});

	it('caps the pending FIFO and drops arrivals after it is full', () => {
		const delivered: string[] = [];
		const gate = createToolRunCompletionNotificationGate((payload) =>
			delivered.push(payload.toolRunId || ''),
		);
		const first = {
			sessionId: '',
			title: '后台任务已完成',
			body: 'result',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'background' as const,
			toolRunId: '',
			toolRunStatus: 'completed' as const,
		};

		for (let index = 0; index < MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS; index += 1) {
			gate.notify({
				...first,
				toolRunId: `toolrun-${index.toString(16).padStart(32, '0')}`,
			});
		}
		gate.notify({ ...first, toolRunId: 'toolrun-1f7a56c167a044bd47e3ae6c51b6ee86' });
		expect(delivered).toEqual([]);

		gate.settingsLoaded();
		expect(delivered).toHaveLength(MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS);
		expect(delivered[0]).toBe('toolrun-00000000000000000000000000000000');
		expect(delivered.at(-1)).toBe(
			`toolrun-${(MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS - 1).toString(16).padStart(32, '0')}`,
		);
		expect(delivered).not.toContain('toolrun-1f7a56c167a044bd47e3ae6c51b6ee86');
	});

	it('preserves background status and active-session rules', () => {
		const completed = {
			sessionId: 'ses-d186f90b4f33cc53ee287f9e8c401e6a',
			title: '后台任务已完成',
			body: 'toolrun-00000000000000000000000000000001 已完成',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'background' as const,
			toolRunId: 'toolrun-00000000000000000000000000000001',
			toolRunStatus: 'completed' as const,
		};
		expect(
			projectToolRunCompletionToast(completed, 'ses-d186f90b4f33cc53ee287f9e8c401e6a'),
		).toBeNull();
		expect(
			projectToolRunCompletionToast(completed, 'ses-fb38531de0466990f46b178209103744'),
		).toEqual({
			message: '后台任务完成: toolrun-00000000000000000000000000000001',
			type: 'success',
			durationMs: 4000,
		});
		expect(
			projectToolRunCompletionToast(
				{ ...completed, toolRunStatus: 'failed', sessionId: '' },
				null,
			),
		).toEqual({
			message: '后台任务失败: toolrun-00000000000000000000000000000001',
			type: 'error',
			durationMs: 4000,
		});
		expect(
			projectToolRunCompletionToast({ ...completed, toolRunStatus: undefined }, null),
		).toBeNull();
	});

});
