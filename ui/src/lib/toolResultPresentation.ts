import { isToolRunStatus, type ToolRunStatus } from './contracts/toolRun.ts';
import { isOneOf } from './contracts/valueGuards.ts';

export const TOOL_AGENT_PRESENCE_STATUSES = ['online', 'offline'] as const;

export type ToolAgentPresenceStatus = (typeof TOOL_AGENT_PRESENCE_STATUSES)[number];

export function isToolAgentPresenceStatus(value: unknown): value is ToolAgentPresenceStatus {
	return isOneOf(value, TOOL_AGENT_PRESENCE_STATUSES);
}

export const TOOL_FILE_OPERATIONS = [
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

export type ToolFileOperation = (typeof TOOL_FILE_OPERATIONS)[number];

export function isToolFileOperation(value: unknown): value is ToolFileOperation {
	return isOneOf(value, TOOL_FILE_OPERATIONS);
}

export const TOOL_FILE_SYMBOL_KINDS = [
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

export type ToolFileSymbolKind = (typeof TOOL_FILE_SYMBOL_KINDS)[number];

export function isToolFileSymbolKind(value: unknown): value is ToolFileSymbolKind {
	return isOneOf(value, TOOL_FILE_SYMBOL_KINDS);
}

export const TOOL_INPUT_OPERATIONS = [
	'type',
	'type_element',
	'key',
	'click',
	'click_element',
	'move',
	'scroll',
] as const;

export type ToolInputOperation = (typeof TOOL_INPUT_OPERATIONS)[number];

export function isToolInputOperation(value: unknown): value is ToolInputOperation {
	return isOneOf(value, TOOL_INPUT_OPERATIONS);
}

export const TOOL_INPUT_BUTTONS = ['left', 'right', 'middle'] as const;

export type ToolInputButton = (typeof TOOL_INPUT_BUTTONS)[number];

export function isToolInputButton(value: unknown): value is ToolInputButton {
	return isOneOf(value, TOOL_INPUT_BUTTONS);
}

export const TOOL_MEDIA_OPERATIONS = [
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

export type ToolMediaOperation = (typeof TOOL_MEDIA_OPERATIONS)[number];

export function isToolMediaOperation(value: unknown): value is ToolMediaOperation {
	return isOneOf(value, TOOL_MEDIA_OPERATIONS);
}

export const TOOL_WINDOW_OPERATIONS = [
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

export type ToolWindowOperation = (typeof TOOL_WINDOW_OPERATIONS)[number];

export function isToolWindowOperation(value: unknown): value is ToolWindowOperation {
	return isOneOf(value, TOOL_WINDOW_OPERATIONS);
}

export const TOOL_WINDOW_WAIT_CONDITIONS = [
	'title_contains',
	'foreground_contains',
	'ui_text',
] as const;

export type ToolWindowWaitCondition = (typeof TOOL_WINDOW_WAIT_CONDITIONS)[number];

export function isToolWindowWaitCondition(value: unknown): value is ToolWindowWaitCondition {
	return isOneOf(value, TOOL_WINDOW_WAIT_CONDITIONS);
}

export const TOOL_WINDOW_FORMATS = ['png'] as const;

export type ToolWindowFormat = (typeof TOOL_WINDOW_FORMATS)[number];

export function isToolWindowFormat(value: unknown): value is ToolWindowFormat {
	return isOneOf(value, TOOL_WINDOW_FORMATS);
}

export const TOOL_WINDOW_CONTROL_TYPES = [
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

export type ToolWindowControlType = (typeof TOOL_WINDOW_CONTROL_TYPES)[number];

export function isToolWindowControlType(value: unknown): value is ToolWindowControlType {
	return isOneOf(value, TOOL_WINDOW_CONTROL_TYPES);
}

export const TOOL_FILE_SEARCH_MODES = ['filename', 'content'] as const;

export type ToolFileSearchMode = (typeof TOOL_FILE_SEARCH_MODES)[number];

export function isToolFileSearchMode(value: unknown): value is ToolFileSearchMode {
	return isOneOf(value, TOOL_FILE_SEARCH_MODES);
}

export const TOOL_MEMORY_OPERATIONS = [
	'search',
	'list',
	'history',
	'valid_at',
	'remember',
	'forget',
	'recall',
] as const;

export type ToolMemoryOperation = (typeof TOOL_MEMORY_OPERATIONS)[number];

export function isToolMemoryOperation(value: unknown): value is ToolMemoryOperation {
	return isOneOf(value, TOOL_MEMORY_OPERATIONS);
}

export const TOOL_SYSTEM_SCOPES = [
	'info',
	'overview',
	'env',
	'registry',
	'power',
	'display',
	'displays',
] as const;

export type ToolSystemScope = (typeof TOOL_SYSTEM_SCOPES)[number];

export function isToolSystemScope(value: unknown): value is ToolSystemScope {
	return isOneOf(value, TOOL_SYSTEM_SCOPES);
}

export const TOOL_MEMORY_RECALL_MODES = ['keyword', 'hybrid'] as const;

export type ToolMemoryRecallMode = (typeof TOOL_MEMORY_RECALL_MODES)[number];

export function isToolMemoryRecallMode(value: unknown): value is ToolMemoryRecallMode {
	return isOneOf(value, TOOL_MEMORY_RECALL_MODES);
}

export type ToolRunResultStatus = ToolRunStatus | 'not_found';

export function isToolRunResultStatus(value: unknown): value is ToolRunResultStatus {
	return value === 'not_found' || isToolRunStatus(value);
}

export const TOOL_RUNS_RESULT_OPERATIONS = [
	'start',
	'list',
	'inspect',
	'cancel',
	'result_injected',
] as const;

export type ToolRunsResultOperation = (typeof TOOL_RUNS_RESULT_OPERATIONS)[number];

export function isToolRunsResultOperation(value: unknown): value is ToolRunsResultOperation {
	return isOneOf(value, TOOL_RUNS_RESULT_OPERATIONS);
}

export const TOOL_SCHEDULE_RESULT_OPERATIONS = ['set', 'list', 'cancel'] as const;

export type ToolScheduleResultOperation = (typeof TOOL_SCHEDULE_RESULT_OPERATIONS)[number];

export function isToolScheduleResultOperation(
	value: unknown,
): value is ToolScheduleResultOperation {
	return isOneOf(value, TOOL_SCHEDULE_RESULT_OPERATIONS);
}

export const TOOL_AC_POWER_STATES = ['offline', 'online', 'unknown'] as const;

export type ToolAcPowerState = (typeof TOOL_AC_POWER_STATES)[number];

export function isToolAcPowerState(value: unknown): value is ToolAcPowerState {
	return isOneOf(value, TOOL_AC_POWER_STATES);
}

export const TOOL_BATTERY_STATES = ['high', 'low', 'critical', 'charging', 'unknown'] as const;

export type ToolBatteryState = (typeof TOOL_BATTERY_STATES)[number];

export function isToolBatteryState(value: unknown): value is ToolBatteryState {
	return isOneOf(value, TOOL_BATTERY_STATES);
}

export const TOOL_MEDIA_MODALITIES = [
	'text',
	'image',
	'audio',
	'video',
	'document',
	'unknown',
] as const;

export type ToolMediaModality = (typeof TOOL_MEDIA_MODALITIES)[number];

export function isToolMediaModality(value: unknown): value is ToolMediaModality {
	return isOneOf(value, TOOL_MEDIA_MODALITIES);
}

export const TOOL_MEDIA_FILE_KINDS = [
	'image',
	'audio',
	'video',
	'document',
	'text',
	'binary',
] as const;

export type ToolMediaFileKind = (typeof TOOL_MEDIA_FILE_KINDS)[number];

export function isToolMediaFileKind(value: unknown): value is ToolMediaFileKind {
	return isOneOf(value, TOOL_MEDIA_FILE_KINDS);
}
