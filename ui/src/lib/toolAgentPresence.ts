export const toolAgentPresenceStatuses = ['online', 'offline'] as const;

export type ToolAgentPresenceStatus = (typeof toolAgentPresenceStatuses)[number];

export function isToolAgentPresenceStatus(value: unknown): value is ToolAgentPresenceStatus {
	return toolAgentPresenceStatuses.some((status) => status === value);
}
