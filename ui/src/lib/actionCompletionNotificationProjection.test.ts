import { afterEach, describe, expect, it } from 'vitest';
import {
	createActionCompletionNotificationGate,
	MAX_PENDING_ACTION_COMPLETION_NOTIFICATIONS,
	projectActionCompletionToast,
} from './actionCompletionNotificationProjection.ts';
import {
	DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS,
	setActionCompletionNotificationChannels,
	shouldShowActionCompletionInApp,
} from './actionCompletionNotificationSettings.ts';

afterEach(() => {
	setActionCompletionNotificationChannels(DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS);
});

describe('action completion toast projection', () => {
	it('waits for persisted notification settings and then preserves event order', () => {
		const delivered: string[] = [];
		const gate = createActionCompletionNotificationGate((payload) =>
			delivered.push(payload.actionId || ''),
		);
		const first = {
			sessionId: '',
			title: 'scheduled',
			body: 'first',
			notificationKind: 'action_completion' as const,
			actionKind: 'scheduled' as const,
			actionId: 'act-1',
		};
		const second = { ...first, actionId: 'act-2' };
		gate.notify(first);
		gate.notify(second);
		expect(delivered).toEqual([]);
		gate.settingsLoaded();
		expect(delivered).toEqual(['act-1', 'act-2']);
	});

	it('caps the pending FIFO and drops arrivals after it is full', () => {
		const delivered: string[] = [];
		const gate = createActionCompletionNotificationGate((payload) =>
			delivered.push(payload.actionId || ''),
		);
		const first = {
			sessionId: '',
			title: 'scheduled',
			body: 'result',
			notificationKind: 'action_completion' as const,
			actionKind: 'scheduled' as const,
			actionId: '',
		};

		for (let index = 0; index < MAX_PENDING_ACTION_COMPLETION_NOTIFICATIONS; index += 1) {
			gate.notify({ ...first, actionId: `act-${index}` });
		}
		gate.notify({ ...first, actionId: 'act-overflow' });
		expect(delivered).toEqual([]);

		gate.settingsLoaded();
		expect(delivered).toHaveLength(MAX_PENDING_ACTION_COMPLETION_NOTIFICATIONS);
		expect(delivered[0]).toBe('act-0');
		expect(delivered.at(-1)).toBe(`act-${MAX_PENDING_ACTION_COMPLETION_NOTIFICATIONS - 1}`);
		expect(delivered).not.toContain('act-overflow');
	});

	it('drops queued toasts when hydrated settings disable the in-app channel', () => {
		const delivered: string[] = [];
		const gate = createActionCompletionNotificationGate((payload) => {
			if (shouldShowActionCompletionInApp()) delivered.push(payload.actionId || '');
		});
		const pending = {
			sessionId: '',
			title: 'scheduled',
			body: 'result',
			notificationKind: 'action_completion' as const,
			actionKind: 'scheduled' as const,
			actionId: 'act-disabled',
		};

		gate.notify(pending);
		setActionCompletionNotificationChannels({ in_app: false, windows: true });
		gate.settingsLoaded();

		expect(delivered).toEqual([]);
	});

	it('preserves background status and active-session rules', () => {
		const completed = {
			sessionId: 'ses-active',
			title: '后台任务已完成',
			body: 'act-1 已完成',
			notificationKind: 'action_completion' as const,
			actionKind: 'background' as const,
			actionId: 'act-1',
			actionStatus: 'completed' as const,
		};
		expect(projectActionCompletionToast(completed, 'ses-active')).toBeNull();
		expect(projectActionCompletionToast(completed, 'ses-other')).toEqual({
			message: '后台任务完成: act-1',
			type: 'success',
			durationMs: 4000,
		});
		expect(
			projectActionCompletionToast(
				{ ...completed, actionStatus: 'failed', sessionId: '' },
				null,
			),
		).toEqual({ message: '后台任务失败: act-1', type: 'error', durationMs: 4000 });
		expect(
			projectActionCompletionToast({ ...completed, actionStatus: undefined }, null),
		).toBeNull();
	});

	it('projects scheduled title and body through the common notification toast', () => {
		expect(
			projectActionCompletionToast(
				{
					sessionId: '',
					title: '备份计划',
					body: '任务已执行',
					notificationKind: 'action_completion',
					actionKind: 'scheduled',
					actionId: 'act-2',
				},
				null,
			),
		).toEqual({ message: '备份计划: 任务已执行', type: 'info', durationMs: 5000 });
	});
});
