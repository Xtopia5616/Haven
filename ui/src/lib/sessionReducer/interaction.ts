import { mapInteractionOwner, type InteractionKind, type InteractionRequest } from '../contracts/app.ts';
import type { SessionActionOf, SessionReducerState } from './types.ts';

type Action = SessionActionOf<
	| 'sessions/cleared'
	| 'session/interaction-upserted'
	| 'session/interactions-hydrated'
	| 'session/interactions-cleared'
	| 'session/interaction-resolved'
	| 'session/interaction-resolution-result'
	| 'session/scheduled-action-cancelled'
>;

export function reduceInteraction(
	inputState: SessionReducerState,
	action: Action,
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
		case 'session/scheduled-action-cancelled':
			return {
				...state,
				interactions: Object.fromEntries(
					Object.entries(state.interactions).filter(
						([, request]) =>
							request.status !== 'pending' ||
							request.owner.kind !== 'scheduled_action' ||
							request.owner.actionId !== action.actionId,
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
		case 'scheduled_action':
			return Boolean(request.owner.actionId);
		case 'app_command':
			return request.sessionId === undefined;
	}
}

const INTERACTION_KINDS = ['ask', 'confirm', 'scheduled_confirm'] as const;
const INTERACTION_STATUSES = ['pending', 'resolved', 'expired', 'cancelled'] as const;
const RISK_LEVELS = ['safe', 'low', 'medium', 'high', 'critical'] as const;

function normalizeInteraction(raw: unknown): InteractionRequest | null {
	if (!raw || typeof raw !== 'object') return null;
	const value = raw as Record<string, unknown>;
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
		!INTERACTION_KINDS.includes(kind as (typeof INTERACTION_KINDS)[number]) ||
		typeof status !== 'string' ||
		!INTERACTION_STATUSES.includes(status as (typeof INTERACTION_STATUSES)[number]) ||
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
	const actionIndex = value.action_index;
	const toolCallId = value.tool_call_id;
	const expiresAt = value.expires_at;
	const normalized = {
		id,
		kind: kind as InteractionRequest['kind'],
		status: status as InteractionRequest['status'],
		options,
		...(typeof toolName === 'string' ? { toolName } : {}),
		...(typeof riskLevel === 'string' &&
		RISK_LEVELS.includes(riskLevel as (typeof RISK_LEVELS)[number])
			? { riskLevel: riskLevel as InteractionRequest['riskLevel'] }
			: {}),
		...(typeof summary === 'string' ? { summary } : {}),
		...(typeof permissionKey === 'string' ? { permissionKey } : {}),
		...(typeof invocationStepId === 'string' ? { invocationStepId } : {}),
		...(typeof actionIndex === 'number' && Number.isFinite(actionIndex) ? { actionIndex } : {}),
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
	if (!result || typeof result !== 'object') return [];
	const raw = (result as { interactions?: unknown }).interactions;
	if (!Array.isArray(raw)) return [];
	return raw
		.map(normalizeInteraction)
		.filter((request): request is InteractionRequest => request !== null);
}
