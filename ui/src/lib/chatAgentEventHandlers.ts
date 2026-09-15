import { updateModelState } from './stores';
import type { SessionAction } from './sessionReducer.ts';

export interface ChatAgentEventContext {
	/** Kept optional for callers that still provide the active-session probe. */
	getActiveSessionId?: () => string | null;
	chunkHandler: (...args: any[]) => any;
	flushChunksNow: () => void;
	dispatchSession: (action: SessionAction) => void;
}

/**
 * Build the Agent event handlers used by the chat route. The route owns
 * reactive state and listener registration; this module owns the event-to-
 * transcript transformations for streaming, tools, supplements and search.
 */
export function createChatAgentEventHandlers({
	chunkHandler,
	flushChunksNow,
	dispatchSession,
}: ChatAgentEventContext): Record<string, (event: any) => void> {
	return {
		'agent:thought': (event) => {
			const data = event.payload;
			flushChunksNow();
			dispatchSession({ type: 'agent/thought', payload: data });
		},
		'agent:thought_chunk': chunkHandler(true, undefined),
		'agent:reasoning_chunk': chunkHandler(false, 'reasoning'),
		'agent:stream_reset': (event) => {
			const data = event.payload;
			if (!data?.sessionId) return;
			// Reset travels through the same backend queue as deltas. Flush the
			// current UI frame before replacing the failed output generation.
			flushChunksNow();
			dispatchSession({ type: 'agent/stream-reset', payload: data });
		},
		'agent:web_search': (event) => {
			const data = event.payload;
			const sessionId = data.sessionId;
			if (!sessionId) return;
			// Search starts a new card after any preceding thought/reasoning text.
			flushChunksNow();
			dispatchSession({ type: 'agent/web-search', payload: data });
		},
		'agent:supplement': (event) => {
			// Human steering marks the matching user bubble received. Cross-session
			// mail and background wake-ups become visible tool cards.
			const data = event.payload;
			dispatchSession({ type: 'agent/supplement', payload: data });
		},
		'agent:action': (event) => {
			const data = event.payload;
			flushChunksNow();
			updateModelState('tool');
			dispatchSession({ type: 'agent/action', payload: data });
		},
		'agent:tool_output': (event) => {
			const data = event.payload;
			if (!data?.stepId) return;
			dispatchSession({
				type: 'agent/tool-output',
				sessionId: data.sessionId,
				stepId: data.stepId,
				output: data.output || '',
			});
		},
		'agent:observation': (event) => {
			const data = event.payload;
			flushChunksNow();
			updateModelState('streaming');
			dispatchSession({ type: 'agent/observation', payload: data });
		},
	};
}
