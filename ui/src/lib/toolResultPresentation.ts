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
