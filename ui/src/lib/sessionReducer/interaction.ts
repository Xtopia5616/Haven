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

function normalizeInteraction(raw: unknown): InteractionRequest | null {
	if (!raw || typeof raw !== 'object') return null;
	const value = raw as Record<string, unknown>;
	const id = typeof value.id === 'string' ? value.id : '';
	const sessionValue = value.sessionId ?? value.session_id;
	const sessionId = typeof sessionValue === 'string' ? sessionValue : '';
	if (!id || !sessionId) return null;
	const riskValue = value.riskLevel ?? value.risk_level;
	return {
		id,
		sessionId,
		kind: value.kind as InteractionRequest['kind'],
		status: value.status as InteractionRequest['status'],
		prompt: typeof value.prompt === 'string' ? value.prompt : '',
		options: Array.isArray(value.options) ? value.options.map(String) : [],
		...((value.toolName ?? value.tool_name)
			? { toolName: String(value.toolName ?? value.tool_name) }
			: {}),
		...(typeof riskValue === 'string'
			? { riskLevel: riskValue as InteractionRequest['riskLevel'] }
			: {}),
		...(value.summary != null ? { summary: String(value.summary) } : {}),
		...((value.permissionKey ?? value.permission_key)
			? { permissionKey: String(value.permissionKey ?? value.permission_key) }
			: {}),
		...((value.invocationStepId ?? value.invocation_step_id)
			? { invocationStepId: String(value.invocationStepId ?? value.invocation_step_id) }
			: {}),
		...((value.actionIndex ?? value.action_index) != null
			? { actionIndex: Number(value.actionIndex ?? value.action_index) }
			: {}),
		...((value.toolCallId ?? value.tool_call_id)
			? { toolCallId: String(value.toolCallId ?? value.tool_call_id) }
			: {}),
		createdAt: String(value.createdAt ?? value.created_at ?? new Date().toISOString()),
		...((value.expiresAt ?? value.expires_at)
			? { expiresAt: String(value.expiresAt ?? value.expires_at) }
			: {}),
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
