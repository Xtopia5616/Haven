import { writable, type Writable } from 'svelte/store';
import { reduceAgent } from './sessionReducer/agent.ts';
import { reduceInteraction } from './sessionReducer/interaction.ts';
import { reduceLifecycle } from './sessionReducer/lifecycle.ts';
import { reduceTranscript } from './sessionReducer/transcript.ts';
import { reduceUsage } from './sessionReducer/usage.ts';
import { blockIdsOf, messagesOf } from './sessionReducer/state.ts';
import { createEqualityGatedSessionSelectorStore } from './sessionReducer/selectorStore.ts';
import { initialSessionState } from './sessionReducer/types.ts';
import type {
	SessionAction,
	SessionMessage,
	SessionReducerState,
	StreamBlockIds,
} from './sessionReducer/types.ts';

export { DRAFT_SESSION_ID, initialSessionState } from './sessionReducer/types.ts';
export type {
	AgentChunkBatchItem,
	ResumeUsage,
	SessionAction,
	SessionError,
	SessionMessage,
	SessionOptimisticMessage,
	SessionReducerState,
	SessionReplayState,
	SessionSummary,
	SessionTermination,
	SessionTerminationStatus,
	SessionTokenStats,
	StreamBlockIds,
} from './sessionReducer/types.ts';
export { resumeInteractions } from './sessionReducer/interaction.ts';
export { backgroundActionResultContent } from './sessionReducer/transcript.ts';

function clearSessionMessages(state: SessionReducerState, sessionId: string): SessionReducerState {
	const withoutMessages = reduceTranscript(state, {
		type: 'session/messages/cleared',
		sessionId,
	});
	const optimistic = Object.fromEntries(
		Object.entries(withoutMessages.optimistic).filter(
			([, message]) => message.sessionId !== sessionId,
		),
	);
	const withoutOptimistic = { ...withoutMessages, optimistic };
	const withoutInteractions = reduceInteraction(withoutOptimistic, {
		type: 'session/interactions-cleared',
		sessionId,
	});
	return reduceUsage(withoutInteractions, {
		type: 'session/usage-cleared',
		sessionId,
	});
}

function resumeSession(
	state: SessionReducerState,
	action: Extract<SessionAction, { type: 'session/messages/resume-loaded' }>,
): SessionReducerState {
	let next = reduceTranscript(state, action);
	if (action.interactions) {
		next = reduceInteraction(next, {
			type: 'session/interactions-hydrated',
			sessionId: action.sessionId,
			requests: action.interactions,
			preserveInteractionIds: action.preserveInteractionIds,
		});
	}
	if (action.usage || action.llmUsage) {
		next = reduceUsage(next, {
			type: 'session/usage-restored',
			sessionId: action.sessionId,
			usage: action.usage,
			llmUsage: action.llmUsage,
		});
	}
	return next;
}

/** Route the public action union to its private domain transition. */
export function reduceSession(
	inputState: SessionReducerState,
	action: SessionAction,
): SessionReducerState {
	switch (action.type) {
		case 'sessions/loaded':
		case 'session/created':
		case 'session/selected':
		case 'session/cleared':
		case 'session/status-updated':
		case 'session/error-shown':
		case 'session/error-cleared':
		case 'session/error-reason-remembered':
		case 'session/error-reason-forgotten':
		case 'session/termination-shown':
		case 'session/retained-error':
		case 'session/title-updated':
			return reduceLifecycle(inputState, action);

		case 'sessions/cleared': {
			const withoutTranscript = reduceTranscript(inputState, action);
			const withoutInteractions = reduceInteraction(withoutTranscript, action);
			const withoutUsage = reduceUsage(withoutInteractions, action);
			return reduceLifecycle(withoutUsage, action);
		}

		case 'session/deleted':
			if (!action.sessionId) return reduceSession(inputState, { type: 'sessions/cleared' });
			return reduceLifecycle(clearSessionMessages(inputState, action.sessionId), action);

		case 'session/messages/cleared':
		case 'session/memory-cleared':
			return clearSessionMessages(inputState, action.sessionId);

		case 'session/messages/resume-loaded':
			return resumeSession(inputState, action);

		case 'session/messages/optimistic-added':
		case 'session/messages/accepted':
		case 'session/messages/rejected':
		case 'session/messages/finalized':
		case 'session/messages/adopt-draft':
		case 'session/messages/asks-settled':
		case 'session/messages/truncated':
		case 'session/replay-reset':
		case 'session/stream-blocks-cleared':
		case 'session/background-result':
			return reduceTranscript(inputState, action);

		case 'session/interaction-upserted':
		case 'session/interactions-hydrated':
		case 'session/interactions-cleared':
		case 'session/interaction-resolved':
		case 'session/interaction-resolution-result':
		case 'session/scheduled-action-cancelled':
			return reduceInteraction(inputState, action);

		case 'agent/chunks':
		case 'agent/thought':
		case 'agent/stream-reset':
		case 'agent/web-search':
		case 'agent/supplement':
		case 'agent/action':
		case 'agent/observation':
			return reduceAgent(inputState, action);

		case 'session/usage-restored':
		case 'session/usage-live':
		case 'session/usage-cleared':
			return reduceUsage(inputState, action);
	}
	return inputState;
}

type SessionStateListener = (state: SessionReducerState) => void;

/** Observable wrapper used by the route and reducer-focused tests. */
export class SessionReducer {
	private state: SessionReducerState;
	private readonly listeners = new Set<SessionStateListener>();
	private readonly stateStore?: Writable<SessionReducerState>;

	constructor(
		initialState: SessionReducerState = initialSessionState,
		stateStore?: Writable<SessionReducerState>,
	) {
		this.state = initialState;
		this.stateStore = stateStore;
	}

	getState(): SessionReducerState {
		return this.state;
	}

	getMessages(sessionId: string): SessionMessage[] {
		return messagesOf(this.state, sessionId);
	}

	getPendingInteractionIds(sessionId: string): string[] {
		return Object.values(this.state.interactions || {})
			.filter(
				(request) =>
					request.owner.kind === 'session' &&
					request.owner.sessionId === sessionId &&
					request.status === 'pending',
			)
			.map((request) => request.id);
	}

	getSessionErrorReason(sessionId: string): string {
		return this.state.sessionErrorReasons[sessionId] || '';
	}

	getBlockIds(sessionId: string, stepNumber: number, runId: number): StreamBlockIds {
		return blockIdsOf(this.state, sessionId, stepNumber, runId);
	}

	dispatch(action: SessionAction): SessionReducerState {
		const next = reduceSession(this.state, action);
		if (next === this.state) return this.state;
		this.state = next;
		this.stateStore?.set(next);
		for (const listener of this.listeners) listener(this.state);
		return this.state;
	}

	subscribe(listener: SessionStateListener): () => void {
		this.listeners.add(listener);
		listener(this.state);
		return () => this.listeners.delete(listener);
	}
}

/** The one application-wide reducer shared by chat, history and the shell. */
export const sessionStateStore = writable<SessionReducerState>(initialSessionState);
export const appSessionReducer = new SessionReducer(initialSessionState, sessionStateStore);

/** Read a reducer-owned slice without notifying consumers for unrelated updates. */
export function createSessionSelectorStore<T>(
	select: (state: SessionReducerState) => T,
	equals: (previous: T, next: T) => boolean = Object.is,
) {
	return createEqualityGatedSessionSelectorStore(sessionStateStore, select, equals);
}
