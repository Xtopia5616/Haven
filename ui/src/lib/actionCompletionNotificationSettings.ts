import { get, writable } from 'svelte/store';

export interface ActionCompletionNotificationChannels {
	in_app: boolean;
	windows: boolean;
}

export const DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS: ActionCompletionNotificationChannels =
	{
		in_app: true,
		windows: true,
	};

const actionCompletionNotificationSettings = writable({
	...DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS,
});

export function normalizeActionCompletionNotificationChannels(
	value: unknown,
): ActionCompletionNotificationChannels {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) {
		return { ...DEFAULT_ACTION_COMPLETION_NOTIFICATION_CHANNELS };
	}
	const channels = value as Record<string, unknown>;
	return {
		in_app: channels.in_app !== false,
		windows: channels.windows !== false,
	};
}

export function setActionCompletionNotificationChannels(value: unknown): void {
	actionCompletionNotificationSettings.set(normalizeActionCompletionNotificationChannels(value));
}

export function shouldShowActionCompletionInApp(): boolean {
	return get(actionCompletionNotificationSettings).in_app;
}
