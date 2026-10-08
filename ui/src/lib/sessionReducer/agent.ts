import type { AgentSupplementPayload } from '../contracts/agent.ts';
import type { AgentToolCallChunkPayload } from '../contracts/agent.ts';
import {
	toolRunIdFromObservation,
	sourceToolRunIdFromObservation,
	accumulateStreamChunk,
	applyThoughtSnap,
	dropStreamedThought,
	finalizeStreamBlocks,
	insertAgentMessage,
	newToolMessage,
	parseToolRunResultInject,
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

type AgentReducerAction = SessionActionOf<
	| 'agent/chunks'
	| 'agent/tool-call-chunk'
	| 'agent/thought'
	| 'agent/stream-reset'
	| 'agent/web-search'
	| 'agent/supplement'
	| 'agent/tool_call'
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

function isToolCallPreviewFor(
	message: {
		toolCallPreview?: boolean;
		stepNumber?: number | null;
		runId?: number | null;
		toolCallIndex?: number;
	},
	stepNumber: number,
	runId: number,
	toolIndex?: number,
): boolean {
	return (
		message.toolCallPreview === true &&
		message.stepNumber === stepNumber &&
		message.runId === runId &&
		(toolIndex === undefined || message.toolCallIndex === toolIndex)
	);
}

function reduceToolCallChunk(
	state: SessionReducerState,
	payload: AgentToolCallChunkPayload,
): SessionReducerState {
	if (!payload.sessionId || !payload.previewId) return state;
	const replay = replayOf(state);
	const previous = replay.chunkSeqByMessage[payload.previewId];
	if (previous != null && payload.seq <= previous) return state;
	const replayState = {
		...state,
		replay: {
			...replay,
			chunkSeqByMessage: {
				...replay.chunkSeqByMessage,
				[payload.previewId]: payload.seq,
			},
		},
	};
	return withMessages(replayState, payload.sessionId, (messages) => {
		const existing = messages.findIndex((message) => message.id === payload.previewId);
		const preview = newToolMessage({
			id: payload.previewId,
			stepNumber: payload.stepNumber,
			toolName: payload.toolName || 'tool_call',
			streaming: true,
			toolArgs: payload.arguments,
			toolCallPreview: true,
			toolCallIndex: payload.toolIndex,
			toolArgsStreaming: true,
			toolArgsTruncated: payload.argumentsTruncated,
		});
		if (existing >= 0) {
			const next = messages.slice();
			next[existing] = { ...next[existing], ...preview };
			return next;
		}
		const withoutDuplicate = messages.filter(
			(message) =>
				!isToolCallPreviewFor(
					message,
					payload.stepNumber,
					payload.runId,
					payload.toolIndex,
				),
		);
		return insertAgentMessage(withoutDuplicate, { ...preview, runId: payload.runId });
	});
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
	if (payload.injectSource === 'tool_run_result') {
		const parsed = parseToolRunResultInject(context);
		const toolRunId = parsed?.tool_run_id || 'unknown';
		const cardId = `tool-run-result-${supplementId}-${toolRunId}`;
		const content = JSON.stringify({
			...(parsed || { tool_run_id: toolRunId, status: 'completed', auto: true }),
			operation: 'tool_runs_result_injected',
		});
		return withMessages(state, payload.sessionId, (messages) => {
			if (messages.some((message) => message.id === cardId)) return messages;
			return insertAgentMessage(
				messages,
				newToolMessage({
					id: cardId,
					stepNumber: payload.stepNumber,
					toolName: 'tool_runs.inspect',
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
export function reduceAgent(
	inputState: SessionReducerState,
	action: AgentReducerAction,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'agent/chunks': {
			return applyAgentChunks(state, action.chunks);
		}
		case 'agent/tool-call-chunk': {
			return reduceToolCallChunk(state, action.payload);
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
				applyThoughtSnap(
					messages.filter(
						(message) =>
							!isToolCallPreviewFor(message, payload.stepNumber, payload.runId),
					),
					{
						messageId: payload.messageId,
						reasoningId: ids.reasoningId,
						thought: payload.thought,
						stepNumber: payload.stepNumber,
						runId: payload.runId,
						time: new Date().toLocaleTimeString(),
					},
				),
			);
		}
		case 'agent/stream-reset': {
			const payload = action.payload;
			const next = withMessages(state, payload.sessionId, (messages) =>
				resetStreamBlocks(
					messages,
					payload.reasoningMessageId,
					payload.thoughtMessageId,
				).filter(
					(message) => !isToolCallPreviewFor(message, payload.stepNumber, payload.runId),
				),
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
		case 'agent/tool_call': {
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
				const withoutPreview = messages.filter(
					(message) =>
						!isToolCallPreviewFor(
							message,
							payload.stepNumber,
							payload.runId,
							payload.toolIndex,
						),
				);
				const fixed = finalizeStreamBlocks(
					payload.suppressStreamedThought
						? dropStreamedThought(withoutPreview, ids.thoughtId)
						: withoutPreview,
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
				const withoutPreview = messages.filter(
					(message) =>
						!isToolCallPreviewFor(
							message,
							payload.stepNumber,
							payload.runId,
							payload.toolIndex,
						),
				);
				const index = withoutPreview.findIndex((message) => message.id === payload.stepId);
				const message = newToolMessage({
					id: payload.stepId,
					stepNumber: payload.stepNumber,
					toolName: payload.toolName,
					content: payload.observation,
					askOptions: payload.askOptions || [],
					renderer: payload.renderer,
					result: payload.result,
					toolRunId: toolRunIdFromObservation(payload.observation),
					sourceToolRunId: sourceToolRunIdFromObservation(
						payload.toolName,
						payload.observation,
					),
					showFallbackIntent: !hasToolPreambleInBlock(withoutPreview, ids.thoughtId),
				});
				if (index < 0) return insertAgentMessage(withoutPreview, message);
				const next = withoutPreview.slice();
				next[index] = { ...next[index], ...message, streaming: false };
				return next;
			});
			return updated;
		}
	}
	return inputState;
}
