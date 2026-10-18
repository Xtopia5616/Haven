import type { ToolRunPayload } from '../contracts/toolRun.ts';
import { mergeLiveStreaming } from '../resumeMessages.ts';
import { clearReplayForSession, messagesOf, replayOf, withMessages } from './state.ts';
import {
	DRAFT_SESSION_ID,
	type SessionActionOf,
	type SessionMessage,
	type SessionReducerState,
} from './types.ts';

type TranscriptReducerAction = SessionActionOf<
	| 'sessions/cleared'
	| 'session/messages/optimistic-added'
	| 'session/messages/accepted'
	| 'session/messages/rejected'
	| 'session/messages/cleared'
	| 'session/messages/finalized'
	| 'session/messages/adopt-draft'
	| 'session/asks-settled'
	| 'session/messages/resume-loaded'
	| 'session/messages/truncated'
	| 'session/replay-reset'
	| 'session/stream-blocks-cleared'
	| 'session/background-result'
>;

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
	const remainingMessages = source.filter((message) => !moving.includes(message));
	const prepared = moving.map((message) =>
		message.role === 'user' ? { ...message, received: true, steering: false } : message,
	);
	return {
		...state,
		messages: {
			...state.messages,
			[fromSessionId]: remainingMessages,
			[toSessionId]: [...prepared, ...messagesOf(state, toSessionId)],
		},
	};
}
export function reduceTranscript(
	inputState: SessionReducerState,
	action: TranscriptReducerAction,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/cleared': {
			const draft = messagesOf(state, DRAFT_SESSION_ID);
			return {
				...state,
				messages: draft.length ? { [DRAFT_SESSION_ID]: draft } : {},
				replay: {
					eventSequenceBySession: {},
					chunkSeqByMessage: {},
					blockIdsBySession: {},
				},
				pendingMessageOwners: {},
			};
		}
		case 'session/messages/optimistic-added': {
			const next = withMessages(state, action.sessionId, (messages) => [
				...messages,
				action.message,
			]);
			return {
				...next,
				pendingMessageOwners: {
					...next.pendingMessageOwners,
					[action.message.id]: {
						sessionId: action.sessionId,
					},
				},
			};
		}
		case 'session/messages/accepted': {
			let next = moveMessages(
				state,
				action.fromSessionId,
				action.toSessionId,
				new Set([action.optimisticMessageId]),
			);
			if (
				action.persistedMessageId &&
				action.persistedMessageId !== action.optimisticMessageId
			) {
				next = withMessages(next, action.toSessionId, (messages) => {
					const index = messages.findIndex(
						(message) => message.id === action.optimisticMessageId,
					);
					if (index < 0) return messages;
					const result = messages.slice();
					result[index] = { ...result[index], id: action.persistedMessageId as string };
					return result;
				});
			}
			const pendingMessageOwners = { ...next.pendingMessageOwners };
			delete pendingMessageOwners[action.optimisticMessageId];
			return { ...next, pendingMessageOwners };
		}
		case 'session/messages/rejected': {
			const pendingOwner = state.pendingMessageOwners[action.messageId];
			let next = withMessages(state, action.sessionId, (messages) =>
				messages.filter((message) => message.id !== action.messageId),
			);
			if (pendingOwner && pendingOwner.sessionId !== action.sessionId) {
				next = withMessages(next, pendingOwner.sessionId, (messages) =>
					messages.filter((message) => message.id !== action.messageId),
				);
			}
			const pendingMessageOwners = { ...next.pendingMessageOwners };
			delete pendingMessageOwners[action.messageId];
			return { ...next, pendingMessageOwners };
		}
		case 'session/messages/cleared': {
			const next = clearReplayForSession(state, action.sessionId);
			const messages = { ...next.messages };
			delete messages[action.sessionId];
			return { ...next, messages };
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
		case 'session/messages/adopt-draft': {
			const draftMessageIds = new Set(
				messagesOf(state, DRAFT_SESSION_ID).map((message) => message.id),
			);
			const next = moveMessages(state, DRAFT_SESSION_ID, action.sessionId);
			if (next === state) return next;
			const pendingMessageOwners = { ...next.pendingMessageOwners };
			for (const messageId of draftMessageIds) {
				if (pendingMessageOwners[messageId]?.sessionId === DRAFT_SESSION_ID) {
					pendingMessageOwners[messageId] = { sessionId: action.sessionId };
				}
			}
			return { ...next, pendingMessageOwners };
		}
		case 'session/asks-settled': {
			const askById = new Map(action.asks.map((ask) => [ask.requestId, ask]));
			if (askById.size === 0) return state;
			return withMessages(state, action.sessionId, (messages) => {
				let changed = false;
				const next = messages.map((message) => {
					if (message.type !== 'ask' || !askById.has(message.id)) return message;
					const ask = askById.get(message.id)!;
					changed = true;
					return {
						...message,
						awaiting: false,
						resolved: ask.resolved ?? null,
					};
				});
				return changed ? next : messages;
			});
		}
		case 'session/messages/resume-loaded': {
			const excluded = new Set(action.excludeMessageIds || []);
			const existing = messagesOf(state, action.sessionId).filter(
				(message) =>
					!excluded.has(message.id) &&
					(!action.preserveStreamingOnly ||
						message.streaming ||
						(message.role === 'user' && message.steering)),
			);
			return withMessages(state, action.sessionId, () =>
				mergeLiveStreaming(action.messages, existing),
			);
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
			const sessionIds = action.sessionId ? [action.sessionId] : Object.keys(state.messages);
			return sessionIds.reduce(
				(next, sessionId) =>
					withMessages(next, sessionId, (messages) => {
						let changed = false;
						const next = messages.map((message) => {
							if (message.toolRunId !== action.toolRunId) return message;
							changed = true;
							return {
								...message,
								content: action.content,
								toolRunId: null,
								sourceToolRunId: message.sourceToolRunId ?? message.toolRunId,
								streaming: false,
							};
						});
						return changed ? next : messages;
					}),
				state,
			);
		}
	}
	return inputState;
}

/** Normalize a terminal background ToolRun into the tool-card payload. */
export function backgroundToolRunResultContent(payload: ToolRunPayload): string | null {
	if (!payload.toolRunId) return null;
	const status = payload.status ?? 'completed';
	const rawOutput = payload.output ?? payload.error ?? '';
	if (typeof rawOutput === 'string' && rawOutput.trim().startsWith('{')) return rawOutput;
	return JSON.stringify({
		output: rawOutput,
		background: true,
		tool_run_id: payload.toolRunId,
		status,
		...(payload.error && !payload.output ? { error: payload.error } : {}),
	});
}
