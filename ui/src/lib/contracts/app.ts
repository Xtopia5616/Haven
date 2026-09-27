/** App-shell IPC event contract at the frontend boundary. */

import type { TauriEvent } from './session.ts';

export const APP_EVENT_NAMES = [
	'app:bootstrap',
	'tray:status_changed',
	'mute:changed',
	'mcp:status_change',
	'skills:status_change',
	'interaction:requested',
	'hotkey:conflict',
	'hotkey:rebind',
	'llm:config_changed',
] as const;

export type AppEventName = (typeof APP_EVENT_NAMES)[number];
export type RiskLevel = 'safe' | 'low' | 'medium' | 'high' | 'critical';
export type TrayStatus = 'normal' | 'recording' | 'muted' | 'busy';
export type SkillsStatusOperation = 'refresh' | 'auto_refresh' | 'toggle';
export type McpStatus =
	'Disconnected' | 'Connecting' | 'Connected' | { Offline: { error: string } };

export interface AppBootstrapPayload {
	status: 'loading' | 'ready';
}
export interface TrayStatusPayload {
	status: TrayStatus;
	tooltip: string;
}
export interface MuteChangedPayload {
	muted: boolean;
}
export interface McpStatusPayload {
	name: string;
	status: McpStatus;
}
export interface SkillsStatusPayload {
	op: SkillsStatusOperation;
}
export type InteractionKind = 'ask' | 'confirm' | 'scheduled_confirm';
export type InteractionStatus = 'pending' | 'resolved' | 'expired' | 'cancelled';
export interface InteractionRequest {
	id: string;
	sessionId: string;
	kind: InteractionKind;
	status: InteractionStatus;
	prompt: string;
	options: string[];
	toolName?: string;
	riskLevel?: RiskLevel;
	summary?: string;
	permissionKey?: string;
	invocationStepId?: string;
	actionIndex?: number;
	toolCallId?: string;
	createdAt: string;
	expiresAt?: string;
	response?: unknown;
}
export interface HotkeyConflictPayload {
	binding: string;
	error: string;
}
export interface HotkeyRebindPayload {
	oldBinding: string;
	newBinding: string;
}

export interface AppEventPayloadMap {
	'app:bootstrap': AppBootstrapPayload;
	'tray:status_changed': TrayStatusPayload;
	'mute:changed': MuteChangedPayload;
	'mcp:status_change': McpStatusPayload;
	'skills:status_change': SkillsStatusPayload;
	'interaction:requested': InteractionRequest;
	'hotkey:conflict': HotkeyConflictPayload;
	'hotkey:rebind': HotkeyRebindPayload;
	'llm:config_changed': null;
}

interface AppWirePayloadMap {
	'app:bootstrap': { status: 'loading' | 'ready' };
	'tray:status_changed': { status: TrayStatus; tooltip: string };
	'mute:changed': { muted: boolean };
	'mcp:status_change': { name: string; status: McpStatus };
	'skills:status_change': { op: SkillsStatusOperation };
	'interaction:requested': {
		id: string;
		session_id: string;
		kind: InteractionKind;
		status: InteractionStatus;
		prompt: string;
		options?: string[];
		tool_name?: string;
		risk_level?: RiskLevel;
		summary?: string;
		permission_key?: string;
		invocation_step_id?: string;
		action_index?: number;
		tool_call_id?: string;
		created_at: string;
		expires_at?: string;
	};
	'hotkey:conflict': { binding: string; error: string };
	'hotkey:rebind': { old_binding: string; new_binding: string };
	'llm:config_changed': null;
}

type WireRecord = Record<string, unknown>;

const APP_EVENT_NAME_SET = new Set<string>(APP_EVENT_NAMES);
const MCP_STATUS_NAMES = ['Disconnected', 'Connecting', 'Connected'] as const;
const BOOTSTRAP_STATUSES = ['loading', 'ready'] as const;
const TRAY_STATUSES = ['normal', 'recording', 'muted', 'busy'] as const;
const SKILLS_STATUS_OPERATIONS = ['refresh', 'auto_refresh', 'toggle'] as const;
const INTERACTION_KINDS = ['ask', 'confirm', 'scheduled_confirm'] as const;
const INTERACTION_STATUSES = ['pending', 'resolved', 'expired', 'cancelled'] as const;
const RISK_LEVELS = ['safe', 'low', 'medium', 'high', 'critical'] as const;

function isRecord(value: unknown): value is WireRecord {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function finiteNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value);
}

function requiredString(record: WireRecord, field: string): string | null {
	return typeof record[field] === 'string' ? (record[field] as string) : null;
}

function optionalStringIsValid(record: WireRecord, field: string): boolean {
	const value = record[field];
	return value === undefined || typeof value === 'string';
}

function stringArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every((item) => typeof item === 'string');
}

function isOneOf<const Values extends readonly string[]>(
	value: unknown,
	values: Values,
): value is Values[number] {
	return typeof value === 'string' && values.includes(value);
}

function isMcpStatus(value: unknown): value is McpStatus {
	if (isOneOf(value, MCP_STATUS_NAMES)) return true;
	if (!isRecord(value) || Object.keys(value).length !== 1 || !isRecord(value.Offline))
		return false;
	return Object.keys(value.Offline).length === 1 && typeof value.Offline.error === 'string';
}

function optionalOneOfIsValid<const Values extends readonly string[]>(
	record: WireRecord,
	field: string,
	values: Values,
): boolean {
	const value = record[field];
	return value === undefined || isOneOf(value, values);
}

/** Convert one known app-shell event from an untrusted Rust/Tauri payload. */
export function mapAppEvent<K extends AppEventName>(
	event: TauriEvent<unknown> & { event: K },
): TauriEvent<AppEventPayloadMap[K]> | null;
export function mapAppEvent(event: unknown): TauriEvent<AppEventPayloadMap[AppEventName]> | null;
export function mapAppEvent(event: unknown): TauriEvent<AppEventPayloadMap[AppEventName]> | null {
	if (
		!isRecord(event) ||
		typeof event.event !== 'string' ||
		!APP_EVENT_NAME_SET.has(event.event) ||
		!finiteNumber(event.id)
	) {
		return null;
	}

	const tauriEvent = event as unknown as TauriEvent<unknown>;
	const p = event.payload;
	if (event.event === 'llm:config_changed') {
		// This unit event has always projected to null regardless of its wire payload.
		return { ...tauriEvent, payload: null };
	}
	if (!isRecord(p)) return null;

	switch (event.event) {
		case 'app:bootstrap':
			if (!isOneOf(p.status, BOOTSTRAP_STATUSES)) return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['app:bootstrap'] };
		case 'tray:status_changed':
			if (!isOneOf(p.status, TRAY_STATUSES) || typeof p.tooltip !== 'string') return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['tray:status_changed'] };
		case 'mute:changed':
			if (typeof p.muted !== 'boolean') return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['mute:changed'] };
		case 'mcp:status_change':
			if (typeof p.name !== 'string' || !isMcpStatus(p.status)) return null;
			return { ...tauriEvent, payload: { name: p.name, status: p.status } };
		case 'skills:status_change':
			if (!isOneOf(p.op, SKILLS_STATUS_OPERATIONS)) return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['skills:status_change'] };
		case 'interaction:requested': {
			const id = requiredString(p, 'id');
			const sessionId = requiredString(p, 'session_id');
			const kind = p.kind;
			const status = p.status;
			const prompt = requiredString(p, 'prompt');
			const createdAt = requiredString(p, 'created_at');
			const options = p.options === undefined ? [] : p.options;
			const actionIndex = p.action_index;
			if (
				id === null ||
				sessionId === null ||
				!isOneOf(kind, INTERACTION_KINDS) ||
				!isOneOf(status, INTERACTION_STATUSES) ||
				prompt === null ||
				createdAt === null ||
				!stringArray(options) ||
				!optionalStringIsValid(p, 'tool_name') ||
				!optionalOneOfIsValid(p, 'risk_level', RISK_LEVELS) ||
				!optionalStringIsValid(p, 'summary') ||
				!optionalStringIsValid(p, 'permission_key') ||
				!optionalStringIsValid(p, 'invocation_step_id') ||
				!optionalStringIsValid(p, 'tool_call_id') ||
				!optionalStringIsValid(p, 'expires_at') ||
				(actionIndex !== undefined &&
					(!finiteNumber(actionIndex) ||
						!Number.isInteger(actionIndex) ||
						actionIndex < 0 ||
						actionIndex > 4_294_967_295))
			)
				return null;

			const wire = p as AppWirePayloadMap['interaction:requested'];
			return {
				...tauriEvent,
				payload: {
					id,
					sessionId,
					kind,
					status,
					prompt,
					options,
					...(wire.tool_name ? { toolName: wire.tool_name } : {}),
					...(wire.risk_level ? { riskLevel: wire.risk_level } : {}),
					...(wire.summary ? { summary: wire.summary } : {}),
					...(wire.permission_key ? { permissionKey: wire.permission_key } : {}),
					...(wire.invocation_step_id
						? { invocationStepId: wire.invocation_step_id }
						: {}),
					...(wire.action_index != null ? { actionIndex: wire.action_index } : {}),
					...(wire.tool_call_id ? { toolCallId: wire.tool_call_id } : {}),
					createdAt,
					...(wire.expires_at ? { expiresAt: wire.expires_at } : {}),
				},
			};
		}
		case 'hotkey:conflict': {
			const binding = requiredString(p, 'binding');
			const error = requiredString(p, 'error');
			if (binding === null || error === null) return null;
			return { ...tauriEvent, payload: { binding, error } };
		}
		case 'hotkey:rebind': {
			const oldBinding = requiredString(p, 'old_binding');
			const newBinding = requiredString(p, 'new_binding');
			if (oldBinding === null || newBinding === null) return null;
			return { ...tauriEvent, payload: { oldBinding, newBinding } };
		}
		case 'llm:config_changed':
			return { ...tauriEvent, payload: null };
	}
	return null;
}
