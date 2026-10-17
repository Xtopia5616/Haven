import { afterEach, describe, expect, it } from 'vitest';
import {
	isNotificationInAppEnabled,
	onNotificationSettingsChanged,
	publishNotificationSettingsChanged,
	shouldApplyInitialNotificationSnapshot,
} from './notificationSettingsEvents.ts';

describe('notification settings events', () => {
	it('publishes a new snapshot to the layout subscriber', () => {
		let received: unknown;
		const unsubscribe = onNotificationSettingsChanged((notification) => {
			received = notification;
		});
		const notification = {
			session_created: { in_app: false, windows: false },
			tool_run_completed: { in_app: false, windows: true },
		};

		publishNotificationSettingsChanged(notification);

		expect(received).toEqual(notification);
		unsubscribe();
	});

	it('reads ToolRun notification policy from the same notification snapshot', () => {
		expect(
			isNotificationInAppEnabled(
				{ tool_run_completed: { in_app: false, windows: true } },
				'tool_run_completed',
			),
		).toBe(false);
		expect(isNotificationInAppEnabled({}, 'tool_run_completed')).toBe(true);
	});

	it('rejects an initial snapshot after a newer settings event has arrived', () => {
		expect(shouldApplyInitialNotificationSnapshot(4, 4)).toBe(true);
		expect(shouldApplyInitialNotificationSnapshot(4, 5)).toBe(false);
	});
});
