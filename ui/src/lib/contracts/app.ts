/** App-shell IPC event contract at the frontend boundary. */

import type { TauriEvent } from './session.ts';
import {
	INTERACTION_KIND_VALUES,
	INTERACTION_STATUS_VALUES,
	type InteractionKind as GeneratedInteractionKind,
	type InteractionOwner as InteractionOwnerWire,
	type InteractionStatus as GeneratedInteractionStatus,
} from './generatedCommands.ts';

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
export type InteractionKind = GeneratedInteractionKind;
export type InteractionStatus = GeneratedInteractionStatus;
export type InteractionOwner =
	| { kind: 'session'; sessionId: string }
	| { kind: 'scheduled_tool_run'; toolRunId: string }
	| { kind: 'app_command' };

export function interactionOwnerToWire(owner: InteractionOwner): InteractionOwnerWire {
	switch (owner.kind) {
		case 'session':
			return { kind: 'session', session_id: owner.sessionId };
		case 'scheduled_tool_run':
			return { kind: 'scheduled_tool_run', tool_run_id: owner.toolRunId };
		case 'app_command':
			return { kind: 'app_command' };
	}
}
interface InteractionRequestBase {
	id: string;
	kind: InteractionKind;
	status: InteractionStatus;
	options: string[];
	toolName?: string;
	riskLevel?: RiskLevel;
	summary?: string;
	permissionKey?: string;
	invocationStepId?: string;
	toolIndex?: number;
	toolCallId?: string;
	createdAt: string;
	expiresAt?: string;
	response?: unknown;
}
export type InteractionRequest =
	| (InteractionRequestBase & {
			owner: Extract<InteractionOwner, { kind: 'session' }>;
			sessionId: string;
	  })
	| (InteractionRequestBase & {
			owner: Extract<InteractionOwner, { kind: 'scheduled_tool_run' }>;
			sessionId?: string;
	  })
	| (InteractionRequestBase & {
			owner: Extract<InteractionOwner, { kind: 'app_command' }>;
		sessionId?: never;
	  });

export function isSessionInteractionRequest(
	request: InteractionRequest,
): request is Extract<InteractionRequest, { owner: { kind: 'session' } }> {
	return request.owner.kind === 'session' && request.sessionId === request.owner.sessionId;
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
		session_id?: string;
		owner:
			| { kind: 'session'; session_id: string }
			| { kind: 'scheduled_tool_run'; tool_run_id: string }
			| { kind: 'app_command' };
		kind: InteractionKind;
		status: InteractionStatus;
		options?: string[];
		tool_name?: string;
		risk_level?: RiskLevel;
		summary?: string;
		permission_key?: string;
		invocation_step_id?: string;
		tool_index?: number;
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

function validPendingPermissionDeadline(
	kind: unknown,
	status: unknown,
	expiresAt: unknown,
): boolean {
	if (status !== 'pending' || kind === 'ask') return true;
	return typeof expiresAt === 'string' && Number.isFinite(Date.parse(expiresAt));
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

export function mapInteractionOwner(
	value: unknown,
	sessionId: string | undefined,
): InteractionOwner | null {
	if (!isRecord(value) || typeof value.kind !== 'string') return null;
	switch (value.kind) {
		case 'session':
			if (
				Object.keys(value).length !== 2 ||
				typeof value.session_id !== 'string' ||
				!value.session_id ||
				sessionId !== value.session_id
			)
				return null;
			return { kind: 'session', sessionId: value.session_id };
		case 'scheduled_tool_run':
			if (
				Object.keys(value).length !== 2 ||
				typeof value.tool_run_id !== 'string' ||
				!value.tool_run_id
			)
				return null;
			return { kind: 'scheduled_tool_run', toolRunId: value.tool_run_id };
		case 'app_command':
			if (Object.keys(value).length !== 1 || sessionId !== undefined) return null;
			return { kind: 'app_command' };
		default:
			return null;
	}
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
			const sessionId = p.session_id;
			const kind = p.kind;
			const status = p.status;
			const createdAt = requiredString(p, 'created_at');
			const options = p.options === undefined ? [] : p.options;
			const toolIndex = p.tool_index;
			if (
				id === null ||
				(sessionId !== undefined && (typeof sessionId !== 'string' || !sessionId)) ||
				!isOneOf(kind, INTERACTION_KIND_VALUES) ||
				!isOneOf(status, INTERACTION_STATUS_VALUES) ||
				createdAt === null ||
				!stringArray(options) ||
				!optionalStringIsValid(p, 'tool_name') ||
				!optionalOneOfIsValid(p, 'risk_level', RISK_LEVELS) ||
				!optionalStringIsValid(p, 'summary') ||
				!optionalStringIsValid(p, 'permission_key') ||
				!optionalStringIsValid(p, 'invocation_step_id') ||
				!optionalStringIsValid(p, 'tool_call_id') ||
				!optionalStringIsValid(p, 'expires_at') ||
				!validPendingPermissionDeadline(kind, status, p.expires_at) ||
				(toolIndex !== undefined &&
					(!finiteNumber(toolIndex) ||
						!Number.isInteger(toolIndex) ||
						toolIndex < 0 ||
						toolIndex > 4_294_967_295))
			)
				return null;
			const owner = mapInteractionOwner(p.owner, sessionId as string | undefined);
			if (!owner) return null;

			const wire = p as AppWirePayloadMap['interaction:requested'];
			const payload = {
					id,
					...(sessionId === undefined ? {} : { sessionId }),
					owner,
					kind,
					status,
					options,
					...(wire.tool_name ? { toolName: wire.tool_name } : {}),
					...(wire.risk_level ? { riskLevel: wire.risk_level } : {}),
					...(wire.summary ? { summary: wire.summary } : {}),
					...(wire.permission_key ? { permissionKey: wire.permission_key } : {}),
					...(wire.invocation_step_id
						? { invocationStepId: wire.invocation_step_id }
						: {}),
					...(wire.tool_index != null ? { toolIndex: wire.tool_index } : {}),
					...(wire.tool_call_id ? { toolCallId: wire.tool_call_id } : {}),
					createdAt,
					...(wire.expires_at ? { expiresAt: wire.expires_at } : {}),
			};
			return { ...tauriEvent, payload: payload as InteractionRequest };
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
