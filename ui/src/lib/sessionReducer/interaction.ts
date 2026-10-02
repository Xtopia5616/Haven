import type { InteractionKind, InteractionRequest } from '../contracts/app.ts';
import type { SessionActionOf, SessionReducerState } from './types.ts';

type Action = SessionActionOf<
	| 'sessions/cleared'
	| 'session/interaction-upserted'
	| 'session/interactions-hydrated'
	| 'session/interactions-cleared'
	| 'session/interaction-resolved'
>;

export function reduceInteraction(
	inputState: SessionReducerState,
	action: Action,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/cleared':
			return { ...state, interactions: {} };
		case 'session/interaction-upserted': {
			const request = action.request;
			if (!request.id || !request.sessionId) return state;
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
					([, request]) => request.sessionId !== action.sessionId,
				),
			);
			for (const request of action.requests)
				if (request.id && request.sessionId === action.sessionId)
					interactions[request.id] = request;
			// Resume is a snapshot read, not an event acknowledgement. Keep pending
			// requests that are still live in the renderer when the snapshot omitted
			// them (for example, an interaction event raced the SQLite checkpoint).
			// An incoming row always wins, including a resolved/expired row.
			const preserveIds = new Set(action.preserveInteractionIds || []);
			for (const [id, request] of Object.entries(state.interactions)) {
				if (
					preserveIds.has(id) &&
					request.sessionId === action.sessionId &&
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
							request.sessionId !== action.sessionId ||
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
	}
	return inputState;
}

const INTERACTION_KINDS = ['ask', 'confirm', 'scheduled_confirm'] as const;
const INTERACTION_STATUSES = ['pending', 'resolved', 'expired', 'cancelled'] as const;
const RISK_LEVELS = ['safe', 'low', 'medium', 'high', 'critical'] as const;

function normalizeInteraction(raw: unknown): InteractionRequest | null {
	if (!raw || typeof raw !== 'object') return null;
	const value = raw as Record<string, unknown>;
	const id = typeof value.id === 'string' ? value.id : '';
	const sessionId = typeof value.session_id === 'string' ? value.session_id : '';
	const kind = value.kind;
	const status = value.status;
	const options = value.options;
	const createdAt = value.created_at;
	if (
		!id ||
		!sessionId ||
		typeof kind !== 'string' ||
		!INTERACTION_KINDS.includes(kind as (typeof INTERACTION_KINDS)[number]) ||
		typeof status !== 'string' ||
		!INTERACTION_STATUSES.includes(status as (typeof INTERACTION_STATUSES)[number]) ||
		!Array.isArray(options) ||
		!options.every((option) => typeof option === 'string') ||
		typeof createdAt !== 'string'
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
	return {
		id,
		sessionId,
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
