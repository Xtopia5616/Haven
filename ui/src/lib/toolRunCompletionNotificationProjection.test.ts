import { afterEach, describe, expect, it } from 'vitest';
import {
	createToolRunCompletionNotificationGate,
	MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS,
	projectToolRunCompletionToast,
} from './toolRunCompletionNotificationProjection.ts';
import {
	DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS,
	setToolRunCompletionNotificationChannels,
	shouldShowToolRunCompletionInApp,
} from './toolRunCompletionNotificationSettings.ts';

afterEach(() => {
	setToolRunCompletionNotificationChannels(DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS);
});

describe('ToolRun completion toast projection', () => {
	it('waits for persisted notification settings and then preserves event order', () => {
		const delivered: string[] = [];
		const gate = createToolRunCompletionNotificationGate((payload) =>
			delivered.push(payload.toolRunId || ''),
		);
		const first = {
			sessionId: '',
			title: 'scheduled',
			body: 'first',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'scheduled' as const,
			toolRunId: 'toolrun-1',
		};
		const second = { ...first, toolRunId: 'toolrun-2' };
		gate.notify(first);
		gate.notify(second);
		expect(delivered).toEqual([]);
		gate.settingsLoaded();
		expect(delivered).toEqual(['toolrun-1', 'toolrun-2']);
	});

	it('caps the pending FIFO and drops arrivals after it is full', () => {
		const delivered: string[] = [];
		const gate = createToolRunCompletionNotificationGate((payload) =>
			delivered.push(payload.toolRunId || ''),
		);
		const first = {
			sessionId: '',
			title: 'scheduled',
			body: 'result',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'scheduled' as const,
			toolRunId: '',
		};

		for (let index = 0; index < MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS; index += 1) {
			gate.notify({ ...first, toolRunId: `toolrun-${index}` });
		}
		gate.notify({ ...first, toolRunId: 'toolrun-overflow' });
		expect(delivered).toEqual([]);

		gate.settingsLoaded();
		expect(delivered).toHaveLength(MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS);
		expect(delivered[0]).toBe('toolrun-0');
		expect(delivered.at(-1)).toBe(`toolrun-${MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS - 1}`);
		expect(delivered).not.toContain('toolrun-overflow');
	});

	it('drops queued toasts when hydrated settings disable the in-app channel', () => {
		const delivered: string[] = [];
		const gate = createToolRunCompletionNotificationGate((payload) => {
			if (shouldShowToolRunCompletionInApp()) delivered.push(payload.toolRunId || '');
		});
		const pending = {
			sessionId: '',
			title: 'scheduled',
			body: 'result',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'scheduled' as const,
			toolRunId: 'toolrun-disabled',
		};

		gate.notify(pending);
		setToolRunCompletionNotificationChannels({ in_app: false, windows: true });
		gate.settingsLoaded();

		expect(delivered).toEqual([]);
	});

	it('preserves background status and active-session rules', () => {
		const completed = {
			sessionId: 'ses-active',
			title: '后台任务已完成',
			body: 'toolrun-1 已完成',
			notificationKind: 'tool_run_completion' as const,
			toolRunKind: 'background' as const,
			toolRunId: 'toolrun-1',
			toolRunStatus: 'completed' as const,
		};
		expect(projectToolRunCompletionToast(completed, 'ses-active')).toBeNull();
		expect(projectToolRunCompletionToast(completed, 'ses-other')).toEqual({
			message: '后台任务完成: toolrun-1',
			type: 'success',
			durationMs: 4000,
		});
		expect(
			projectToolRunCompletionToast(
				{ ...completed, toolRunStatus: 'failed', sessionId: '' },
				null,
			),
		).toEqual({ message: '后台任务失败: toolrun-1', type: 'error', durationMs: 4000 });
		expect(
			projectToolRunCompletionToast({ ...completed, toolRunStatus: undefined }, null),
		).toBeNull();
	});

	it('projects scheduled title and body through the common notification toast', () => {
		expect(
			projectToolRunCompletionToast(
				{
					sessionId: '',
					title: '备份计划',
					body: '任务已执行',
					notificationKind: 'tool_run_completion',
					toolRunKind: 'scheduled',
					toolRunId: 'toolrun-2',
				},
				null,
			),
		).toEqual({ message: '备份计划: 任务已执行', type: 'info', durationMs: 5000 });
	});
});
