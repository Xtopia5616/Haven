import { mapInteractionOwner, type InteractionRequest } from '../contracts/app.ts';
import { isRecord } from '../contracts/objectGuards.ts';
import type { InteractionKind } from '../contracts/generatedCommands.ts';
import {
	INTERACTION_KIND_VALUES,
	INTERACTION_STATUS_VALUES,
	RISK_LEVEL_VALUES,
} from '../contracts/generatedCommands.ts';
import type { SessionActionOf, SessionReducerState } from './types.ts';

type InteractionReducerAction = SessionActionOf<
	| 'sessions/cleared'
	| 'session/interaction-upserted'
	| 'session/interactions-hydrated'
	| 'session/interactions-cleared'
	| 'session/asks-settled'
	| 'session/interaction-resolved'
	| 'session/interaction-resolution-result'
	| 'session/scheduled-tool-run-cancelled'
>;

export function reduceInteraction(
	inputState: SessionReducerState,
	action: InteractionReducerAction,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/cleared': {
			const interactions = Object.fromEntries(
				Object.entries(state.interactions).filter(([, request]) => request.owner.kind !== 'session'),
			);
			return { ...state, interactions };
		}
		case 'session/interaction-upserted': {
			const request = action.request;
			if (!request.id || !hasValidOwnerContext(request)) return state;
			const previous = state.interactions[request.id];
			if (previous && JSON.stringify(previous) === JSON.stringify(request)) return state;
			return {
				...state,
				interactions: { ...state.interactions, [request.id]: request },
			};
		}
		case 'session/interactions-hydrated': {
			const interactions = Object.fromEntries(
				Object.entries(state.interactions).filter(
					([, request]) => !isSessionInteractionFor(request, action.sessionId),
				),
			);
			for (const request of action.requests)
				if (request.id && isSessionInteractionFor(request, action.sessionId))
					interactions[request.id] = request;
			// Resume is a snapshot read, not an event acknowledgement. Keep pending
			// requests that are still live in the renderer when the snapshot omitted
			// them (for example, an interaction event raced the SQLite checkpoint).
			// An incoming row always wins, including a resolved/expired row.
			const preserveIds = new Set(action.preserveInteractionIds || []);
			for (const [id, request] of Object.entries(state.interactions)) {
				if (
					preserveIds.has(id) &&
					isSessionInteractionFor(request, action.sessionId) &&
					request.status === 'pending' &&
					!(id in interactions)
				)
					interactions[id] = request;
			}
			return { ...state, interactions };
		}
		case 'session/interactions-cleared':
			return {
				...state,
				interactions: Object.fromEntries(
					Object.entries(state.interactions).filter(
						([, request]) =>
							!isSessionInteractionFor(request, action.sessionId) ||
							(!!action.kind && request.kind !== action.kind),
					),
				),
			};
		case 'session/asks-settled':
			return {
				...state,
				interactions: Object.fromEntries(
					Object.entries(state.interactions).filter(
						([, request]) =>
							!isSessionInteractionFor(request, action.sessionId) || request.kind !== 'ask',
					),
				),
			};
		case 'session/interaction-resolved': {
			const request = state.interactions[action.id];
			if (!request || request.status !== 'pending') return state;
			return {
				...state,
				interactions: {
					...state.interactions,
					[action.id]: {
						...request,
						status: 'resolved',
						...(action.response === undefined ? {} : { response: action.response }),
					},
				},
			};
		}
		case 'session/interaction-resolution-result': {
			const request = state.interactions[action.id];
			if (!request || request.status !== 'pending') return state;
			if (action.result === 'stale') {
				const interactions = { ...state.interactions };
				delete interactions[action.id];
				return { ...state, interactions };
			}
			return {
				...state,
				interactions: {
					...state.interactions,
					[action.id]: {
						...request,
						status: action.result,
						...(action.response === undefined ? {} : { response: action.response }),
					},
				},
			};
		}
		case 'session/scheduled-tool-run-cancelled':
			return {
				...state,
				interactions: Object.fromEntries(
					Object.entries(state.interactions).filter(
						([, request]) =>
							request.status !== 'pending' ||
							request.owner.kind !== 'scheduled_tool_run' ||
							request.owner.toolRunId !== action.toolRunId,
					),
				),
			};
	}
	return inputState;
}

function isSessionInteractionFor(request: InteractionRequest, sessionId: string): boolean {
	return request.owner.kind === 'session' && request.owner.sessionId === sessionId;
}

function hasValidOwnerContext(request: InteractionRequest): boolean {
	switch (request.owner.kind) {
		case 'session':
			return Boolean(request.sessionId) && request.sessionId === request.owner.sessionId;
		case 'scheduled_tool_run':
			return Boolean(request.owner.toolRunId);
		case 'app_command':
			return request.sessionId === undefined;
	}
}

function normalizeInteraction(raw: unknown): InteractionRequest | null {
	if (!isRecord(raw)) return null;
	const value = raw;
	const id = typeof value.id === 'string' ? value.id : '';
	const sessionId = value.session_id;
	const owner = mapInteractionOwner(value.owner, typeof sessionId === 'string' ? sessionId : undefined);
	const kind = value.kind;
	const status = value.status;
	const options = value.options;
	const createdAt = value.created_at;
	if (
		!id ||
		(sessionId !== undefined && (typeof sessionId !== 'string' || !sessionId)) ||
		!owner ||
		typeof kind !== 'string' ||
		!INTERACTION_KIND_VALUES.includes(kind as (typeof INTERACTION_KIND_VALUES)[number]) ||
		typeof status !== 'string' ||
		!INTERACTION_STATUS_VALUES.includes(status as (typeof INTERACTION_STATUS_VALUES)[number]) ||
		!Array.isArray(options) ||
		!options.every((option) => typeof option === 'string') ||
		typeof createdAt !== 'string'
	)
		return null;
	if (
		status === 'pending' &&
		kind !== 'ask' &&
		(typeof value.expires_at !== 'string' || !Number.isFinite(Date.parse(value.expires_at)))
	)
		return null;
	const toolName = value.tool_name;
	const riskLevel = value.risk_level;
	const summary = value.summary;
	const permissionKey = value.permission_key;
	const invocationStepId = value.invocation_step_id;
	const toolIndex = value.tool_index;
	const toolCallId = value.tool_call_id;
	const expiresAt = value.expires_at;
	const normalized = {
		id,
		kind: kind as InteractionRequest['kind'],
		status: status as InteractionRequest['status'],
		options,
		...(typeof toolName === 'string' ? { toolName } : {}),
		...(typeof riskLevel === 'string' &&
		RISK_LEVEL_VALUES.includes(riskLevel as (typeof RISK_LEVEL_VALUES)[number])
			? { riskLevel: riskLevel as InteractionRequest['riskLevel'] }
			: {}),
		...(typeof summary === 'string' ? { summary } : {}),
		...(typeof permissionKey === 'string' ? { permissionKey } : {}),
		...(typeof invocationStepId === 'string' ? { invocationStepId } : {}),
		...(typeof toolIndex === 'number' && Number.isFinite(toolIndex) ? { toolIndex } : {}),
		...(typeof toolCallId === 'string' ? { toolCallId } : {}),
		createdAt,
		...(typeof expiresAt === 'string' ? { expiresAt } : {}),
	};
	if (owner.kind === 'session') {
		if (sessionId !== owner.sessionId) return null;
		return { ...normalized, sessionId: owner.sessionId, owner };
	}
	if (owner.kind === 'app_command') {
		if (sessionId !== undefined) return null;
		return { ...normalized, owner };
	}
	return {
		...normalized,
		...(sessionId === undefined ? {} : { sessionId: sessionId as string }),
		owner,
	};
}

/** Normalize the renderer-safe resume projection at the reducer boundary. */
export function resumeInteractions(result: unknown): InteractionRequest[] {
	if (!isRecord(result)) return [];
	const raw = result.interactions;
	if (!Array.isArray(raw)) return [];
	return raw
		.map(normalizeInteraction)
		.filter((request): request is InteractionRequest => request !== null);
}
