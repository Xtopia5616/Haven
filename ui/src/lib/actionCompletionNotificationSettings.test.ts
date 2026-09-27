import { afterEach, describe, expect, it } from 'vitest';
import {
	DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS,
	normalizeActionCompletionNotificationChannels,
	setActionCompletionNotificationChannels,
	shouldShowActionCompletionInApp,
} from './actionCompletionNotificationSettings.ts';

afterEach(() => {
	setActionCompletionNotificationChannels(DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS);
});

describe('action completion notification settings', () => {
	it('defaults missing channels to enabled', () => {
		expect(normalizeActionCompletionNotificationChannels(undefined)).toEqual({
			in_app: true,
			windows: true,
		});
	});

	it('keeps app and Windows channels independent', () => {
		setActionCompletionNotificationChannels({ in_app: false, windows: true });
		expect(shouldShowActionCompletionInApp()).toBe(false);
		expect(
			normalizeActionCompletionNotificationChannels({ in_app: false, windows: true }),
		).toEqual({
			in_app: false,
			windows: true,
		});

		setActionCompletionNotificationChannels({ in_app: true, windows: false });
		expect(shouldShowActionCompletionInApp()).toBe(true);
		expect(
			normalizeActionCompletionNotificationChannels({ in_app: true, windows: false }),
		).toEqual({
			in_app: true,
			windows: false,
		});
	});
});
