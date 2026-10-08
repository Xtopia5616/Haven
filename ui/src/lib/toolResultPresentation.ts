import { isToolRunStatus, type ToolRunStatus } from './contracts/toolRun.ts';

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

export const toolMemoryOperations = ['search', 'list', 'remember', 'forget', 'recall'] as const;

export type ToolMemoryOperation = (typeof toolMemoryOperations)[number];

export function isToolMemoryOperation(value: unknown): value is ToolMemoryOperation {
	return toolMemoryOperations.some((operation) => operation === value);
}

export const toolProcessOperations = ['list', 'kill'] as const;

export type ToolProcessOperation = (typeof toolProcessOperations)[number];

export function isToolProcessOperation(value: unknown): value is ToolProcessOperation {
	return toolProcessOperations.some((operation) => operation === value);
}

export const toolProcessStatuses = [
	'Idle',
	'Run',
	'Sleep',
	'Stop',
	'Zombie',
	'Tracing',
	'Dead',
	'Wakekill',
	'Waking',
	'Parked',
	'LockBlocked',
	'UninterruptibleDiskSleep',
	'Suspended',
	'Unknown',
] as const;

export type ToolProcessStatus = (typeof toolProcessStatuses)[number];

export function isToolProcessStatus(value: unknown): value is ToolProcessStatus {
	return toolProcessStatuses.some((status) => status === value);
}

export const toolSystemScopes = [
	'info',
	'overview',
	'env',
	'registry',
	'power',
	'display',
	'displays',
] as const;

export type ToolSystemScope = (typeof toolSystemScopes)[number];

export function isToolSystemScope(value: unknown): value is ToolSystemScope {
	return toolSystemScopes.some((scope) => scope === value);
}

export const toolMemoryRecallModes = ['keyword', 'hybrid'] as const;

export type ToolMemoryRecallMode = (typeof toolMemoryRecallModes)[number];

export function isToolMemoryRecallMode(value: unknown): value is ToolMemoryRecallMode {
	return toolMemoryRecallModes.some((mode) => mode === value);
}

export type ToolRunResultStatus = ToolRunStatus | 'not_found';

export function isToolRunResultStatus(value: unknown): value is ToolRunResultStatus {
	return value === 'not_found' || isToolRunStatus(value);
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
