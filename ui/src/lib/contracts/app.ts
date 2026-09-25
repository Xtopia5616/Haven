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
export type McpStatus =
	| 'Disconnected'
	| 'Connecting'
	| 'Connected'
	| { Offline: { error: string } };

export interface AppBootstrapPayload { status: 'loading' | 'ready'; }
export interface TrayStatusPayload { status: string; tooltip: string; }
export interface MuteChangedPayload { muted: boolean; }
export interface McpStatusPayload { name: string; status: McpStatus; }
export interface SkillsStatusPayload { op: string; }
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
export interface HotkeyConflictPayload { binding: string; error: string; }
export interface HotkeyRebindPayload { oldBinding: string; newBinding: string; }

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
	'tray:status_changed': { status: string; tooltip: string };
	'mute:changed': { muted: boolean };
	'mcp:status_change': { name: string; status: McpStatus };
	'skills:status_change': { op: string };
	'interaction:requested': {
		id: string;
		session_id: string;
		kind: InteractionKind;
		status: InteractionStatus;
		prompt: string;
		options?: string[];
		tool_name?: string | null;
		risk_level?: RiskLevel | null;
		summary?: string | null;
		permission_key?: string | null;
		invocation_step_id?: string | null;
		action_index?: number | null;
		tool_call_id?: string | null;
		created_at: string;
		expires_at?: string | null;
	};
	'hotkey:conflict': { binding: string; error: string };
	'hotkey:rebind': { old_binding: string; new_binding: string };
	'llm:config_changed': null;
}

type WireRecord = Record<string, unknown>;

const APP_EVENT_NAME_SET = new Set<string>(APP_EVENT_NAMES);

function isRecord(value: unknown): value is WireRecord {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function finiteNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value);
}

function requiredString(record: WireRecord, field: string): string | null {
	return typeof record[field] === 'string' ? record[field] as string : null;
}

function optionalNullableStringIsValid(record: WireRecord, field: string): boolean {
	const value = record[field];
	return value === undefined || value === null || typeof value === 'string';
}

function stringArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every((item) => typeof item === 'string');
}

/** Keep serde's externally-tagged MCP status value opaque for forward compatibility. */
function isMcpStatus(value: unknown): boolean {
	if (typeof value === 'string') return true;
	return isRecord(value);
}

/** Convert one known app-shell event from an untrusted Rust/Tauri payload. */
export function mapAppEvent<K extends AppEventName>(
	event: TauriEvent<unknown> & { event: K },
): TauriEvent<AppEventPayloadMap[K]> | null;
export function mapAppEvent(
	event: unknown,
): TauriEvent<AppEventPayloadMap[AppEventName]> | null;
export function mapAppEvent(
	event: unknown,
): TauriEvent<AppEventPayloadMap[AppEventName]> | null {
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
			if (typeof p.status !== 'string') return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['app:bootstrap'] };
		case 'tray:status_changed':
			if (typeof p.status !== 'string' || typeof p.tooltip !== 'string') return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['tray:status_changed'] };
		case 'mute:changed':
			if (typeof p.muted !== 'boolean') return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['mute:changed'] };
		case 'mcp:status_change':
			if (typeof p.name !== 'string' || !isMcpStatus(p.status)) return null;
			// Preserve the whole wrapper and status value, including additive fields.
			return { ...tauriEvent, payload: p as AppWirePayloadMap['mcp:status_change'] };
		case 'skills:status_change':
			if (typeof p.op !== 'string') return null;
			return { ...tauriEvent, payload: p as AppWirePayloadMap['skills:status_change'] };
		case 'interaction:requested': {
			const id = requiredString(p, 'id');
			const sessionId = requiredString(p, 'session_id');
			const kind = requiredString(p, 'kind');
			const status = requiredString(p, 'status');
			const prompt = requiredString(p, 'prompt');
			const createdAt = requiredString(p, 'created_at');
			const options = p.options === undefined ? [] : p.options;
			const actionIndex = p.action_index;
			if (
				id === null || sessionId === null || kind === null || status === null ||
				prompt === null || createdAt === null || !stringArray(options) ||
				!optionalNullableStringIsValid(p, 'tool_name') ||
				!optionalNullableStringIsValid(p, 'risk_level') ||
				!optionalNullableStringIsValid(p, 'summary') ||
				!optionalNullableStringIsValid(p, 'permission_key') ||
				!optionalNullableStringIsValid(p, 'invocation_step_id') ||
				!optionalNullableStringIsValid(p, 'tool_call_id') ||
				!optionalNullableStringIsValid(p, 'expires_at') ||
				(actionIndex !== undefined && actionIndex !== null &&
					(!finiteNumber(actionIndex) || !Number.isInteger(actionIndex) ||
						actionIndex < 0 || actionIndex > 4_294_967_295))
			) return null;

			const wire = p as AppWirePayloadMap['interaction:requested'];
			return { ...tauriEvent, payload: {
				id,
				sessionId,
				kind: kind as InteractionKind,
				status: status as InteractionStatus,
				prompt,
				options,
				...(wire.tool_name ? { toolName: wire.tool_name } : {}),
				...(wire.risk_level ? { riskLevel: wire.risk_level } : {}),
				...(wire.summary ? { summary: wire.summary } : {}),
				...(wire.permission_key ? { permissionKey: wire.permission_key } : {}),
				...(wire.invocation_step_id ? { invocationStepId: wire.invocation_step_id } : {}),
				...(wire.action_index != null ? { actionIndex: wire.action_index } : {}),
				...(wire.tool_call_id ? { toolCallId: wire.tool_call_id } : {}),
				createdAt,
				...(wire.expires_at ? { expiresAt: wire.expires_at } : {}),
			} };
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
