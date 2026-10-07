import { get, writable } from 'svelte/store';
import { isRecord } from './contracts/objectGuards.ts';

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
	if (!isRecord(value)) {
		return { ...DEFAULT_TOOL_RUN_COMPLETION_NOTIFICATION_CHANNELS };
	}
	return {
		in_app: value.in_app !== false,
		windows: value.windows !== false,
	};
}

export function setToolRunCompletionNotificationChannels(value: unknown): void {
	toolRunCompletionNotificationSettings.set(normalizeToolRunCompletionNotificationChannels(value));
}

export function shouldShowToolRunCompletionInApp(): boolean {
	return get(toolRunCompletionNotificationSettings).in_app;
}
