import { writable, type Writable } from 'svelte/store';
import type {
	AgentActionPayload,
	AgentChunkPayload,
	AgentObservationPayload,
	AgentStreamResetPayload,
	AgentSupplementPayload,
	AgentWebSearchPayload,
} from './contracts/agent.ts';
import type { InteractionKind, InteractionRequest } from './contracts/app.ts';
import type { ActionPayload } from './contracts/action.ts';
import { mergeLiveStreaming } from './resumeMessages.ts';
import {
	actionIdFromObservation,
	accumulateStreamChunk,
	applyThoughtSnap,
	dropStreamedThought,
	finalizeStreamBlocks,
	insertAgentMessage,
	newToolMessage,
	parseActionResultInject,
	resetStreamBlocks,
	webSearchCardContent,
	webSearchId,
	type StreamMessage,
} from './streaming.ts';
import { hasToolPreambleInBlock } from './toolIntent.ts';
import { coalesceTokenTotal, type LlmUsage } from './sessionUsage.ts';
import { isBusyStatus } from './sessionStatus.ts';

export const DRAFT_SESSION_ID = '_draft';
const EVENT_SEQ_HISTORY_LIMIT = 4096;

/** The session summary fields used by the chat shell. */
export interface SessionSummary {
	id: string;
	status: string;
	[key: string]: unknown;
}

export interface SessionError {
	sessionId: string;
	reason: string;
}

/** One renderer message, including the explicit dynamic tool extension points. */
export type SessionMessage = StreamMessage & {
	attachments?: Array<{ media_type: string; data: string; filename?: string }>;
	toolArgs?: unknown;
	outcome?: string | null;
	renderer?: string | null;
	actionId?: string | null;
	resolved?: { answer?: string; ignored?: boolean } | null;
	received?: boolean;
};

export interface SessionTokenStats {
	promptTokens: number;
	completionTokens: number;
	totalTokens: number;
	cachedTokens?: number;
	cacheCreationTokens?: number;
	cacheMissTokens?: number;
	contextTokens?: number;
	cacheExclusive?: boolean;
	cumulativePromptTokens: number;
	cumulativeCompletionTokens: number;
	cumulativeTotalTokens: number;
	cumulativeCachedTokens?: number;
	cumulativeCacheCreationTokens?: number;
	cumulativeCacheMissTokens?: number;
	costUsd: number | null;
	cumulativeCostUsd: number | null;
	contextWindow: number | null;
	model: string | null;
	cacheAccounting?: string;
	restored?: boolean;
	lastUpdated?: number;
}

export interface SessionReplayState {
	eventSeqBySession: Record<string, number[]>;
	chunkSeqByMessage: Record<string, number>;
	blockIdsBySession: Record<string, Record<string, StreamBlockIds>>;
}

export interface SessionOptimisticMessage {
	sessionId: string;
	messageId: string;
	status: 'pending' | 'accepted' | 'rejected';
}

export interface AgentChunkBatchItem {
	kind: 'thought' | 'reasoning';
	msgType?: string;
	payload: AgentChunkPayload;
}

/**
 * One serializable state tree for the conversation. The optional runtime
 * fields keep old list-only reducer fixtures source-compatible; the exported
 * application state is always initialized with every field populated.
 */
export interface SessionReducerState {
	sessions: SessionSummary[];
	activeSessionId: string | null;
	error: SessionError | null;
	messages?: Record<string, SessionMessage[]>;
	interactions?: Record<string, InteractionRequest>;
	tokenStats?: Record<string, SessionTokenStats>;
	llmUsage?: Record<string, LlmUsage[]>;
	replay?: SessionReplayState;
	optimistic?: Record<string, SessionOptimisticMessage>;
}

export interface ResumeUsage {
	prompt_tokens?: number;
	completion_tokens?: number;
	total_tokens?: number;
	cached_tokens?: number;
	cache_creation_tokens?: number;
	cache_miss_tokens?: number;
	context_tokens?: number;
	context_window?: number | null;
	cost_usd?: number | null;
	has_cost?: boolean;
}

export type SessionAction =
	| { type: 'sessions/loaded'; sessions: SessionSummary[]; autoSelect?: boolean }
	| { type: 'sessions/cleared' }
	| {
			type: 'session/created';
			sessionId: string;
			freshStart: boolean;
			adoptedDraft: boolean;
			status?: string;
			title?: string | null;
	  }
	| { type: 'session/selected'; sessionId: string | null }
	| { type: 'session/cleared' }
	| { type: 'session/deleted'; sessionId: string | null }
	| { type: 'session/status-updated'; sessionId: string; status: string; title?: string | null }
	| { type: 'session/error-shown'; sessionId: string; reason: string }
	| { type: 'session/error-cleared'; sessionId?: string | null }
	| { type: 'session/retained-error'; session: SessionSummary }
	| { type: 'session/title-updated'; sessionId: string; title: string }
	| { type: 'session/messages/optimistic-added'; sessionId: string; message: SessionMessage }
	| {
			type: 'session/messages/accepted';
			fromSessionId: string;
			toSessionId: string;
			optimisticId: string;
			persistedId?: string | null;
	  }
	| { type: 'session/messages/rejected'; sessionId: string; messageId: string }
	| { type: 'session/messages/cleared'; sessionId: string }
	| { type: 'session/messages/finalized'; sessionId: string }
	| { type: 'session/messages/adopt-draft'; sessionId: string }
	| {
			type: 'session/messages/resume-loaded';
			sessionId: string;
			messages: SessionMessage[];
			interactions?: InteractionRequest[];
			/** Pending live requests that must survive a possibly stale resume snapshot. */
			preserveInteractionIds?: string[];
			usage?: ResumeUsage | null;
			llmUsage?: LlmUsage[];
			preserveStreamingOnly?: boolean;
			excludeMessageIds?: string[];
	  }
	| { type: 'session/messages/truncated'; sessionId: string; targetStep: number }
	| { type: 'session/memory-cleared'; sessionId: string }
	| { type: 'session/replay-reset'; sessionId: string }
	| { type: 'session/stream-blocks-cleared'; sessionId: string }
	| { type: 'session/background-result'; sessionId?: string; actionId: string; content: string }
	| { type: 'session/interaction-upserted'; request: InteractionRequest }
	| {
			type: 'session/interactions-hydrated';
			sessionId: string;
			requests: InteractionRequest[];
			preserveInteractionIds?: string[];
	  }
	| { type: 'session/interactions-cleared'; sessionId: string; kind?: InteractionKind }
	| { type: 'session/interaction-resolved'; id: string; response?: unknown }
	| {
			type: 'agent/chunk';
			kind: 'thought' | 'reasoning';
			msgType?: string;
			payload: AgentChunkPayload;
	  }
	| { type: 'agent/chunks'; chunks: AgentChunkBatchItem[] }
	| {
			type: 'agent/thought';
			payload: {
				sessionId: string;
				thought: string;
				stepNumber: number;
				runId: number;
				messageId: string;
			};
	  }
	| { type: 'agent/stream-reset'; payload: AgentStreamResetPayload }
	| { type: 'agent/web-search'; payload: AgentWebSearchPayload }
	| { type: 'agent/supplement'; payload: AgentSupplementPayload }
	| { type: 'agent/action'; payload: AgentActionPayload }
	| { type: 'agent/observation'; payload: AgentObservationPayload }
	| {
			type: 'session/usage-restored';
			sessionId: string;
			usage: ResumeUsage | null | undefined;
			llmUsage?: LlmUsage[];
	  }
	| { type: 'session/usage-live'; sessionId: string; stats?: SessionTokenStats; call?: LlmUsage }
	| { type: 'session/usage-cleared'; sessionId: string };

export interface StreamBlockIds {
	thoughtId?: string;
	reasoningId?: string;
}

export const initialSessionState: SessionReducerState = {
	sessions: [],
	activeSessionId: null,
	error: null,
	messages: {},
	interactions: {},
	tokenStats: {},
	llmUsage: {},
	replay: { eventSeqBySession: {}, chunkSeqByMessage: {}, blockIdsBySession: {} },
	optimistic: {},
};

function cloneSession(session: SessionSummary): SessionSummary {
	return { ...session };
}

function messagesOf(state: SessionReducerState, sessionId: string): SessionMessage[] {
	return state.messages?.[sessionId] || [];
}

function withMessages(
	state: SessionReducerState,
	sessionId: string,
	fn: (messages: SessionMessage[]) => SessionMessage[],
): SessionReducerState {
	const current = messagesOf(state, sessionId);
	const next = fn(current);
	if (next === current) return state;
	return { ...state, messages: { ...(state.messages || {}), [sessionId]: next } };
}

function replayOf(state: SessionReducerState): SessionReplayState {
	return (
		state.replay || {
			eventSeqBySession: {},
			chunkSeqByMessage: {},
			blockIdsBySession: {},
		}
	);
}

function streamBlockKey(stepNumber: number, runId: number): string {
	return `${stepNumber}:${runId}`;
}

function blockIdsOf(
	state: SessionReducerState,
	sessionId: string,
	stepNumber: number,
	runId: number,
): StreamBlockIds {
	return replayOf(state).blockIdsBySession[sessionId]?.[streamBlockKey(stepNumber, runId)] || {};
}

function registerBlock(
	state: SessionReducerState,
	sessionId: string,
	stepNumber: number,
	runId: number,
	kind: 'thought' | 'reasoning',
	messageId: string,
): SessionReducerState {
	if (!sessionId || !messageId) return state;
	const replay = replayOf(state);
	const sessionBlocks = replay.blockIdsBySession[sessionId] || {};
	const key = streamBlockKey(stepNumber, runId);
	const entry = sessionBlocks[key] || {};
	const field = kind === 'thought' ? 'thoughtId' : 'reasoningId';
	if (entry[field] === messageId) return state;
	return {
		...state,
		replay: {
			...replay,
			blockIdsBySession: {
				...replay.blockIdsBySession,
				[sessionId]: { ...sessionBlocks, [key]: { ...entry, [field]: messageId } },
			},
		},
	};
}

/** Return null for a duplicate event, otherwise advance the session replay set. */
function acceptEventSequence(
	state: SessionReducerState,
	sessionId: string,
	eventSeq: number | undefined,
): SessionReducerState | null {
	if (eventSeq == null || !Number.isFinite(eventSeq)) return state;
	const replay = replayOf(state);
	const previous = replay.eventSeqBySession[sessionId] || [];
	if (previous.includes(eventSeq)) return null;
	const seen = [...previous, eventSeq];
	if (seen.length > EVENT_SEQ_HISTORY_LIMIT)
		seen.splice(0, seen.length - EVENT_SEQ_HISTORY_LIMIT);
	return {
		...state,
		replay: {
			...replay,
			eventSeqBySession: { ...replay.eventSeqBySession, [sessionId]: seen },
		},
	};
}

function clearReplayForSession(
	state: SessionReducerState,
	sessionId: string,
	preserveEventSequence = false,
): SessionReducerState {
	const replay = replayOf(state);
	const eventSeqBySession = { ...replay.eventSeqBySession };
	const blockIdsBySession = { ...replay.blockIdsBySession };
	const chunkSeqByMessage = { ...replay.chunkSeqByMessage };
	if (!preserveEventSequence) delete eventSeqBySession[sessionId];
	for (const ids of Object.values(blockIdsBySession[sessionId] || {})) {
		if (ids.thoughtId) delete chunkSeqByMessage[ids.thoughtId];
		if (ids.reasoningId) delete chunkSeqByMessage[ids.reasoningId];
	}
	delete blockIdsBySession[sessionId];
	for (const message of messagesOf(state, sessionId)) delete chunkSeqByMessage[message.id];
	return {
		...state,
		replay: { ...replay, eventSeqBySession, blockIdsBySession, chunkSeqByMessage },
	};
}

/**
 * Apply one frame of stream deltas with one state/message-list copy per
 * session. The single-chunk action remains the public compatibility shape;
 * the frame action avoids copying the complete message array for every token
 * while retaining the same sequence, block registration and arrival-order
 * semantics.
 */
function applyAgentChunks(
	state: SessionReducerState,
	chunks: readonly AgentChunkBatchItem[],
): SessionReducerState {
	if (chunks.length === 0) return state;
	const bySession = new Map<string, AgentChunkBatchItem[]>();
	for (const chunk of chunks) {
		const sessionId = chunk.payload.sessionId;
		if (!sessionId) continue;
		let sessionChunks = bySession.get(sessionId);
		if (!sessionChunks) bySession.set(sessionId, (sessionChunks = []));
		sessionChunks.push(chunk);
	}
	if (bySession.size === 0) return state;

	const replay = replayOf(state);
	const chunkSeqByMessage = { ...replay.chunkSeqByMessage };
	const blockIdsBySession = { ...replay.blockIdsBySession };
	const messages = { ...(state.messages || {}) };
	let replayChanged = false;
	let messagesChanged = false;

	for (const [sessionId, sessionChunks] of bySession) {
		let nextMessages = messagesOf(state, sessionId);
		const sessionBlocks = { ...(blockIdsBySession[sessionId] || {}) };
		let sessionChanged = false;
		for (const chunk of sessionChunks) {
			const payload = chunk.payload;
			const previous = chunkSeqByMessage[payload.messageId];
			if (previous != null && payload.seq <= previous) continue;
			chunkSeqByMessage[payload.messageId] = Math.max(previous ?? -1, payload.seq);
			replayChanged = true;

			const key = streamBlockKey(payload.stepNumber, payload.runId);
			const entry = sessionBlocks[key] || {};
			const field = chunk.kind === 'thought' ? 'thoughtId' : 'reasoningId';
			const ids =
				entry[field] === payload.messageId
					? entry
					: { ...entry, [field]: payload.messageId };
			if (ids !== entry) {
				sessionBlocks[key] = ids;
				blockIdsBySession[sessionId] = sessionBlocks;
				replayChanged = true;
			}

			if (chunk.kind === 'thought' && ids.reasoningId) {
				const finalized = finalizeStreamBlocks(nextMessages, ids.reasoningId, null);
				if (finalized !== nextMessages) {
					nextMessages = finalized;
					sessionChanged = true;
				}
			}
			const next = accumulateStreamChunk(nextMessages, {
				messageId: payload.messageId,
				delta: payload.delta,
				msgType: chunk.msgType,
				stepNumber: payload.stepNumber,
				runId: payload.runId,
				time: new Date().toLocaleTimeString(),
			});
			if (next !== nextMessages) {
				nextMessages = next;
				sessionChanged = true;
			}
		}
		if (sessionChanged) {
			messages[sessionId] = nextMessages;
			messagesChanged = true;
		}
	}

	if (!replayChanged && !messagesChanged) return state;
	return {
		...state,
		...(messagesChanged ? { messages } : {}),
		...(replayChanged
			? {
					replay: { ...replay, chunkSeqByMessage, blockIdsBySession },
				}
			: {}),
	};
}

function moveMessages(
	state: SessionReducerState,
	fromSessionId: string,
	toSessionId: string,
	messageIds?: Set<string>,
): SessionReducerState {
	if (!fromSessionId || !toSessionId || fromSessionId === toSessionId) return state;
	const source = messagesOf(state, fromSessionId);
	const moving = source.filter((message) => !messageIds || messageIds.has(message.id));
	if (moving.length === 0) return state;
	const remaining = source.filter((message) => !moving.includes(message));
	const prepared = moving.map((message) =>
		message.role === 'user' ? { ...message, received: true, steering: false } : message,
	);
	return {
		...state,
		messages: {
			...(state.messages || {}),
			[fromSessionId]: remaining,
			[toSessionId]: [...prepared, ...messagesOf(state, toSessionId)],
		},
	};
}

function restoreTokenStats(usage: ResumeUsage): SessionTokenStats {
	const prompt = usage.prompt_tokens || 0;
	const completion = usage.completion_tokens || 0;
	const cached = usage.cached_tokens || 0;
	const creation = usage.cache_creation_tokens || 0;
	return {
		promptTokens: 0,
		completionTokens: 0,
		totalTokens: 0,
		cachedTokens: 0,
		cacheCreationTokens: 0,
		cacheMissTokens: 0,
		contextTokens: usage.context_tokens || 0,
		cacheExclusive: false,
		cumulativePromptTokens: prompt,
		cumulativeCompletionTokens: completion,
		cumulativeTotalTokens: coalesceTokenTotal(
			prompt,
			completion,
			usage.total_tokens || 0,
			cached,
			creation,
		),
		cumulativeCachedTokens: cached,
		cumulativeCacheCreationTokens: creation,
		cumulativeCacheMissTokens: usage.cache_miss_tokens || 0,
		costUsd: null,
		cumulativeCostUsd: usage.has_cost && usage.cost_usd != null ? usage.cost_usd : null,
		contextWindow: usage.context_window ?? null,
		model: null,
		restored: true,
		lastUpdated: Date.now(),
	};
}

function applySupplement(
	state: SessionReducerState,
	payload: AgentSupplementPayload,
): SessionReducerState {
	const context = (payload.additionalContext || '').trim();
	if (!payload.sessionId || !context) return state;
	const supplementId =
		payload.supplementId || `supplement-${payload.stepNumber}-${payload.runId}`;
	if (payload.injectSource === 'cross_session') {
		const cardId = `peer-mail-${supplementId}`;
		const content = JSON.stringify({ operation: 'inbox', auto: true, text: context });
		return withMessages(state, payload.sessionId, (messages) => {
			if (messages.some((message) => message.id === cardId)) return messages;
			return insertAgentMessage(
				messages,
				newToolMessage({
					id: cardId,
					stepNumber: payload.stepNumber,
					toolName: 'agent.inbox',
					content,
					time: new Date().toLocaleTimeString(),
				}),
			);
		});
	}
	if (payload.injectSource === 'action_result') {
		const parsed = parseActionResultInject(context);
		const actionId = parsed?.action_id || 'unknown';
		const cardId = `action-result-${supplementId}-${actionId}`;
		const content = JSON.stringify({
			...(parsed || { action_id: actionId, status: 'completed', auto: true }),
			operation: 'actions_result_injected',
		});
		return withMessages(state, payload.sessionId, (messages) => {
			if (messages.some((message) => message.id === cardId)) return messages;
			return insertAgentMessage(
				messages,
				newToolMessage({
					id: cardId,
					stepNumber: payload.stepNumber,
					toolName: 'actions.inspect',
					content,
					time: new Date().toLocaleTimeString(),
				}),
			);
		});
	}
	if (!payload.messageId) return state;
	return withMessages(state, payload.sessionId, (messages) => {
		const index = messages.findIndex((message) => message.id === payload.messageId);
		if (index < 0) return messages;
		const next = messages.slice();
		next[index] = { ...next[index], received: true, steering: false };
		return next;
	});
}

/** Central session runtime transition function. */
export function reduceSession(
	inputState: SessionReducerState,
	action: SessionAction,
): SessionReducerState {
	let state = inputState;
	switch (action.type) {
		case 'sessions/loaded': {
			const sessions = action.sessions.map(cloneSession);
			const activeError =
				state.error && state.error.sessionId === state.activeSessionId
					? state.sessions.find((session) => session.id === state.activeSessionId)
					: null;
			if (activeError && !sessions.some((session) => session.id === activeError.id)) {
				sessions.push({ ...activeError, status: 'error' });
			}
			if (action.autoSelect && !state.activeSessionId) {
				const firstActive = sessions.find(
					(session) =>
						(isBusyStatus(session.status) || session.status === 'paused') &&
						messagesOf(state, session.id).length > 0,
				);
				return { ...state, sessions, activeSessionId: firstActive?.id || null };
			}
			return { ...state, sessions };
		}

		case 'sessions/cleared': {
			const draft = messagesOf(state, DRAFT_SESSION_ID);
			return {
				...state,
				sessions: [],
				activeSessionId: null,
				error: null,
				messages: draft.length ? { [DRAFT_SESSION_ID]: draft } : {},
				interactions: {},
				tokenStats: {},
				llmUsage: {},
				replay: { eventSeqBySession: {}, chunkSeqByMessage: {}, blockIdsBySession: {} },
				optimistic: {},
			};
		}

		case 'session/created': {
			const sessions = state.sessions.some((session) => session.id === action.sessionId)
				? state.sessions.map((session) =>
						session.id === action.sessionId
							? {
									...session,
									...(action.status ? { status: action.status } : {}),
									...(action.title != null ? { title: action.title } : {}),
								}
							: session,
					)
				: [
						...state.sessions,
						{
							id: action.sessionId,
							status: action.status || 'pending',
							title: action.title || null,
						},
					];
			return action.freshStart && !action.adoptedDraft
				? { ...state, sessions }
				: { ...state, sessions, activeSessionId: action.sessionId, error: null };
		}

		case 'session/selected':
			return {
				...state,
				activeSessionId: action.sessionId,
				error:
					state.error && state.error.sessionId !== action.sessionId ? null : state.error,
			};

		case 'session/cleared':
			return { ...state, activeSessionId: null, error: null };

		case 'session/deleted': {
			if (!action.sessionId) return reduceSession(state, { type: 'sessions/cleared' });
			const cleared = reduceSession(state, {
				type: 'session/messages/cleared',
				sessionId: action.sessionId,
			});
			return {
				...cleared,
				sessions: cleared.sessions.filter((session) => session.id !== action.sessionId),
				activeSessionId:
					cleared.activeSessionId === action.sessionId ? null : cleared.activeSessionId,
				error: cleared.error?.sessionId === action.sessionId ? null : cleared.error,
			};
		}

		case 'session/status-updated': {
			const sessions = state.sessions.map((session) =>
				session.id === action.sessionId
					? {
							...session,
							status: action.status,
							...(action.title != null ? { title: action.title } : {}),
						}
					: session,
			);
			return state.error?.sessionId === action.sessionId && isBusyStatus(action.status)
				? { ...state, sessions, error: null }
				: { ...state, sessions };
		}

		case 'session/error-shown': {
			const sessions = state.sessions.map((session) =>
				session.id === action.sessionId ? { ...session, status: 'error' } : session,
			);
			return state.activeSessionId === action.sessionId
				? {
						...state,
						sessions,
						error: { sessionId: action.sessionId, reason: action.reason },
					}
				: { ...state, sessions };
		}

		case 'session/error-cleared':
			return !action.sessionId || state.error?.sessionId === action.sessionId
				? { ...state, error: null }
				: state;

		case 'session/retained-error': {
			const sessions = state.sessions.some((session) => session.id === action.session.id)
				? state.sessions.map((session) =>
						session.id === action.session.id
							? { ...session, ...action.session, status: 'error' }
							: session,
					)
				: [...state.sessions, { ...action.session, status: 'error' }];
			return { ...state, sessions };
		}

		case 'session/title-updated':
			return {
				...state,
				sessions: state.sessions.map((session) =>
					session.id === action.sessionId ? { ...session, title: action.title } : session,
				),
			};

		case 'session/messages/optimistic-added': {
			const next = withMessages(state, action.sessionId, (messages) => [
				...messages.filter((message) => !message.id.startsWith('placeholder-')),
				action.message,
			]);
			return {
				...next,
				optimistic: {
					...(next.optimistic || {}),
					[action.message.id]: {
						sessionId: action.sessionId,
						messageId: action.message.id,
						status: 'pending',
					},
				},
			};
		}

		case 'session/messages/accepted': {
			let next = moveMessages(
				state,
				action.fromSessionId,
				action.toSessionId,
				new Set([action.optimisticId]),
			);
			if (action.persistedId && action.persistedId !== action.optimisticId) {
				next = withMessages(next, action.toSessionId, (messages) => {
					const index = messages.findIndex(
						(message) => message.id === action.optimisticId,
					);
					if (index < 0) return messages;
					const result = messages.slice();
					result[index] = { ...result[index], id: action.persistedId as string };
					return result;
				});
			}
			const optimistic = { ...(next.optimistic || {}) };
			optimistic[action.optimisticId] = {
				sessionId: action.toSessionId,
				messageId: action.persistedId || action.optimisticId,
				status: 'accepted',
			};
			return { ...next, optimistic };
		}

		case 'session/messages/rejected': {
			const optimisticEntry = state.optimistic?.[action.messageId];
			let next = withMessages(state, action.sessionId, (messages) =>
				messages.filter((message) => message.id !== action.messageId),
			);
			if (optimisticEntry && optimisticEntry.sessionId !== action.sessionId) {
				next = withMessages(next, optimisticEntry.sessionId, (messages) =>
					messages.filter((message) => message.id !== action.messageId),
				);
			}
			const optimistic = { ...(next.optimistic || {}) };
			if (optimistic[action.messageId])
				optimistic[action.messageId] = {
					...optimistic[action.messageId],
					status: 'rejected',
				};
			return { ...next, optimistic };
		}

		case 'session/messages/cleared': {
			const next = withMessages(state, action.sessionId, () => []);
			const cleared: SessionReducerState = {
				...next,
				tokenStats: Object.fromEntries(
					Object.entries(next.tokenStats || {}).filter(([id]) => id !== action.sessionId),
				),
				llmUsage: Object.fromEntries(
					Object.entries(next.llmUsage || {}).filter(([id]) => id !== action.sessionId),
				),
				interactions: Object.fromEntries(
					Object.entries(next.interactions || {}).filter(
						([, request]) => request.sessionId !== action.sessionId,
					),
				),
			};
			return clearReplayForSession(cleared, action.sessionId);
		}

		case 'session/messages/finalized':
			return withMessages(state, action.sessionId, (messages) => {
				let changed = false;
				const next = messages.map((message) => {
					if (!message.streaming) return message;
					changed = true;
					return { ...message, streaming: false };
				});
				return changed ? next : messages;
			});

		case 'session/messages/adopt-draft':
			return moveMessages(state, DRAFT_SESSION_ID, action.sessionId);

		case 'session/messages/resume-loaded': {
			const excluded = new Set(action.excludeMessageIds || []);
			const existing = messagesOf(state, action.sessionId).filter(
				(message) =>
					!excluded.has(message.id) &&
					(!action.preserveStreamingOnly || message.streaming),
			);
			let next = withMessages(state, action.sessionId, () =>
				mergeLiveStreaming(action.messages, existing),
			);
			if (action.interactions) {
				next = reduceSession(next, {
					type: 'session/interactions-hydrated',
					sessionId: action.sessionId,
					requests: action.interactions,
					preserveInteractionIds: action.preserveInteractionIds,
				});
			}
			if (action.usage) {
				next = reduceSession(next, {
					type: 'session/usage-restored',
					sessionId: action.sessionId,
					usage: action.usage,
					llmUsage: action.llmUsage,
				});
			} else if (action.llmUsage) {
				next = {
					...next,
					llmUsage: { ...(next.llmUsage || {}), [action.sessionId]: action.llmUsage },
				};
			}
			return next;
		}

		case 'session/messages/truncated': {
			const next = withMessages(state, action.sessionId, (messages) => {
				const index = messages.findIndex(
					(message) =>
						message.stepNumber != null &&
						message.stepNumber >= action.targetStep &&
						message.role !== 'user',
				);
				return index < 0 ? messages : messages.slice(0, index);
			});
			return clearReplayForSession(next, action.sessionId, true);
		}

		case 'session/memory-cleared':
			return reduceSession(state, {
				type: 'session/messages/cleared',
				sessionId: action.sessionId,
			});

		case 'session/replay-reset':
			return clearReplayForSession(state, action.sessionId, true);

		case 'session/stream-blocks-cleared': {
			const replay = replayOf(state);
			const blockIdsBySession = { ...replay.blockIdsBySession };
			const chunkSeqByMessage = { ...replay.chunkSeqByMessage };
			for (const ids of Object.values(blockIdsBySession[action.sessionId] || {})) {
				if (ids.thoughtId) delete chunkSeqByMessage[ids.thoughtId];
				if (ids.reasoningId) delete chunkSeqByMessage[ids.reasoningId];
			}
			delete blockIdsBySession[action.sessionId];
			return { ...state, replay: { ...replay, blockIdsBySession, chunkSeqByMessage } };
		}

		case 'session/background-result': {
			const sessionIds = action.sessionId
				? [action.sessionId]
				: Object.keys(state.messages || {});
			return sessionIds.reduce(
				(next, sessionId) =>
					withMessages(next, sessionId, (messages) => {
						let changed = false;
						const next = messages.map((message) => {
							if (message.actionId !== action.actionId) return message;
							changed = true;
							return {
								...message,
								content: action.content,
								actionId: null,
								streaming: false,
							};
						});
						return changed ? next : messages;
					}),
				state,
			);
		}

		case 'session/interaction-upserted': {
			const request = action.request;
			if (!request.id || !request.sessionId) return state;
			const previous = state.interactions?.[request.id];
			if (previous && JSON.stringify(previous) === JSON.stringify(request)) return state;
			return {
				...state,
				interactions: { ...(state.interactions || {}), [request.id]: request },
			};
		}

		case 'session/interactions-hydrated': {
			const interactions = Object.fromEntries(
				Object.entries(state.interactions || {}).filter(
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
			for (const [id, request] of Object.entries(state.interactions || {})) {
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
					Object.entries(state.interactions || {}).filter(
						([, request]) =>
							request.sessionId !== action.sessionId ||
							(!!action.kind && request.kind !== action.kind),
					),
				),
			};

		case 'session/interaction-resolved': {
			const request = state.interactions?.[action.id];
			if (!request || request.status !== 'pending') return state;
			return {
				...state,
				interactions: {
					...(state.interactions || {}),
					[action.id]: {
						...request,
						status: 'resolved',
						...(action.response === undefined ? {} : { response: action.response }),
					},
				},
			};
		}

		case 'agent/chunk': {
			return applyAgentChunks(state, [action]);
		}

		case 'agent/chunks': {
			return applyAgentChunks(state, action.chunks);
		}

		case 'agent/thought': {
			const payload = action.payload;
			const registered = registerBlock(
				state,
				payload.sessionId,
				payload.stepNumber,
				payload.runId,
				'thought',
				payload.messageId,
			);
			const ids = blockIdsOf(
				registered,
				payload.sessionId,
				payload.stepNumber,
				payload.runId,
			);
			return withMessages(registered, payload.sessionId, (messages) =>
				applyThoughtSnap(messages, {
					messageId: payload.messageId,
					reasoningId: ids.reasoningId,
					thought: payload.thought,
					stepNumber: payload.stepNumber,
					runId: payload.runId,
					time: new Date().toLocaleTimeString(),
				}),
			);
		}

		case 'agent/stream-reset': {
			const payload = action.payload;
			const next = withMessages(state, payload.sessionId, (messages) =>
				resetStreamBlocks(messages, payload.reasoningMessageId, payload.thoughtMessageId),
			);
			const replay = replayOf(next);
			const chunkSeqByMessage = { ...replay.chunkSeqByMessage };
			delete chunkSeqByMessage[payload.reasoningMessageId];
			delete chunkSeqByMessage[payload.thoughtMessageId];
			return { ...next, replay: { ...replay, chunkSeqByMessage } };
		}

		case 'agent/web-search': {
			const payload = action.payload;
			if (!payload.sessionId || !payload.callId) return state;
			const searchId = webSearchId(
				payload.sessionId,
				payload.stepNumber,
				payload.runId,
				payload.callId,
			);
			const ids = blockIdsOf(state, payload.sessionId, payload.stepNumber, payload.runId);
			return withMessages(state, payload.sessionId, (messages) => {
				let next = messages;
				const existing = next.find((message) => message.id === searchId);
				const content = webSearchCardContent(payload, existing?.content);
				if (!existing) next = finalizeStreamBlocks(next, ids.reasoningId, ids.thoughtId);
				if (payload.phase === 'completed') {
					if (!existing)
						return insertAgentMessage(
							next,
							newToolMessage({
								id: searchId,
								stepNumber: payload.stepNumber,
								toolName: 'web_search',
								time: new Date().toLocaleTimeString(),
								content,
								streaming: false,
							}),
						);
					return next.map((message) =>
						message.id === searchId
							? { ...message, content, streaming: false }
							: message,
					);
				}
				if (existing)
					return next.map((message) =>
						message.id === searchId
							? { ...message, content, streaming: true }
							: message,
					);
				return insertAgentMessage(
					next,
					newToolMessage({
						id: searchId,
						stepNumber: payload.stepNumber,
						toolName: 'web_search',
						time: new Date().toLocaleTimeString(),
						content,
						streaming: true,
					}),
				);
			});
		}

		case 'agent/supplement': {
			const accepted = acceptEventSequence(
				state,
				action.payload.sessionId,
				action.payload.eventSeq,
			);
			return accepted ? applySupplement(accepted, action.payload) : state;
		}

		case 'agent/action': {
			const payload = action.payload;
			const accepted = acceptEventSequence(state, payload.sessionId, payload.eventSeq);
			if (!accepted) return state;
			const ids = blockIdsOf(accepted, payload.sessionId, payload.stepNumber, payload.runId);
			return withMessages(accepted, payload.sessionId, (messages) => {
				const fixed = finalizeStreamBlocks(
					payload.suppressStreamedThought
						? dropStreamedThought(messages, ids.thoughtId)
						: messages,
					ids.reasoningId,
					ids.thoughtId,
				);
				if (fixed.some((message) => message.id === payload.stepId)) return fixed;
				return insertAgentMessage(
					fixed,
					newToolMessage({
						id: payload.stepId,
						stepNumber: payload.stepNumber,
						toolName: payload.toolName,
						time: new Date().toLocaleTimeString(),
						streaming: true,
						toolArgs: payload.input,
						showFallbackIntent: !hasToolPreambleInBlock(fixed, ids.thoughtId),
					}),
				);
			});
		}

		case 'agent/observation': {
			const payload = action.payload;
			const accepted = acceptEventSequence(state, payload.sessionId, payload.eventSeq);
			if (!accepted) return state;
			const ids = blockIdsOf(accepted, payload.sessionId, payload.stepNumber, payload.runId);
			const updated = withMessages(accepted, payload.sessionId, (messages) => {
				const index = messages.findIndex((message) => message.id === payload.stepId);
				const message = newToolMessage({
					id: payload.stepId,
					stepNumber: payload.stepNumber,
					toolName: payload.toolName,
					content: payload.observation,
					askOptions: payload.askOptions || [],
					outcome: payload.outcome,
					renderer: payload.renderer,
					result: payload.result,
					actionId: actionIdFromObservation(payload.observation),
					showFallbackIntent: !hasToolPreambleInBlock(messages, ids.thoughtId),
				});
				if (index < 0) return insertAgentMessage(messages, message);
				const next = messages.slice();
				next[index] = { ...next[index], ...message, streaming: false };
				return next;
			});
			return updated;
		}

		case 'session/usage-restored':
			if (!action.usage)
				return action.llmUsage
					? {
							...state,
							llmUsage: {
								...(state.llmUsage || {}),
								[action.sessionId]: action.llmUsage,
							},
						}
					: state;
			return {
				...state,
				tokenStats: {
					...(state.tokenStats || {}),
					[action.sessionId]: restoreTokenStats(action.usage),
				},
				llmUsage: action.llmUsage
					? { ...(state.llmUsage || {}), [action.sessionId]: action.llmUsage }
					: state.llmUsage,
			};

		case 'session/usage-live': {
			const tokenStats = action.stats
				? {
						...(state.tokenStats || {}),
						[action.sessionId]: {
							...action.stats,
							restored: false,
							lastUpdated: Date.now(),
						},
					}
				: state.tokenStats;
			const llmUsage = action.call
				? {
						...(state.llmUsage || {}),
						[action.sessionId]: [
							...(state.llmUsage?.[action.sessionId] || []),
							action.call,
						],
					}
				: state.llmUsage;
			return { ...state, tokenStats, llmUsage };
		}

		case 'session/usage-cleared': {
			const tokenStats = { ...(state.tokenStats || {}) };
			const llmUsage = { ...(state.llmUsage || {}) };
			delete tokenStats[action.sessionId];
			delete llmUsage[action.sessionId];
			return { ...state, tokenStats, llmUsage };
		}
	}
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

/** Normalize a terminal background action into the tool-card payload. */
export function backgroundActionResultContent(payload: ActionPayload): string | null {
	if (payload.kind !== 'background' || !payload.id) return null;
	const status = payload.status ?? 'completed';
	const rawOutput = payload.output ?? payload.error ?? '';
	if (typeof rawOutput === 'string' && rawOutput.trim().startsWith('{')) return rawOutput;
	return JSON.stringify({
		output: rawOutput,
		background: true,
		action_id: payload.id,
		status,
		...(payload.exitCode != null ? { exit_code: payload.exitCode } : {}),
		...(payload.error && !payload.output ? { error: payload.error } : {}),
	});
}
