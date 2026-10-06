import { afterEach, describe, expect, it } from 'vitest';
import {
	DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS,
	normalizeToolRunCompletionNotificationChannels,
	setToolRunCompletionNotificationChannels,
	shouldShowToolRunCompletionInApp,
} from './toolRunCompletionNotificationSettings.ts';

afterEach(() => {
	setToolRunCompletionNotificationChannels(DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS);
});

describe('ToolRun completion notification settings', () => {
	it('defaults missing channels to enabled', () => {
		expect(normalizeToolRunCompletionNotificationChannels(undefined)).toEqual({
			in_app: true,
			windows: true,
		});
	});

	it('keeps app and Windows channels independent', () => {
		setToolRunCompletionNotificationChannels({ in_app: false, windows: true });
		expect(shouldShowToolRunCompletionInApp()).toBe(false);
		expect(
			normalizeToolRunCompletionNotificationChannels({ in_app: false, windows: true }),
		).toEqual({
			in_app: false,
			windows: true,
		});

		setToolRunCompletionNotificationChannels({ in_app: true, windows: false });
		expect(shouldShowToolRunCompletionInApp()).toBe(true);
		expect(
			normalizeToolRunCompletionNotificationChannels({ in_app: true, windows: false }),
		).toEqual({
			in_app: true,
			windows: false,
		});
	});
});
