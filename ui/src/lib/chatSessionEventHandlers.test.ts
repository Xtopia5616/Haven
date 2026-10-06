import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatSessionEventHandler } from './chatSessionEventHandlers.ts';
import { initialSessionState, SessionReducer } from './sessionReducer.ts';
import { getToolOutputPreviewStore, setToolOutputPreview } from './toolOutputPreviewStore.ts';

function handler(options: {
	fresh?: boolean;
	adoptedDraft?: boolean;
	activeSessionId?: string | null;
	dispatchSession: (action: import('./sessionReducer.ts').SessionAction) => void;
	flushChunksNow?: () => void;
	clearAskAwaiting?: (sessionId: string | null) => void;
	evictTerminalSessionMemory?: (sessionId: string) => void;
	clearStepBlockIds?: (sessionId: string | null) => void;
	updateSessionTitle?: (sessionId: string, title: string) => void;
	scheduleLoadSessions?: () => void;
}) {
	return createChatSessionEventHandler({
		getActiveSessionId: () => options.activeSessionId ?? null,
		isFreshSessionIntent: () => options.fresh ?? false,
		adoptDraftMessages: () => options.adoptedDraft ?? false,
		dispatchSession: options.dispatchSession,
		getSessionErrorId: () => null,
		clearAskAwaiting: options.clearAskAwaiting ?? vi.fn(),
		evictTerminalSessionMemory: options.evictTerminalSessionMemory ?? vi.fn(),
		clearStepBlockIds: options.clearStepBlockIds ?? vi.fn(),
		flushChunksNow: options.flushChunksNow ?? vi.fn(),
		updateSessionTitle: options.updateSessionTitle ?? vi.fn(),
		scheduleLoadSessions: options.scheduleLoadSessions ?? vi.fn(),
	});
}

describe('chat session lifecycle handler', () => {
	it('stops live bubbles when a session is paused without clearing its pending ask', () => {
		const flushChunksNow = vi.fn();
		const clearAskAwaiting = vi.fn();
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
		const onLifecycle = handler({
			activeSessionId: 'ses-paused',
			flushChunksNow,
			clearAskAwaiting,
			dispatchSession: (action) => reducer.dispatch(action),
		});
		setToolOutputPreview('step-shell', 'partial', 'ses-paused');

		onLifecycle({
			payload: {
				type: 'updated',
				sessionId: 'ses-paused',
				status: 'paused',
				waitingReason: 'ask',
				title: 'Question',
				reason: null,
			},
		} as never);

		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).not.toHaveBeenCalled();
		expect(get(getToolOutputPreviewStore('step-shell'))).toBeUndefined();
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
		const onLifecycle = handler({
			fresh: true,
			adoptedDraft: true,
			dispatchSession,
		});

		onLifecycle({
			payload: {
				type: 'created',
				sessionId: 'ses-fast',
				status: 'pending',
				waitingReason: null,
				title: null,
			},
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

	it('projects a completion and performs terminal cleanup once from the same event', () => {
		const sessionId = 'ses-done';
		const flushChunksNow = vi.fn();
		const clearAskAwaiting = vi.fn();
		const evictTerminalSessionMemory = vi.fn();
		const clearStepBlockIds = vi.fn();
		const scheduleLoadSessions = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ id: sessionId, status: 'running', title: '旧标题' }],
			activeSessionId: sessionId,
			messages: {
				[sessionId]: [
					{ id: 'step-live', role: 'assistant', content: '最后一段', streaming: true },
				],
			},
		});
		const onLifecycle = handler({
			activeSessionId: sessionId,
			flushChunksNow,
			clearAskAwaiting,
			evictTerminalSessionMemory,
			clearStepBlockIds,
			scheduleLoadSessions,
			dispatchSession: (action) => reducer.dispatch(action),
		});
		setToolOutputPreview('step-live', 'partial', sessionId);

		onLifecycle({
			payload: {
				type: 'completed',
				sessionId,
				title: '研究',
				reason: '用户主动结束会话',
			},
		} as never);

		expect(reducer.getState().sessions[0]).toMatchObject({
			id: sessionId,
			status: 'completed',
			title: '研究',
		});
		expect(reducer.getState().termination).toEqual({
			sessionId,
			status: 'completed',
			reason: '用户主动结束会话',
		});
		expect(reducer.getMessages(sessionId)[0].streaming).toBe(false);
		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).toHaveBeenCalledOnce();
		expect(evictTerminalSessionMemory).toHaveBeenCalledOnce();
		expect(clearStepBlockIds).toHaveBeenCalledOnce();
		expect(scheduleLoadSessions).toHaveBeenCalledOnce();
		expect(get(getToolOutputPreviewStore('step-live'))).toBeUndefined();
	});

	it('projects an error and updates terminal status, error details, and cleanup together', () => {
		const sessionId = 'ses-error';
		const clearAskAwaiting = vi.fn();
		const evictTerminalSessionMemory = vi.fn();
		const flushChunksNow = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ id: sessionId, status: 'running', title: '旧标题' }],
			activeSessionId: sessionId,
		});
		const onLifecycle = handler({
			activeSessionId: sessionId,
			clearAskAwaiting,
			flushChunksNow,
			evictTerminalSessionMemory,
			dispatchSession: (action) => reducer.dispatch(action),
		});

		onLifecycle({
			payload: {
				type: 'error',
				sessionId,
				title: '构建',
				error: '网络请求超时',
			},
		} as never);

		expect(reducer.getState().sessions[0]).toMatchObject({
			status: 'error',
			title: '构建',
		});
		expect(reducer.getSessionErrorReason(sessionId)).toBe('网络请求超时');
		expect(clearAskAwaiting).toHaveBeenCalledOnce();
		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(evictTerminalSessionMemory).toHaveBeenCalledOnce();
	});

	it('refreshes inactive terminal sessions after clearing their queued output', () => {
		const cleanupOrder: string[] = [];
		const onLifecycle = handler({
			activeSessionId: 'ses-active',
			flushChunksNow: () => cleanupOrder.push('flush'),
			evictTerminalSessionMemory: () => cleanupOrder.push('evict'),
			dispatchSession: vi.fn(),
		});
		setToolOutputPreview('step-background', '后台命令输出', 'ses-background');

		onLifecycle({
			payload: {
				type: 'error',
				sessionId: 'ses-background',
				title: '后台会话',
				error: '请求失败',
			},
		} as never);

		expect(cleanupOrder).toEqual(['flush', 'evict']);
		expect(get(getToolOutputPreviewStore('step-background'))).toBeUndefined();
	});

	it('clears the resumed ask indicator on a pending update', () => {
		const clearAskAwaiting = vi.fn();
		const onLifecycle = handler({
			activeSessionId: 'ses-active',
			clearAskAwaiting,
			dispatchSession: vi.fn(),
		});

		onLifecycle({
			payload: {
				type: 'updated',
				sessionId: 'ses-active',
				status: 'pending',
				waitingReason: null,
				title: '研究',
				reason: null,
			},
		} as never);

		expect(clearAskAwaiting).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).toHaveBeenCalledWith('ses-active');
	});

	it('updates a session title and refreshes the history list', () => {
		const updateSessionTitle = vi.fn();
		const scheduleLoadSessions = vi.fn();
		const onLifecycle = handler({
			updateSessionTitle,
			scheduleLoadSessions,
			dispatchSession: vi.fn(),
		});

		onLifecycle({
			payload: { type: 'title_updated', sessionId: 'ses-title', title: '新标题' },
		} as never);

		expect(updateSessionTitle).toHaveBeenCalledOnce();
		expect(updateSessionTitle).toHaveBeenCalledWith('ses-title', '新标题');
		expect(scheduleLoadSessions).toHaveBeenCalledOnce();
	});

	it('clears the deleted session projection from the same lifecycle event', () => {
		const sessionId = 'ses-deleted';
		const flushChunksNow = vi.fn();
		const clearAskAwaiting = vi.fn();
		const clearStepBlockIds = vi.fn();
		const scheduleLoadSessions = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ id: sessionId, status: 'paused', title: '删除目标' }],
			activeSessionId: sessionId,
			messages: { [sessionId]: [{ id: 'msg-1', role: 'user', content: '问题' }] },
		});
		const onLifecycle = handler({
			activeSessionId: sessionId,
			flushChunksNow,
			clearAskAwaiting,
			clearStepBlockIds,
			scheduleLoadSessions,
			dispatchSession: (action) => reducer.dispatch(action),
		});
		setToolOutputPreview('step-deleted', 'partial', sessionId);

		onLifecycle({ payload: { type: 'deleted', sessionId } } as never);

		expect(reducer.getState().sessions).toEqual([]);
		expect(reducer.getState().activeSessionId).toBeNull();
		expect(reducer.getMessages(sessionId)).toEqual([]);
		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).toHaveBeenCalledWith(sessionId);
		expect(clearStepBlockIds).toHaveBeenCalledWith(sessionId);
		expect(scheduleLoadSessions).toHaveBeenCalledOnce();
		expect(get(getToolOutputPreviewStore('step-deleted'))).toBeUndefined();
	});
});
