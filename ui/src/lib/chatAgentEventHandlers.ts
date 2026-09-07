import {
	actionIdFromObservation,
	applyThoughtSnap,
	dropStreamedThought,
	finalizeStreamBlocks,
	insertAgentMessage,
	newToolMessage,
	parseActionResultInject,
	resetStreamBlocks,
	webSearchCardContent,
	webSearchId,
} from './streaming';
import { clearToolOutputPreview, setToolOutputPreview, updateModelState } from './stores';
import { pruneSeq, updateSessionMessages } from './sessionMessages.ts';
import { hasToolPreambleInBlock } from './toolIntent.ts';

export interface ChatAgentEventContext {
	getActiveSessionId: () => string | null;
	blockIdsOf: (...args: any[]) => any;
	chunkHandler: (...args: any[]) => any;
	flushChunksNow: () => void;
}

/**
 * Build the Agent event handlers used by the chat route. The route owns
 * reactive state and listener registration; this module owns the event-to-
 * transcript transformations for streaming, tools, supplements and search.
 */
export function createChatAgentEventHandlers({
	getActiveSessionId,
	blockIdsOf,
	chunkHandler,
	flushChunksNow,
}: ChatAgentEventContext): Record<string, (event: any) => void> {
	return {
		'agent:thought': (event) => {
			const data = event.payload;
			const sessionId = data.sessionId;
			// The snap carries the minted message id the chunks streamed into (and
			// the DB row is persisted under), so reconcile by id. The sibling
			// reasoning id comes from the block registry.
			const thoughtId = data.messageId;
			const { reasoningId } = blockIdsOf(sessionId, data.stepNumber, data.runId);
			// Apply queued chunks first so no delta is left to accumulate onto the
			// finalized message afterwards.
			flushChunksNow();
			if (thoughtId) pruneSeq(thoughtId);
			if (reasoningId) pruneSeq(reasoningId);
			// The chunk handler owns the streaming state; forcing ready here causes
			// a visible ready->tool flicker when the step continues with tools.
			updateSessionMessages(sessionId, (messages) =>
				applyThoughtSnap(messages, {
					messageId: thoughtId,
					reasoningId,
					thought: data.thought,
					stepNumber: data.stepNumber,
					runId: data.runId,
					time: new Date().toLocaleTimeString(),
				}),
			);
		},
		'agent:thought_chunk': chunkHandler(true, undefined),
		'agent:reasoning_chunk': chunkHandler(false, 'reasoning'),
		'agent:stream_reset': (event) => {
			const data = event.payload;
			if (!data?.sessionId) return;
			// Reset travels through the same backend queue as deltas. Flush the
			// current UI frame before replacing the failed output generation.
			flushChunksNow();
			pruneSeq(data.thoughtMessageId);
			pruneSeq(data.reasoningMessageId);
			updateSessionMessages(data.sessionId, (messages) =>
				resetStreamBlocks(messages, data.reasoningMessageId, data.thoughtMessageId),
			);
		},
		'agent:web_search': (event) => {
			const data = event.payload;
			const sessionId = data.sessionId;
			if (!sessionId || (getActiveSessionId() && sessionId !== getActiveSessionId())) return;
			// Search starts a new card after any preceding thought/reasoning text.
			flushChunksNow();
			const callId = data.callId || null;
			// A null-id card would collide with the later keyed update.
			if (!callId) return;
			const searchId = webSearchId(sessionId, data.stepNumber, data.runId, callId);
			const placeholderId = webSearchId(sessionId, data.stepNumber, data.runId, null);
			const { reasoningId, thoughtId } = blockIdsOf(sessionId, data.stepNumber, data.runId);
			updateSessionMessages(sessionId, (messages) => {
				let next = messages;
				let existing = next.find((message) => message.id === searchId);
				// Upgrade a legacy null-id placeholder when the real call id arrives.
				if (!existing) {
					const placeholderIndex = next.findIndex(
						(message) =>
							message.id === placeholderId && message.toolName === 'web_search',
					);
					if (placeholderIndex >= 0) {
						next = next.map((message, index) =>
							index === placeholderIndex ? { ...message, id: searchId } : message,
						);
						existing = next[placeholderIndex];
					}
				}
				const content = webSearchCardContent(data, existing?.content);
				// Only a new card finalizes the preceding stream blocks. Later phases
				// for the same call id must not finalize post-search text.
				if (!existing) {
					if (reasoningId) pruneSeq(reasoningId);
					if (thoughtId) pruneSeq(thoughtId);
					next = finalizeStreamBlocks(next, reasoningId, thoughtId);
				}
				if (data.phase === 'completed') {
					if (!existing) {
						return insertAgentMessage(
							next,
							newToolMessage({
								id: searchId,
								stepNumber: data.stepNumber,
								toolName: 'web_search',
								time: new Date().toLocaleTimeString(),
								content,
								streaming: false,
							}),
						);
					}
					return next.map((message) =>
						message.id === searchId
							? { ...message, streaming: false, content }
							: message,
					);
				}
				if (existing) {
					return next.map((message) =>
						message.id === searchId
							? { ...message, content, streaming: true }
							: message,
					);
				}
				return insertAgentMessage(
					next,
					newToolMessage({
						id: searchId,
						stepNumber: data.stepNumber,
						toolName: 'web_search',
						time: new Date().toLocaleTimeString(),
						content,
						streaming: true,
					}),
				);
			});
		},
		'agent:supplement': (event) => {
			// Human steering marks the matching user bubble received. Cross-session
			// mail and background wake-ups become visible tool cards.
			const data = event.payload;
			const sessionId = data.sessionId;
			const context = (data.additionalContext || '').trim();
			if (!sessionId || !context) return;
			const source = data.injectSource;
			const supplementId = data.supplementId || `supplement-${data.stepNumber ?? 0}-${data.runId ?? 0}`;
			if (source === 'cross_session') {
				const cardId = `peer-mail-${supplementId}`;
				const content = JSON.stringify({ operation: 'inbox', auto: true, text: context });
				updateSessionMessages(sessionId, (messages) => {
					if (
						messages.some(
							(message) =>
								message.id === cardId ||
								(message.toolName === 'agent' && message.content === content),
						)
					)
						return messages;
					return insertAgentMessage(
						messages,
						newToolMessage({
							id: cardId,
							stepNumber: data.stepNumber ?? 0,
							toolName: 'agent',
							content,
							time: new Date().toLocaleTimeString(),
						}),
					);
				});
				return;
			}
			if (source === 'action_result') {
				const parsed = parseActionResultInject(context);
				const actionId = parsed?.action_id || 'unknown';
				const cardId = `action-result-${supplementId}-${actionId}`;
				const content = JSON.stringify(
					parsed || {
						operation: 'result_injected',
						action_id: actionId,
						status: 'completed',
						auto: true,
					},
				);
				updateSessionMessages(sessionId, (messages) => {
					if (messages.some((message) => message.id === cardId)) return messages;
					return insertAgentMessage(
						messages,
						newToolMessage({
							id: cardId,
							stepNumber: data.stepNumber ?? 0,
							toolName: 'actions',
							content,
							time: new Date().toLocaleTimeString(),
						}),
					);
				});
				return;
			}
			updateSessionMessages(sessionId, (messages) => {
				const messageId = data.messageId;
				if (messageId) {
					const index = messages.findIndex((message) => message.id === messageId);
					if (index >= 0) {
						const next = [...messages];
						next[index] = { ...next[index], received: true, steering: false };
						return next;
					}
				}
				let marked = false;
				const next = [...messages];
				for (let index = next.length - 1; index >= 0; index -= 1) {
					const message = next[index];
					if (
						message.role === 'user' &&
						!message.received &&
						(message.content || '').trim() === context
					) {
						next[index] = { ...message, received: true, steering: false };
						marked = true;
						break;
					}
				}
				return marked ? next : messages;
			});
		},
		'agent:action': (event) => {
			const data = event.payload;
			const sessionId = data.sessionId;
			flushChunksNow();
			updateModelState('tool');
			const toolMessageId = data.stepId;
			const { reasoningId, thoughtId } = blockIdsOf(sessionId, data.stepNumber, data.runId);
			if (reasoningId) pruneSeq(reasoningId);
			if (thoughtId) pruneSeq(thoughtId);
			if (data.silent) {
				updateSessionMessages(sessionId, (messages) =>
					finalizeStreamBlocks(
						data.suppressStreamedThought
							? dropStreamedThought(messages, thoughtId)
							: messages,
						reasoningId,
						thoughtId,
					),
				);
				return;
			}
			updateSessionMessages(sessionId, (messages) => {
				const fixed = finalizeStreamBlocks(
					data.suppressStreamedThought
						? dropStreamedThought(messages, thoughtId)
						: messages,
					reasoningId,
					thoughtId,
				);
				if (fixed.some((message) => message.id === toolMessageId)) return fixed;
				const showFallbackIntent = !hasToolPreambleInBlock(fixed, thoughtId);
				return insertAgentMessage(
					fixed,
					newToolMessage({
						id: toolMessageId,
						stepNumber: data.stepNumber,
						toolName: data.toolName,
						time: new Date().toLocaleTimeString(),
						streaming: true,
						toolArgs: data.input ?? null,
						showFallbackIntent,
					}),
				);
			});
		},
		'agent:tool_output': (event) => {
			const data = event.payload || {};
			const toolMessageId = data.stepId;
			const output = typeof data.output === 'string' ? data.output : '';
			if (!toolMessageId) return;
			setToolOutputPreview(toolMessageId, output);
		},
		'agent:observation': (event) => {
			const data = event.payload;
			const sessionId = data.sessionId;
			const toolMessageId = data.stepId;
			const { thoughtId } = blockIdsOf(sessionId, data.stepNumber, data.runId);
			if (data.silent) {
				clearToolOutputPreview(toolMessageId);
				if (toolMessageId) {
					updateSessionMessages(sessionId, (messages) =>
						messages.filter((message) => message.id !== toolMessageId),
					);
				}
				return;
			}
			flushChunksNow();
			updateModelState('streaming');
			clearToolOutputPreview(toolMessageId);
			const actionId = actionIdFromObservation(data.observation);
			updateSessionMessages(sessionId, (messages) => {
				const index = messages.findIndex((message) => message.id === toolMessageId);
				const showFallbackIntent = !hasToolPreambleInBlock(messages, thoughtId);
				const message = newToolMessage({
					id: toolMessageId,
					stepNumber: data.stepNumber,
					toolName: data.toolName,
					content: data.observation,
					askOptions: data.askOptions || [],
					outcome: data.outcome,
					actionId,
					showFallbackIntent,
				});
				if (index >= 0) {
					const next = [...messages];
					next[index] = { ...next[index], ...message, streaming: false };
					return next;
				}
				return insertAgentMessage(messages, message);
			});
		},
	};
}
