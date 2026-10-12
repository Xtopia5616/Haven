import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatAgentEventHandlers } from './chatAgentEventHandlers.ts';
import {
	clearToolOutputPreviewsForSession,
	getToolOutputPreviewStore,
} from './toolOutputPreviewStore.ts';
import { reactExecutionPhaseStore } from './reactExecutionPhaseStore.ts';

describe('chat agent live tool output', () => {
	it('shows waiting for a tool result, then generation after the observation', () => {
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession: vi.fn(),
		});

		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
		handlers['agent:tool_call']({ payload: { sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a' } } as never);
		expect(get(reactExecutionPhaseStore)).toMatchObject({
			sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
			phase: 'waiting_result',
		});

		handlers['agent:observation']({
			payload: {
				sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
				stepId: 'step-b1ff030bcda172afbbad69aa787379d5',
				stepNumber: 1,
				runId: 1,
				eventSequence: 2,
				toolName: 'shell',
				observation: 'done',
			},
		} as never);
		expect(get(reactExecutionPhaseStore)).toMatchObject({
			sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
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
			payload: { sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a', stepId: 'step-b1ff030bcda172afbbad69aa787379d5', output: 'line 1' },
		} as never);

		expect(dispatchSession).not.toHaveBeenCalled();
		expect(get(getToolOutputPreviewStore('step-b1ff030bcda172afbbad69aa787379d5'))).toBe('line 1');
	});

	it('clears the side-channel when the canonical observation arrives', () => {
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession: vi.fn(),
		});
		getToolOutputPreviewStore('step-b1ff030bcda172afbbad69aa787379d5').set('partial');

		handlers['agent:observation']({
			payload: {
				sessionId: 'ses-d628ca764938045bf3f3194d0ff8893a',
				stepId: 'step-b1ff030bcda172afbbad69aa787379d5',
				stepNumber: 1,
				runId: 1,
				eventSequence: 2,
				toolName: 'shell',
				observation: 'done',
			},
		} as never);

		expect(get(getToolOutputPreviewStore('step-b1ff030bcda172afbbad69aa787379d5'))).toBeUndefined();
	});
});
