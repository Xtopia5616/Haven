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

export const toolFileSearchModes = ['filename', 'content'] as const;

export type ToolFileSearchMode = (typeof toolFileSearchModes)[number];

export function isToolFileSearchMode(value: unknown): value is ToolFileSearchMode {
	return toolFileSearchModes.some((mode) => mode === value);
}

export const toolMemoryRecallModes = ['keyword', 'hybrid'] as const;

export type ToolMemoryRecallMode = (typeof toolMemoryRecallModes)[number];

export function isToolMemoryRecallMode(value: unknown): value is ToolMemoryRecallMode {
	return toolMemoryRecallModes.some((mode) => mode === value);
}

export const toolAcPowerStates = ['offline', 'online', 'unknown'] as const;

export type ToolAcPowerState = (typeof toolAcPowerStates)[number];

export function isToolAcPowerState(value: unknown): value is ToolAcPowerState {
	return toolAcPowerStates.some((state) => state === value);
}

export const toolBatteryStates = ['high', 'low', 'critical', 'charging', 'unknown'] as const;

export type ToolBatteryState = (typeof toolBatteryStates)[number];

export function isToolBatteryState(value: unknown): value is ToolBatteryState {
	return toolBatteryStates.some((state) => state === value);
}

export const toolMediaModalities = [
	'text',
	'image',
	'audio',
	'video',
	'document',
	'unknown',
] as const;

export type ToolMediaModality = (typeof toolMediaModalities)[number];

export function isToolMediaModality(value: unknown): value is ToolMediaModality {
	return toolMediaModalities.some((modality) => modality === value);
}

export const toolMediaFileKinds = [
	'image',
	'audio',
	'video',
	'document',
	'text',
	'binary',
] as const;

export type ToolMediaFileKind = (typeof toolMediaFileKinds)[number];

export function isToolMediaFileKind(value: unknown): value is ToolMediaFileKind {
	return toolMediaFileKinds.some((fileKind) => fileKind === value);
}
