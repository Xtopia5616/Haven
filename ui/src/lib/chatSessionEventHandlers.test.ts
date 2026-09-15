import { describe, expect, it, vi } from 'vitest';
import { createChatSessionEventHandlers } from './chatSessionEventHandlers.ts';
import { initialSessionState, SessionReducer } from './sessionReducer.ts';

function handlers(options: {
	fresh?: boolean;
	adoptedDraft?: boolean;
	activeSessionId?: string | null;
	reducer?: SessionReducer;
	dispatchSession?: (action: import('./sessionReducer.ts').SessionAction) => void;
	flushChunksNow?: () => void;
}) {
	return createChatSessionEventHandlers({
		getActiveSessionId: () => options.activeSessionId ?? null,
		isFreshSessionIntent: () => options.fresh ?? false,
		adoptDraftMessages: () => options.adoptedDraft ?? false,
		dispatchSession: options.dispatchSession ?? ((action) => options.reducer?.dispatch(action)),
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
	it.each(['paused'] as const)('stops live bubbles when a session is %s', (status) => {
		const flushChunksNow = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			messages: {
				['ses-paused']: [
					{ id: 'step-thought', role: 'assistant', content: '半截回复', streaming: true },
					{
						id: 'ask-1',
						type: 'ask',
						content: '还要继续吗？',
						awaiting: true,
						streaming: false,
					},
				],
			},
		});
		const eventHandlers = handlers({
			activeSessionId: 'ses-paused',
			flushChunksNow,
			reducer,
		});

		eventHandlers['session:updated']({
			payload: { sessionId: 'ses-paused', status, title: null },
		} as never);

		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(reducer.getMessages('ses-paused')).toEqual([
			{ id: 'step-thought', role: 'assistant', content: '半截回复', streaming: false },
			{
				id: 'ask-1',
				type: 'ask',
				content: '还要继续吗？',
				awaiting: true,
				streaming: false,
			},
		]);
	});

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
			status: 'pending',
			title: null,
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
			status: 'pending',
			title: null,
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
