import { get, writable } from 'svelte/store';

export interface ToolRunCompletionNotificationChannels {
	in_app: boolean;
	windows: boolean;
}

export const DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS: ToolRunCompletionNotificationChannels =
	{
		in_app: true,
		windows: true,
	};

const toolRunCompletionNotificationSettings = writable({
	...DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS,
});

export function normalizeToolRunCompletionNotificationChannels(
	value: unknown,
): ToolRunCompletionNotificationChannels {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) {
		return { ...DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS };
	}
	const channels = value as Record<string, unknown>;
	return {
		in_app: channels.in_app !== false,
		windows: channels.windows !== false,
	};
}

export function setToolRunCompletionNotificationChannels(value: unknown): void {
	toolRunCompletionNotificationSettings.set(normalizeToolRunCompletionNotificationChannels(value));
}

export function shouldShowToolRunCompletionInApp(): boolean {
	return get(toolRunCompletionNotificationSettings).in_app;
}
