import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatAgentEventHandlers } from './chatAgentEventHandlers.ts';
import {
	clearToolOutputPreviewsForSession,
	getToolOutputPreviewStore,
} from './toolOutputPreviewStore.ts';
import { reactExecutionPhaseStore } from './runtimeStateStore.ts';

describe('chat agent live tool output', () => {
	it('shows waiting for a tool result, then generation after the observation', () => {
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession: vi.fn(),
		});

		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
		handlers['agent:tool_call']({ payload: { sessionId: 'ses-live' } } as never);
		expect(get(reactExecutionPhaseStore)).toMatchObject({
			sessionId: 'ses-live',
			phase: 'waiting_result',
		});

		handlers['agent:observation']({
			payload: {
				sessionId: 'ses-live',
				stepId: 'step-shell',
				stepNumber: 1,
				runId: 1,
				eventSeq: 2,
				toolName: 'shell',
				observation: 'done',
			},
		} as never);
		expect(get(reactExecutionPhaseStore)).toMatchObject({
			sessionId: 'ses-live',
			phase: 'generating',
		});
	});

	it('keeps preview ticks out of the session reducer hot path', () => {
		const dispatchSession = vi.fn();
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession,
		});
		clearToolOutputPreviewsForSession(null);

		handlers['agent:tool_output']({
			payload: { sessionId: 'ses-live', stepId: 'step-shell', output: 'line 1' },
		} as never);

		expect(dispatchSession).not.toHaveBeenCalled();
		expect(get(getToolOutputPreviewStore('step-shell'))).toBe('line 1');
	});

	it('clears the side-channel when the canonical observation arrives', () => {
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession: vi.fn(),
		});
		getToolOutputPreviewStore('step-shell').set('partial');

		handlers['agent:observation']({
			payload: {
				sessionId: 'ses-live',
				stepId: 'step-shell',
				stepNumber: 1,
				runId: 1,
				eventSeq: 2,
				toolName: 'shell',
				observation: 'done',
			},
		} as never);

		expect(get(getToolOutputPreviewStore('step-shell'))).toBeUndefined();
	});
});
