export const toolAgentPresenceStatuses = ['online', 'offline'] as const;

export type ToolAgentPresenceStatus = (typeof toolAgentPresenceStatuses)[number];

export function isToolAgentPresenceStatus(value: unknown): value is ToolAgentPresenceStatus {
	return toolAgentPresenceStatuses.some((status) => status === value);
}

export const toolExecutionModes = ['foreground', 'background'] as const;

export type ToolExecutionMode = (typeof toolExecutionModes)[number];

export function isToolExecutionMode(value: unknown): value is ToolExecutionMode {
	return toolExecutionModes.some((mode) => mode === value);
}

export const toolScheduleModes = ['tool', 'continue'] as const;

export type ToolScheduleMode = (typeof toolScheduleModes)[number];

export function isToolScheduleMode(value: unknown): value is ToolScheduleMode {
	return toolScheduleModes.some((mode) => mode === value);
}
