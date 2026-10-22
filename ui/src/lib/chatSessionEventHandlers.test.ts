import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatSessionEventHandler } from './chatSessionEventHandlers.ts';
import { initialSessionState, SessionReducer } from './sessionReducer.ts';
import { getToolOutputPreviewStore, setToolOutputPreview } from './toolOutputPreviewStore.ts';
import { clearMediaPlans, mediaPlanStore, rememberMediaPlan } from './mediaPlanStore.ts';

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
		getErroredSessionId: () => null,
		clearAskAwaiting: options.clearAskAwaiting ?? vi.fn(),
		evictTerminalSessionMemory: options.evictTerminalSessionMemory ?? vi.fn(),
		clearStepBlockIds: options.clearStepBlockIds ?? vi.fn(),
		flushChunksNow: options.flushChunksNow ?? vi.fn(),
		updateSessionTitle: options.updateSessionTitle ?? vi.fn(),
		scheduleLoadSessions: options.scheduleLoadSessions ?? vi.fn(),
	});
}

describe('chat session lifecycle handler', () => {
	it('keeps media plans for the visible terminal session and clears inactive terminal plans', () => {
		const activeSessionId = 'ses-d186f90b4f33cc53ee287f9e8c401e6a';
		const inactiveSessionId = 'ses-ffe697a25c453d07bf0c9571521df2b7';
		mediaPlanStore.set({});
		const payload = (sessionId: string) => ({
			sessionId,
			stepNumber: 1,
			runId: 1,
			requestKind: 'chat' as const,
			strategy: 'text_only_safe' as const,
			projections: [],
			notices: [],
		});
		rememberMediaPlan(payload(activeSessionId));
		rememberMediaPlan(payload(inactiveSessionId));
		const onLifecycle = handler({
			activeSessionId,
			dispatchSession: vi.fn(),
		});

		onLifecycle({ payload: { type: 'completed', sessionId: activeSessionId } } as never);
		onLifecycle({ payload: { type: 'completed', sessionId: inactiveSessionId } } as never);
		expect(get(mediaPlanStore)[activeSessionId]).toHaveLength(1);
		expect(get(mediaPlanStore)[inactiveSessionId]).toBeUndefined();
		clearMediaPlans(null);
	});

	it('stops live bubbles when a session is paused without clearing its pending ask', () => {
		const flushChunksNow = vi.fn();
		const clearAskAwaiting = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ sessionId: 'ses-d7f8acbf674cce3d6ba47e40218ba936', status: 'running' }],
			activeSessionId: 'ses-d7f8acbf674cce3d6ba47e40218ba936',
			messages: {
				['ses-d7f8acbf674cce3d6ba47e40218ba936']: [
					{
						id: 'step-db57142e6c7eae1b75511b732e35f0db',
						role: 'assistant' as const,
						content: '半截回复',
						streaming: true,
					},
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
			activeSessionId: 'ses-d7f8acbf674cce3d6ba47e40218ba936',
			flushChunksNow,
			clearAskAwaiting,
			dispatchSession: (action) => reducer.dispatch(action),
		});
		setToolOutputPreview(
			'step-b1ff030bcda172afbbad69aa787379d5',
			'partial',
			'ses-d7f8acbf674cce3d6ba47e40218ba936',
		);

		onLifecycle({
			payload: {
				type: 'updated',
				sessionId: 'ses-d7f8acbf674cce3d6ba47e40218ba936',
				status: 'paused',
				waitingReason: 'ask',
				title: 'Question',
				reason: '用户主动打断输出',
			},
		} as never);

		expect(reducer.snapshot().sessions[0]).toMatchObject({
			status: 'paused',
			waitingReason: 'ask',
			title: 'Question',
		});
		expect(reducer.snapshot().runEndNotice).toEqual({
			sessionId: 'ses-d7f8acbf674cce3d6ba47e40218ba936',
			status: 'paused',
			reason: '用户主动打断输出',
		});
		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).not.toHaveBeenCalled();
		expect(
			get(getToolOutputPreviewStore('step-b1ff030bcda172afbbad69aa787379d5')),
		).toBeUndefined();
		expect(reducer.getMessages('ses-d7f8acbf674cce3d6ba47e40218ba936')).toEqual([
			{
				id: 'step-db57142e6c7eae1b75511b732e35f0db',
				role: 'assistant' as const,
				content: '半截回复',
				streaming: false,
			},
			{
				id: 'ask-1',
				type: 'ask',
				content: '还要继续吗？',
				awaiting: true,
				streaming: false,
			},
		]);
	});

	it('shows a successful turn-end notice immediately when waiting for the next user input', () => {
		const sessionId = 'ses-4672b4fe127c92b1420b7cc931b66f8e';
		const scheduleLoadSessions = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ sessionId, status: 'running' }],
			activeSessionId: sessionId,
		});
		const onLifecycle = handler({
			activeSessionId: sessionId,
			scheduleLoadSessions,
			dispatchSession: (action) => reducer.dispatch(action),
		});

		onLifecycle({
			payload: {
				type: 'updated',
				sessionId,
				status: 'paused',
				waitingReason: 'user_input',
				title: '研究',
				reason: null,
				runEndKind: 'turn_completed',
			},
		} as never);

		expect(reducer.snapshot().sessions[0]).toMatchObject({
			status: 'paused',
			waitingReason: 'user_input',
		});
		expect(reducer.snapshot().runEndNotice).toEqual({
			sessionId,
			status: 'paused',
			outcome: 'turn_completed',
			reason: '本轮已完成。',
		});
		expect(scheduleLoadSessions).toHaveBeenCalledOnce();
	});

	it('does not label an interaction pause as a normal turn completion', () => {
		const sessionId = 'ses-903687ec42798a55bbf2a67d03b4af3d';
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ sessionId, status: 'running' }],
			activeSessionId: sessionId,
		});
		const onLifecycle = handler({
			activeSessionId: sessionId,
			dispatchSession: (action) => reducer.dispatch(action),
		});

		onLifecycle({
			payload: {
				type: 'updated',
				sessionId,
				status: 'paused',
				waitingReason: 'ask',
				title: '研究',
				reason: null,
			},
		} as never);

		expect(reducer.snapshot().sessions[0].status).toBe('paused');
		expect(reducer.snapshot().runEndNotice).toBeNull();
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
				sessionId: 'ses-6bb3f11b1693b49cabe739510f5d9b02',
				status: 'pending',
				waitingReason: null,
				title: null,
			},
		} as never);

		expect(dispatchSession).toHaveBeenCalledWith({
			type: 'session/created',
			sessionId: 'ses-6bb3f11b1693b49cabe739510f5d9b02',
			freshStart: true,
			adoptedDraft: true,
			status: 'pending',
			title: null,
		});
	});

	it('projects a completion and performs terminal cleanup once from the same event', () => {
		const sessionId = 'ses-810b085358816d1bc83caf2eb4f45ff1';
		const flushChunksNow = vi.fn();
		const clearAskAwaiting = vi.fn();
		const evictTerminalSessionMemory = vi.fn();
		const clearStepBlockIds = vi.fn();
		const scheduleLoadSessions = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ sessionId, status: 'running', title: '旧标题' }],
			activeSessionId: sessionId,
			messages: {
				[sessionId]: [
					{
						id: 'step-fa7d9da560291aec4d4fd57e8812de10',
						role: 'assistant' as const,
						content: '最后一段',
						streaming: true,
					},
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
		setToolOutputPreview('step-fa7d9da560291aec4d4fd57e8812de10', 'partial', sessionId);

		onLifecycle({
			payload: {
				type: 'completed',
				sessionId,
				title: '研究',
				reason: '用户主动结束会话',
			},
		} as never);

		expect(reducer.snapshot().sessions[0]).toMatchObject({
			sessionId,
			status: 'completed',
			title: '研究',
		});
		expect(reducer.snapshot().runEndNotice).toEqual({
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
		expect(
			get(getToolOutputPreviewStore('step-fa7d9da560291aec4d4fd57e8812de10')),
		).toBeUndefined();
	});

	it('projects one error run-end notice with terminal status and cleanup', () => {
		const sessionId = 'ses-1a4ee0ce88355110073c1a46637b89bc';
		const clearAskAwaiting = vi.fn();
		const evictTerminalSessionMemory = vi.fn();
		const flushChunksNow = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ sessionId, status: 'running', title: '旧标题' }],
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

		expect(reducer.snapshot().sessions[0]).toMatchObject({
			status: 'error',
			title: '构建',
		});
		expect(reducer.snapshot().runEndNotice).toEqual({
			sessionId,
			status: 'error',
			reason: '网络请求超时',
		});
		expect(reducer.getSessionErrorReason(sessionId)).toBe('网络请求超时');
		expect(clearAskAwaiting).toHaveBeenCalledOnce();
		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(evictTerminalSessionMemory).toHaveBeenCalledOnce();
	});

	it('refreshes inactive terminal sessions after clearing their queued output', () => {
		const cleanupOrder: string[] = [];
		const onLifecycle = handler({
			activeSessionId: 'ses-d186f90b4f33cc53ee287f9e8c401e6a',
			flushChunksNow: () => cleanupOrder.push('flush'),
			evictTerminalSessionMemory: () => cleanupOrder.push('evict'),
			dispatchSession: vi.fn(),
		});
		setToolOutputPreview(
			'step-f5ba153deb417361f0e17158c2c3924a',
			'后台命令输出',
			'ses-ffe697a25c453d07bf0c9571521df2b7',
		);

		onLifecycle({
			payload: {
				type: 'error',
				sessionId: 'ses-ffe697a25c453d07bf0c9571521df2b7',
				title: '后台会话',
				error: '请求失败',
			},
		} as never);

		expect(cleanupOrder).toEqual(['flush', 'evict']);
		expect(
			get(getToolOutputPreviewStore('step-f5ba153deb417361f0e17158c2c3924a')),
		).toBeUndefined();
	});

	it('clears the resumed ask indicator on a pending update', () => {
		const clearAskAwaiting = vi.fn();
		const onLifecycle = handler({
			activeSessionId: 'ses-d186f90b4f33cc53ee287f9e8c401e6a',
			clearAskAwaiting,
			dispatchSession: vi.fn(),
		});

		onLifecycle({
			payload: {
				type: 'updated',
				sessionId: 'ses-d186f90b4f33cc53ee287f9e8c401e6a',
				status: 'pending',
				waitingReason: null,
				title: '研究',
				reason: null,
			},
		} as never);

		expect(clearAskAwaiting).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).toHaveBeenCalledWith('ses-d186f90b4f33cc53ee287f9e8c401e6a');
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
			payload: {
				type: 'title_updated',
				sessionId: 'ses-4f151e73abe298e48f40d72febb7be74',
				title: '新标题',
			},
		} as never);

		expect(updateSessionTitle).toHaveBeenCalledOnce();
		expect(updateSessionTitle).toHaveBeenCalledWith(
			'ses-4f151e73abe298e48f40d72febb7be74',
			'新标题',
		);
		expect(scheduleLoadSessions).toHaveBeenCalledOnce();
	});

	it('clears the deleted session projection from the same lifecycle event', () => {
		const sessionId = 'ses-ce69f619d8040910b44a763046f457f9';
		const flushChunksNow = vi.fn();
		const clearAskAwaiting = vi.fn();
		const clearStepBlockIds = vi.fn();
		const scheduleLoadSessions = vi.fn();
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ sessionId, status: 'paused', title: '删除目标' }],
			activeSessionId: sessionId,
			messages: {
				[sessionId]: [
					{
						id: 'msg-00000000000000000000000000000001',
						role: 'user' as const,
						content: '问题',
					},
				],
			},
		});
		const onLifecycle = handler({
			activeSessionId: sessionId,
			flushChunksNow,
			clearAskAwaiting,
			clearStepBlockIds,
			scheduleLoadSessions,
			dispatchSession: (action) => reducer.dispatch(action),
		});
		setToolOutputPreview('step-e98ef375faf65dc4add5e1667f51f7a6', 'partial', sessionId);

		onLifecycle({ payload: { type: 'deleted', sessionId } } as never);

		expect(reducer.snapshot().sessions).toEqual([]);
		expect(reducer.snapshot().activeSessionId).toBeNull();
		expect(reducer.getMessages(sessionId)).toEqual([]);
		expect(flushChunksNow).toHaveBeenCalledOnce();
		expect(clearAskAwaiting).toHaveBeenCalledWith(sessionId);
		expect(clearStepBlockIds).toHaveBeenCalledWith(sessionId);
		expect(scheduleLoadSessions).toHaveBeenCalledOnce();
		expect(
			get(getToolOutputPreviewStore('step-e98ef375faf65dc4add5e1667f51f7a6')),
		).toBeUndefined();
	});
});
