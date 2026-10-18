import type { InteractionRequest } from '../contracts/app.ts';
import type { SessionActionOf, SessionReducerState } from './types.ts';

type InteractionReducerAction = SessionActionOf<
	| 'sessions/cleared'
	| 'session/interaction-upserted'
	| 'session/interactions-hydrated'
	| 'session/interactions-cleared'
	| 'session/asks-settled'
	| 'session/ask-resolved'
	| 'session/interaction-resolution-result'
	| 'session/schedule-run-cancelled'
>;

export function reduceInteraction(
	inputState: SessionReducerState,
	action: InteractionReducerAction,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/cleared': {
			const interactions = Object.fromEntries(
				Object.entries(state.interactions).filter(
					([, request]) => request.owner.kind !== 'session',
				),
			);
			return { ...state, interactions };
		}
		case 'session/interaction-upserted': {
			const request = action.request;
			if (!request.requestId || !hasValidOwnerContext(request)) return state;
			const previous = state.interactions[request.requestId];
			if (previous && JSON.stringify(previous) === JSON.stringify(request)) return state;
			return {
				...state,
				interactions: { ...state.interactions, [request.requestId]: request },
			};
		}
		case 'session/interactions-hydrated': {
			const interactions = Object.fromEntries(
				Object.entries(state.interactions).filter(
					([, request]) => !isSessionInteractionFor(request, action.sessionId),
				),
			);
			for (const request of action.requests)
				if (request.requestId && isSessionInteractionFor(request, action.sessionId))
					interactions[request.requestId] = request;
			// Resume is a snapshot read, not an event acknowledgement. Keep pending
			// requests that are still live in the renderer when the snapshot omitted
			// them (for example, an interaction event raced the SQLite checkpoint).
			// An incoming row always wins, including a resolved/expired row.
			const preserveIds = new Set(action.preserveRequestIds || []);
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
							!isSessionInteractionFor(request, action.sessionId) ||
							request.kind !== 'ask',
					),
				),
			};
		case 'session/ask-resolved': {
			const request = state.interactions[action.requestId];
			if (
				!request ||
				request.kind !== 'ask' ||
				request.owner.kind !== 'session' ||
				request.status !== 'pending'
			)
				return state;
			return {
				...state,
				interactions: {
					...state.interactions,
					[action.requestId]: {
						...request,
						status: 'resolved',
						response: action.response,
					},
				},
			};
		}
		case 'session/interaction-resolution-result': {
			const request = state.interactions[action.requestId];
			if (!request || request.status !== 'pending') return state;
			if (action.result === 'stale') {
				const interactions = { ...state.interactions };
				delete interactions[action.requestId];
				return { ...state, interactions };
			}
			return {
				...state,
				interactions: {
					...state.interactions,
					[action.requestId]: {
						...request,
						status: action.result,
					},
				},
			};
		}
		case 'session/schedule-run-cancelled':
			return {
				...state,
				interactions: Object.fromEntries(
					Object.entries(state.interactions).filter(
						([, request]) =>
							request.status !== 'pending' ||
							request.owner.kind !== 'schedule_run' ||
							request.owner.scheduleRunId !== action.scheduleRunId,
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
		case 'schedule_run':
			return Boolean(request.owner.scheduleRunId);
		case 'app_command':
			return request.sessionId === undefined;
	}
}
