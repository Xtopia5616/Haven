import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import {
	createChatAgentEventHandlers,
} from './chatAgentEventHandlers.ts';
import { toolOutputPreviewStore } from './stores.ts';

describe('chat agent live tool output', () => {
	it('keeps preview ticks out of the session reducer hot path', () => {
		const dispatchSession = vi.fn();
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession,
		});
		toolOutputPreviewStore.set({});

		handlers['agent:tool_output']({
			payload: { sessionId: 'ses-live', stepId: 'step-shell', output: 'line 1' },
		} as never);

		expect(dispatchSession).not.toHaveBeenCalled();
		expect(get(toolOutputPreviewStore)).toEqual({ 'step-shell': 'line 1' });
	});

	it('clears the side-channel when the canonical observation arrives', () => {
		const handlers = createChatAgentEventHandlers({
			chunkHandler: vi.fn(),
			flushChunksNow: vi.fn(),
			dispatchSession: vi.fn(),
		});
		toolOutputPreviewStore.set({ 'step-shell': 'partial' });

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

		expect(get(toolOutputPreviewStore)).toEqual({});
	});
});
