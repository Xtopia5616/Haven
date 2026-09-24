import type { AgentSupplementPayload } from '../contracts/agent.ts';
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
} from '../streaming.ts';
import { hasToolPreambleInBlock } from '../toolIntent.ts';
import {
	acceptEventSequence,
	blockIdsOf,
	registerBlock,
	replayOf,
	streamBlockKey,
	withMessages,
	messagesOf,
} from './state.ts';
import type { AgentChunkBatchItem, SessionActionOf, SessionReducerState } from './types.ts';

type Action = SessionActionOf<
	| 'agent/chunks'
	| 'agent/thought'
	| 'agent/stream-reset'
	| 'agent/web-search'
	| 'agent/supplement'
	| 'agent/action'
	| 'agent/observation'
>;

/**
 * Apply one frame of stream deltas with one state/message-list copy per
 * session. The frame action avoids copying the complete message array for
 * every token while retaining the same sequence, block registration and
 * arrival-order semantics.
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
	const messages = { ...state.messages };
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
export function reduceAgent(inputState: SessionReducerState, action: Action): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'agent/chunks': {
			return applyAgentChunks(state, action.chunks);
		}
		case 'agent/thought': {
			const payload = action.payload;
			const accepted = acceptEventSequence(
				state,
				payload.sessionId,
				payload.eventSeq,
				payload.messageId,
			);
			if (!accepted) return state;
			const registered = registerBlock(
				accepted,
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
				action.payload.supplementId,
			);
			return accepted ? applySupplement(accepted, action.payload) : state;
		}
		case 'agent/action': {
			const payload = action.payload;
			const accepted = acceptEventSequence(
				state,
				payload.sessionId,
				payload.eventSeq,
				payload.stepId,
			);
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
			const accepted = acceptEventSequence(
				state,
				payload.sessionId,
				payload.eventSeq,
				payload.stepId,
			);
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
	}
	return inputState;
}
