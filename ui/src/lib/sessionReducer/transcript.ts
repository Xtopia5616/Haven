import type { ActionPayload } from '../contracts/action.ts';
import { mergeLiveStreaming } from '../resumeMessages.ts';
import { clearReplayForSession, messagesOf, replayOf, withMessages } from './state.ts';
import {
	DRAFT_SESSION_ID,
	type SessionActionOf,
	type SessionMessage,
	type SessionReducerState,
} from './types.ts';

type Action = SessionActionOf<
	| 'sessions/cleared'
	| 'session/messages/optimistic-added'
	| 'session/messages/accepted'
	| 'session/messages/rejected'
	| 'session/messages/cleared'
	| 'session/messages/finalized'
	| 'session/messages/adopt-draft'
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
	const remaining = source.filter((message) => !moving.includes(message));
	const prepared = moving.map((message) =>
		message.role === 'user' ? { ...message, received: true, steering: false } : message,
	);
	return {
		...state,
		messages: {
			...state.messages,
			[fromSessionId]: remaining,
			[toSessionId]: [...prepared, ...messagesOf(state, toSessionId)],
		},
	};
}
export function reduceTranscript(
	inputState: SessionReducerState,
	action: Action,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/cleared': {
			const draft = messagesOf(state, DRAFT_SESSION_ID);
			return {
				...state,
				messages: draft.length ? { [DRAFT_SESSION_ID]: draft } : {},
				replay: { eventSeqBySession: {}, chunkSeqByMessage: {}, blockIdsBySession: {} },
				optimistic: {},
			};
		}
		case 'session/messages/optimistic-added': {
			const next = withMessages(state, action.sessionId, (messages) => [
				...messages.filter((message) => !message.id.startsWith('placeholder-')),
				action.message,
			]);
			return {
				...next,
				optimistic: {
					...next.optimistic,
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
			const optimistic = { ...next.optimistic };
			optimistic[action.optimisticId] = {
				sessionId: action.toSessionId,
				messageId: action.persistedId || action.optimisticId,
				status: 'accepted',
			};
			return { ...next, optimistic };
		}
		case 'session/messages/rejected': {
			const optimisticEntry = state.optimistic[action.messageId];
			let next = withMessages(state, action.sessionId, (messages) =>
				messages.filter((message) => message.id !== action.messageId),
			);
			if (optimisticEntry && optimisticEntry.sessionId !== action.sessionId) {
				next = withMessages(next, optimisticEntry.sessionId, (messages) =>
					messages.filter((message) => message.id !== action.messageId),
				);
			}
			const optimistic = { ...next.optimistic };
			if (optimistic[action.messageId])
				optimistic[action.messageId] = {
					...optimistic[action.messageId],
					status: 'rejected',
				};
			return { ...next, optimistic };
		}
		case 'session/messages/cleared': {
			const next = withMessages(state, action.sessionId, () => []);
			return clearReplayForSession(next, action.sessionId);
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
	}
	return inputState;
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
