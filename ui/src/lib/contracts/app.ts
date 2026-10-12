/** App-shell IPC event contract at the frontend boundary. */

import { isValidTauriEventId, type TauriEvent } from './tauriEvent.ts';
import { isMcpClientStatus } from './mcpClientStatus.ts';
import { isRecord } from './objectGuards.ts';
import { matchesGeneratedEventPayload } from './generatedWireShape.ts';
import { isBoolean, isFiniteNumber, isOneOf, isString, isStringArray } from './valueGuards.ts';
import {
	isCanonicalEntityId,
	optionalCanonicalIdFieldIsValid,
	optionalStringFieldIsValid,
	readStringField,
	type WireRecord,
} from './wireGuards.ts';
import {
	APP_EVENT_NAMES,
	BOOTSTRAP_STATUS_VALUES,
	CONFIRMATION_RESOLUTION_RESULT_VALUES,
	INTERACTION_KIND_VALUES,
	INTERACTION_STATUS_VALUES,
	SKILLS_STATUS_OPERATION_VALUES,
	RISK_LEVEL_VALUES,
	TRAY_STATUS_EVENT_VALUE_VALUES,
	type AppBootstrapEvent as GeneratedAppBootstrapEvent,
	type HotkeyConflictEvent as GeneratedHotkeyConflictEvent,
	type InteractionKind,
	type InteractionOwner as InteractionOwnerWire,
	type InteractionRequestedEvent as GeneratedInteractionRequestedEvent,
	type InteractionStatus,
	type ConfirmationResolutionResult,
	type McpStatusChangedEvent as GeneratedMcpStatusChangedEvent,
	type MuteChangedEvent as GeneratedMuteChangedEvent,
	type RiskLevel,
	type SkillsStatusChangedEvent as GeneratedSkillsStatusChangedEvent,
	type TrayStatusChangedEvent as GeneratedTrayStatusChangedEvent,
} from './generatedCommands.ts';

export type AppEventName = (typeof APP_EVENT_NAMES)[number];
export type InteractionOwnerView =
	| { kind: 'session'; sessionId: string }
	| { kind: 'scheduled_tool_run'; toolRunId: string }
	| { kind: 'app_command' };

export function interactionOwnerToWire(owner: InteractionOwnerView): InteractionOwnerWire {
	switch (owner.kind) {
		case 'session':
			return { kind: 'session', session_id: owner.sessionId };
		case 'scheduled_tool_run':
			return { kind: 'scheduled_tool_run', tool_run_id: owner.toolRunId };
		case 'app_command':
			return { kind: 'app_command' };
	}
}

/** Reject unknown resolution statuses before they enter interaction state. */
export function parseConfirmationResolutionResult(value: unknown): ConfirmationResolutionResult {
	if (!isOneOf(value, CONFIRMATION_RESOLUTION_RESULT_VALUES)) {
		throw new Error('Invalid confirmation resolution result');
	}
	return value;
}
interface InteractionRequestBase {
	requestId: string;
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
}
interface ConfirmInteractionDetails {
	toolName: string;
	riskLevel: RiskLevel;
	summary: string;
	toolIndex: number;
	permissionKey?: string;
	toolCallId?: string;
}
type SessionInteractionOwner = Extract<InteractionOwnerView, { kind: 'session' }>;
type ScheduledToolRunInteractionOwner = Extract<
	InteractionOwnerView,
	{ kind: 'scheduled_tool_run' }
>;
type AppCommandInteractionOwner = Extract<InteractionOwnerView, { kind: 'app_command' }>;

export type InteractionRequest =
	| (InteractionRequestBase & {
			kind: Extract<InteractionKind, 'ask'>;
			owner: SessionInteractionOwner;
			sessionId: string;
			/** Renderer-local answer projection for an Ask interaction. */
			response?: AskResponseView;
	  })
	| (InteractionRequestBase & {
			kind: Extract<InteractionKind, 'confirm'>;
			owner: SessionInteractionOwner;
			sessionId: string;
			invocationStepId: string;
	  } & ConfirmInteractionDetails & {
				response?: never;
			})
	| (InteractionRequestBase & {
			kind: Extract<InteractionKind, 'confirm'>;
			owner: AppCommandInteractionOwner;
			sessionId?: never;
			invocationStepId?: never;
			toolCallId?: never;
	  } & ConfirmInteractionDetails & {
				response?: never;
			})
	| (InteractionRequestBase & {
			kind: Extract<InteractionKind, 'scheduled_confirm'>;
			owner: ScheduledToolRunInteractionOwner;
			sessionId?: string;
			toolName: string;
			summary: string;
			permissionKey: string;
			response?: never;
	  });

/** Normalized renderer view for a resolved Ask interaction's response. */
export type AskResponseView =
	{ answer: string; ignored?: false } | { ignored: true; answer?: never };

/** Runtime producer/owner combinations used by the interaction event and resume projections. */
export function isValidInteractionKindOwnerPair(
	kind: unknown,
	owner: InteractionOwnerView,
): boolean {
	switch (owner.kind) {
		case 'session':
			return kind === 'ask' || kind === 'confirm';
		case 'scheduled_tool_run':
			return kind === 'scheduled_confirm';
		case 'app_command':
			return kind === 'confirm';
	}
}

export function isSessionInteractionRequest(
	request: InteractionRequest,
): request is Extract<InteractionRequest, { owner: { kind: 'session' } }> {
	return request.owner.kind === 'session' && request.sessionId === request.owner.sessionId;
}
export interface HotkeyRebindPayload {
	oldBinding: string;
	newBinding: string;
}

export interface AppEventPayloadMap {
	'app:bootstrap': GeneratedAppBootstrapEvent;
	'tray:status_changed': GeneratedTrayStatusChangedEvent;
	'mute:changed': GeneratedMuteChangedEvent;
	'mcp:status_change': GeneratedMcpStatusChangedEvent;
	'skills:status_change': GeneratedSkillsStatusChangedEvent;
	'interaction:requested': InteractionRequest;
	'hotkey:conflict': GeneratedHotkeyConflictEvent;
	'hotkey:rebind': HotkeyRebindPayload;
	'llm:config_changed': null;
}

const APP_EVENT_NAME_SET = new Set<string>(APP_EVENT_NAMES);

function validPendingPermissionDeadline(
	kind: unknown,
	status: unknown,
	expiresAt: unknown,
): boolean {
	if (status !== 'pending' || kind === 'ask') return true;
	return isString(expiresAt) && isFiniteNumber(Date.parse(expiresAt));
}

function optionalOneOfIsValid<const Values extends readonly string[]>(
	record: WireRecord,
	field: string,
	values: Values,
): boolean {
	const value = record[field];
	return value === undefined || isOneOf(value, values);
}

function isNonEmptyString(value: unknown): value is string {
	return isString(value) && value.trim().length > 0;
}

export function mapInteractionOwner(
	value: unknown,
	sessionId: string | undefined,
): InteractionOwnerView | null {
	if (!isRecord(value) || !isString(value.kind)) return null;
	switch (value.kind) {
		case 'session':
			if (
				Object.keys(value).length !== 2 ||
				!isCanonicalEntityId(value.session_id, 'ses') ||
				sessionId !== value.session_id
			)
				return null;
			return { kind: 'session', sessionId: value.session_id };
		case 'scheduled_tool_run':
			if (
				Object.keys(value).length !== 2 ||
				!isCanonicalEntityId(value.tool_run_id, 'toolrun')
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

/** Convert one interaction from its Rust/Tauri DTO into the renderer contract. */
export function mapInteractionRequest(value: unknown): InteractionRequest | null {
	if (!isRecord(value) || !isOneOf(value.kind, INTERACTION_KIND_VALUES)) return null;
	const kind = value.kind as InteractionKind;
	const requestId = readStringField(value, 'request_id');
	const sessionId = value.session_id;
	const status = value.status;
	const createdAt = readStringField(value, 'created_at');
	const options = value.options === undefined ? [] : value.options;
	const expiresAt = value.expires_at;
	const toolIndex = value.tool_index;
	if (
		requestId === null ||
		!isCanonicalEntityId(requestId, kind === 'ask' ? 'step' : 'conf') ||
		(sessionId !== undefined && !isCanonicalEntityId(sessionId, 'ses')) ||
		!isOneOf(status, INTERACTION_STATUS_VALUES) ||
		createdAt === null ||
		!isStringArray(options) ||
		!optionalStringFieldIsValid(value, 'tool_name') ||
		!optionalOneOfIsValid(value, 'risk_level', RISK_LEVEL_VALUES) ||
		!optionalStringFieldIsValid(value, 'summary') ||
		!optionalStringFieldIsValid(value, 'permission_key') ||
		!optionalCanonicalIdFieldIsValid(value, 'invocation_step_id', 'step') ||
		!optionalStringFieldIsValid(value, 'tool_call_id') ||
		!optionalStringFieldIsValid(value, 'expires_at') ||
		!validPendingPermissionDeadline(kind, status, expiresAt) ||
		(toolIndex !== undefined &&
			(!isFiniteNumber(toolIndex) ||
				!Number.isInteger(toolIndex) ||
				toolIndex < 0 ||
				toolIndex > 4_294_967_295))
	) {
		return null;
	}
	const owner = mapInteractionOwner(value.owner, sessionId);
	if (!owner || !isValidInteractionKindOwnerPair(kind, owner)) return null;
	if (kind === 'confirm') {
		if (
			!isNonEmptyString(value.tool_name) ||
			!isOneOf(value.risk_level, RISK_LEVEL_VALUES) ||
			!isNonEmptyString(value.summary) ||
			toolIndex === undefined ||
			(owner.kind === 'session' && !isCanonicalEntityId(value.invocation_step_id, 'step')) ||
			(owner.kind === 'app_command' && value.invocation_step_id !== undefined)
		)
			return null;
	}
	if (
		kind === 'scheduled_confirm' &&
		(!isNonEmptyString(value.tool_name) ||
			!isNonEmptyString(value.summary) ||
			!isNonEmptyString(value.permission_key))
	)
		return null;

	const wire = value as unknown as GeneratedInteractionRequestedEvent;
	const normalized = {
		requestId,
		owner,
		status,
		options,
		...(wire.tool_name ? { toolName: wire.tool_name } : {}),
		...(wire.risk_level ? { riskLevel: wire.risk_level } : {}),
		...(wire.summary ? { summary: wire.summary } : {}),
		...(wire.permission_key ? { permissionKey: wire.permission_key } : {}),
		...(wire.invocation_step_id ? { invocationStepId: wire.invocation_step_id } : {}),
		...(wire.tool_index != null ? { toolIndex: wire.tool_index } : {}),
		...(wire.tool_call_id ? { toolCallId: wire.tool_call_id } : {}),
		createdAt,
		...(wire.expires_at ? { expiresAt: wire.expires_at } : {}),
	};
	if (owner.kind === 'session') {
		return { ...normalized, kind, sessionId: owner.sessionId } as InteractionRequest;
	}
	if (owner.kind === 'app_command') {
		return { ...normalized, kind } as InteractionRequest;
	}
	return {
		...normalized,
		kind,
		...(sessionId === undefined ? {} : { sessionId }),
	} as InteractionRequest;
}

/** Convert one known app-shell event from an untrusted Rust/Tauri payload. */
export function mapAppEvent<K extends AppEventName>(
	event: TauriEvent<unknown> & { event: K },
): TauriEvent<AppEventPayloadMap[K]> | null;
export function mapAppEvent(event: unknown): TauriEvent<AppEventPayloadMap[AppEventName]> | null;
export function mapAppEvent(event: unknown): TauriEvent<AppEventPayloadMap[AppEventName]> | null {
	if (
		!isRecord(event) ||
		!isString(event.event) ||
		!APP_EVENT_NAME_SET.has(event.event) ||
		!isValidTauriEventId(event.id) ||
		!matchesGeneratedEventPayload(event.event, event.payload)
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
			if (!isOneOf(p.status, BOOTSTRAP_STATUS_VALUES)) return null;
			return {
				...tauriEvent,
				payload: p as unknown as AppEventPayloadMap['app:bootstrap'],
			};
		case 'tray:status_changed':
			if (!isOneOf(p.status, TRAY_STATUS_EVENT_VALUE_VALUES) || !isString(p.tooltip))
				return null;
			return {
				...tauriEvent,
				payload: p as unknown as AppEventPayloadMap['tray:status_changed'],
			};
		case 'mute:changed':
			if (!isBoolean(p.muted)) return null;
			return { ...tauriEvent, payload: p as unknown as AppEventPayloadMap['mute:changed'] };
		case 'mcp:status_change':
			if (!isString(p.name) || !isMcpClientStatus(p.status)) return null;
			return { ...tauriEvent, payload: { name: p.name, status: p.status } };
		case 'skills:status_change':
			if (!isOneOf(p.op, SKILLS_STATUS_OPERATION_VALUES)) return null;
			return {
				...tauriEvent,
				payload: p as unknown as AppEventPayloadMap['skills:status_change'],
			};
		case 'interaction:requested': {
			const payload = mapInteractionRequest(p);
			return payload ? { ...tauriEvent, payload } : null;
		}
		case 'hotkey:conflict': {
			const binding = readStringField(p, 'binding');
			const error = readStringField(p, 'error');
			if (binding === null || error === null) return null;
			return { ...tauriEvent, payload: { binding, error } };
		}
		case 'hotkey:rebind': {
			const oldBinding = readStringField(p, 'old_binding');
			const newBinding = readStringField(p, 'new_binding');
			if (oldBinding === null || newBinding === null) return null;
			return { ...tauriEvent, payload: { oldBinding, newBinding } };
		}
		case 'llm:config_changed':
			return { ...tauriEvent, payload: null };
	}
	return null;
}
