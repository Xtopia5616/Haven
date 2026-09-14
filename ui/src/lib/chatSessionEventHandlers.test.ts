import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatSessionEventHandlers } from './chatSessionEventHandlers.ts';
import { sessionMessagesStore, setSessionMessages } from './sessionMessages.ts';

function handlers(options: {
	fresh?: boolean;
	adoptedDraft?: boolean;
	activeSessionId?: string | null;
	dispatchSession?: (action: import('./sessionReducer.ts').SessionAction) => void;
	flushChunksNow?: () => void;
}) {
	return createChatSessionEventHandlers({
		getActiveSessionId: () => options.activeSessionId ?? null,
		isFreshSessionIntent: () => options.fresh ?? false,
		adoptDraftMessages: () => options.adoptedDraft ?? false,
		dispatchSession: options.dispatchSession ?? vi.fn(),
		getSessionErrorId: () => null,
		rememberSessionError: vi.fn(),
		forgetSessionError: vi.fn(),
		clearAskAwaiting: vi.fn(),
		evictTerminalSessionMemory: vi.fn(),
		clearStepBlockIds: vi.fn(),
		flushChunksNow: options.flushChunksNow ?? vi.fn(),
		updateSessionTitle: vi.fn(),
		loadSessions: vi.fn(),
	});
}

describe('chat session lifecycle handlers', () => {
	beforeEach(() => {
		sessionMessagesStore.set({});
	});

	it.each(['paused', 'paused_awaiting_answer', 'paused_awaiting_confirm'] as const)(
		'stops live bubbles when a session is %s',
		(status) => {
			const flushChunksNow = vi.fn();
			setSessionMessages('ses-paused', [
				{ id: 'step-thought', role: 'assistant', content: '半截回复', streaming: true },
				{
					id: 'ask-1',
					type: 'ask',
					content: '还要继续吗？',
					awaiting: true,
					streaming: false,
				},
			]);
			const eventHandlers = handlers({
				activeSessionId: 'ses-paused',
				flushChunksNow,
			});

			eventHandlers['session:updated']({
				payload: { sessionId: 'ses-paused', status, title: null },
			} as never);

			expect(flushChunksNow).toHaveBeenCalledOnce();
			expect(get(sessionMessagesStore)['ses-paused']).toEqual([
				{ id: 'step-thought', role: 'assistant', content: '半截回复', streaming: false },
				{
					id: 'ask-1',
					type: 'ask',
					content: '还要继续吗？',
					awaiting: true,
					streaming: false,
				},
			]);
		},
	);

	it('selects a fresh session when it adopts the pending draft', () => {
		const dispatchSession = vi.fn();
		const eventHandlers = handlers({
			fresh: true,
			adoptedDraft: true,
			dispatchSession,
		});

		eventHandlers['session:created']({
			payload: { sessionId: 'ses-fast', status: 'pending', title: null },
		} as never);

		expect(dispatchSession).toHaveBeenCalledWith({
			type: 'session/created',
			sessionId: 'ses-fast',
			freshStart: true,
			adoptedDraft: true,
		});
	});

	it('does not select an unrelated session while a fresh draft is pending', () => {
		const dispatchSession = vi.fn();
		const eventHandlers = handlers({ fresh: true, adoptedDraft: false, dispatchSession });

		eventHandlers['session:created']({
			payload: { sessionId: 'ses-background', status: 'pending', title: null },
		} as never);

		expect(dispatchSession).toHaveBeenCalledWith({
			type: 'session/created',
			sessionId: 'ses-background',
			freshStart: true,
			adoptedDraft: false,
		});
	});

	it('passes the failure reason to the active-session error handler', () => {
		const dispatchSession = vi.fn();
		const eventHandlers = handlers({ activeSessionId: 'ses-error', dispatchSession });

		eventHandlers['session:error']({
			payload: { sessionId: 'ses-error', error: '网络请求超时' },
		} as never);

		expect(dispatchSession).toHaveBeenCalledWith({
			type: 'session/error-shown',
			sessionId: 'ses-error',
			reason: '网络请求超时',
		});
	});
});
