import { isToolRunStatus, type ToolRunStatus } from './contracts/toolRun.ts';
import type { ScheduleMode as GeneratedScheduleMode } from './contracts/generatedCommands.ts';

export { isScheduleMode as isToolScheduleMode } from './contracts/toolRun.ts';

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

export const toolFileOperations = [
	'read',
	'inspect',
	'stat',
	'hash',
	'write',
	'create_dir',
	'edit',
	'patch',
	'copy',
	'move',
	'delete',
	'list',
	'summary',
	'search',
	'outline',
] as const;

export type ToolFileOperation = (typeof toolFileOperations)[number];

export function isToolFileOperation(value: unknown): value is ToolFileOperation {
	return toolFileOperations.some((operation) => operation === value);
}

export const toolFileSymbolKinds = [
	'heading',
	'function',
	'class',
	'interface',
	'struct',
	'enum',
	'trait',
	'impl',
	'module',
	'type',
] as const;

export type ToolFileSymbolKind = (typeof toolFileSymbolKinds)[number];

export function isToolFileSymbolKind(value: unknown): value is ToolFileSymbolKind {
	return toolFileSymbolKinds.some((kind) => kind === value);
}

export const toolInputOperations = [
	'type',
	'type_element',
	'key',
	'click',
	'click_element',
	'move',
	'scroll',
] as const;

export type ToolInputOperation = (typeof toolInputOperations)[number];

export function isToolInputOperation(value: unknown): value is ToolInputOperation {
	return toolInputOperations.some((operation) => operation === value);
}

export const toolInputButtons = ['left', 'right', 'middle'] as const;

export type ToolInputButton = (typeof toolInputButtons)[number];

export function isToolInputButton(value: unknown): value is ToolInputButton {
	return toolInputButtons.some((button) => button === value);
}

export const toolMediaOperations = [
	'inspect',
	'describe',
	'ocr',
	'transcribe',
	'extract',
	'render',
	'generate',
	'record',
	'play',
	'speak',
	'volume_get',
	'volume_set',
	'mute_get',
	'mute_set',
	'read',
	'summary',
] as const;

export type ToolMediaOperation = (typeof toolMediaOperations)[number];

export function isToolMediaOperation(value: unknown): value is ToolMediaOperation {
	return toolMediaOperations.some((operation) => operation === value);
}

export const toolWindowOperations = [
	'list',
	'foreground',
	'focus',
	'close',
	'screenshot',
	'ocr',
	'ui_tree',
	'observe',
	'invoke',
	'set_value',
	'toggle',
	'select',
	'wait',
] as const;

export type ToolWindowOperation = (typeof toolWindowOperations)[number];

export function isToolWindowOperation(value: unknown): value is ToolWindowOperation {
	return toolWindowOperations.some((operation) => operation === value);
}

export const toolWindowWaitConditions = [
	'title_contains',
	'foreground_contains',
	'ui_text',
] as const;

export type ToolWindowWaitCondition = (typeof toolWindowWaitConditions)[number];

export function isToolWindowWaitCondition(value: unknown): value is ToolWindowWaitCondition {
	return toolWindowWaitConditions.some((condition) => condition === value);
}

export const toolWindowFormats = ['png'] as const;

export type ToolWindowFormat = (typeof toolWindowFormats)[number];

export function isToolWindowFormat(value: unknown): value is ToolWindowFormat {
	return toolWindowFormats.some((format) => format === value);
}

export const toolWindowControlTypes = [
	'Button',
	'Calendar',
	'CheckBox',
	'ComboBox',
	'Edit',
	'Hyperlink',
	'Image',
	'ListItem',
	'List',
	'Menu',
	'MenuBar',
	'MenuItem',
	'ProgressBar',
	'RadioButton',
	'ScrollBar',
	'Slider',
	'Spinner',
	'StatusBar',
	'Tab',
	'TabItem',
	'Text',
	'ToolBar',
	'ToolTip',
	'Tree',
	'TreeItem',
	'Custom',
	'Group',
	'Thumb',
	'DataGrid',
	'DataItem',
	'Document',
	'SplitButton',
	'Window',
	'Pane',
	'Header',
	'HeaderItem',
	'Table',
	'TitleBar',
	'Separator',
	'Unknown',
] as const;

export type ToolWindowControlType = (typeof toolWindowControlTypes)[number];

export function isToolWindowControlType(value: unknown): value is ToolWindowControlType {
	return toolWindowControlTypes.some((controlType) => controlType === value);
}

export type ToolScheduleMode = GeneratedScheduleMode;

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

export const toolRunsResultOperations = [
	'list',
	'inspect',
	'cancel',
	'result_injected',
	'tool_runs_list',
	'tool_runs_inspect',
	'tool_runs_cancel',
	'tool_runs_result_injected',
] as const;

export type ToolRunsResultOperation = (typeof toolRunsResultOperations)[number];

export type ToolRunsOperation = 'list' | 'inspect' | 'cancel' | 'result_injected';

export function isToolRunsResultOperation(value: unknown): value is ToolRunsResultOperation {
	return toolRunsResultOperations.some((operation) => operation === value);
}

export function normalizeToolRunsOperation(
	value: ToolRunsResultOperation | null | undefined,
): ToolRunsOperation | null | undefined {
	switch (value) {
		case 'tool_runs_list':
			return 'list';
		case 'tool_runs_inspect':
			return 'inspect';
		case 'tool_runs_cancel':
			return 'cancel';
		case 'tool_runs_result_injected':
			return 'result_injected';
		default:
			return value;
	}
}

export const toolScheduleResultOperations = [
	'set',
	'list',
	'cancel',
	'schedule_set',
	'schedule_list',
	'schedule_cancel',
] as const;

export type ToolScheduleResultOperation = (typeof toolScheduleResultOperations)[number];

export type ToolScheduleOperation = 'set' | 'list' | 'cancel';

export function isToolScheduleResultOperation(
	value: unknown,
): value is ToolScheduleResultOperation {
	return toolScheduleResultOperations.some((operation) => operation === value);
}

export function normalizeToolScheduleOperation(
	value: ToolScheduleResultOperation | null | undefined,
): ToolScheduleOperation | null | undefined {
	switch (value) {
		case 'schedule_set':
			return 'set';
		case 'schedule_list':
			return 'list';
		case 'schedule_cancel':
			return 'cancel';
		default:
			return value;
	}
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
