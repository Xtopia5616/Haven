import { describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { createChatSessionEventHandlers } from './chatSessionEventHandlers.ts';
import { initialSessionState, SessionReducer } from './sessionReducer.ts';
import {
	getToolOutputPreviewStore,
	setToolOutputPreview,
} from './toolOutputPreviewStore.ts';

function handlers(options: {
	fresh?: boolean;
	adoptedDraft?: boolean;
	activeSessionId?: string | null;
	dispatchSession: (action: import('./sessionReducer.ts').SessionAction) => void;
	flushChunksNow?: () => void;
	clearAskAwaiting?: (sessionId: string | null) => void;
	evictTerminalSessionMemory?: (sessionId: string) => void;
	clearStepBlockIds?: (sessionId: string | null) => void;
	scheduleLoadSessions?: () => void;
}) {
	return createChatSessionEventHandlers({
		getActiveSessionId: () => options.activeSessionId ?? null,
		isFreshSessionIntent: () => options.fresh ?? false,
		adoptDraftMessages: () => options.adoptedDraft ?? false,
		dispatchSession: options.dispatchSession,
		getSessionErrorId: () => null,
		clearAskAwaiting: options.clearAskAwaiting ?? vi.fn(),
		evictTerminalSessionMemory: options.evictTerminalSessionMemory ?? vi.fn(),
		clearStepBlockIds: options.clearStepBlockIds ?? vi.fn(),
		flushChunksNow: options.flushChunksNow ?? vi.fn(),
		updateSessionTitle: vi.fn(),
		scheduleLoadSessions: options.scheduleLoadSessions ?? vi.fn(),
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
			dispatchSession: (action) => reducer.dispatch(action),
		});
		setToolOutputPreview('step-shell', 'partial', 'ses-paused');

		eventHandlers['session:updated']({
			payload: { sessionId: 'ses-paused', status, title: null, reason: null },
		} as never);

		expect(flushChunksNow).toHaveBeenCalledOnce();
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
		const eventHandlers = handlers({
			fresh: true,
			adoptedDraft: true,
			dispatchSession,
		});

		eventHandlers['session:created']({
			payload: { sessionId: 'ses-fast', status: 'pending', title: null, reason: null },
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
			payload: { sessionId: 'ses-background', status: 'pending', title: null, reason: null },
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
		const reducer = new SessionReducer();
		const dispatchSession = vi.fn((action: import('./sessionReducer.ts').SessionAction) =>
			reducer.dispatch(action),
		);
		const eventHandlers = handlers({ activeSessionId: 'ses-error', dispatchSession });

		eventHandlers['session:error']({
			payload: { sessionId: 'ses-error', error: '网络请求超时' },
		} as never);

		expect(dispatchSession).toHaveBeenCalledWith({
			type: 'session/error-shown',
			sessionId: 'ses-error',
			reason: '网络请求超时',
		});
		expect(reducer.getSessionErrorReason('ses-error')).toBe('网络请求超时');
	});

	it('shows the reason for a normally completed active session', () => {
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ id: 'ses-done', status: 'running', title: '研究' }],
			activeSessionId: 'ses-done',
		});
		const eventHandlers = handlers({
			activeSessionId: 'ses-done',
			dispatchSession: (action) => reducer.dispatch(action),
		});

		eventHandlers['session:completed']({
			payload: {
				sessionId: 'ses-done',
				status: 'completed',
				title: '研究',
				reason: '用户主动结束会话',
			},
		} as never);

		expect(reducer.getState().termination).toEqual({
			sessionId: 'ses-done',
			status: 'completed',
			reason: '用户主动结束会话',
		});
	});

	it('shows the reason for an explicitly interrupted active session', () => {
		const reducer = new SessionReducer({
			...initialSessionState,
			sessions: [{ id: 'ses-paused', status: 'running', title: '研究' }],
			activeSessionId: 'ses-paused',
		});
		const eventHandlers = handlers({
			activeSessionId: 'ses-paused',
			dispatchSession: (action) => reducer.dispatch(action),
		});

		eventHandlers['session:updated']({
			payload: {
				sessionId: 'ses-paused',
				status: 'paused',
				title: null,
				reason: '用户主动打断输出',
			},
		} as never);

		expect(reducer.getState().termination).toEqual({
			sessionId: 'ses-paused',
			status: 'paused',
			reason: '用户主动打断输出',
		});
	});

	it.each(['completed', 'error'] as const)(
		'keeps the terminal UI projection when the primary %s event is followed by session:updated',
		(status) => {
			const sessionId = `ses-terminal-${status}`;
			const stepId = `step-terminal-${status}`;
			const reason = status === 'completed' ? '用户主动结束会话' : '网络请求超时';
			const reducer = new SessionReducer({
				...initialSessionState,
				sessions: [{ id: sessionId, status: 'running', title: '研究' }],
				activeSessionId: sessionId,
				messages: {
					[sessionId]: [
						{ id: stepId, role: 'assistant', content: '最后一段', streaming: true },
					],
				},
			});
			const clearAskAwaiting = vi.fn();
			const evictTerminalSessionMemory = vi.fn();
			const clearStepBlockIds = vi.fn();
			const scheduleLoadSessions = vi.fn();
			const flushChunksNow = vi.fn();
			const notifications = vi.fn();
			const eventHandlers = handlers({
				activeSessionId: sessionId,
				flushChunksNow,
				clearAskAwaiting,
				evictTerminalSessionMemory,
				clearStepBlockIds,
				scheduleLoadSessions,
				dispatchSession: (action) => reducer.dispatch(action),
			});
			reducer.subscribe(notifications);
			setToolOutputPreview(stepId, '工具输出', sessionId);

			if (status === 'completed') {
				eventHandlers['session:completed']({
					payload: {
						sessionId,
						status,
						title: '研究',
						reason,
						occurrenceId: 'occ-paired',
					},
				} as never);
			} else {
				eventHandlers['session:error']({
					payload: { sessionId, error: reason, occurrenceId: 'occ-paired' },
				} as never);
			}
			const notificationsAfterPrimary = notifications.mock.calls.length;
			eventHandlers['session:updated']({
				payload: { sessionId, status, title: '研究', reason, occurrenceId: 'occ-paired' },
			} as never);

			expect(flushChunksNow).toHaveBeenCalledOnce();
			expect(clearAskAwaiting).toHaveBeenCalledOnce();
			expect(evictTerminalSessionMemory).toHaveBeenCalledOnce();
			expect(clearStepBlockIds).toHaveBeenCalledOnce();
			expect(scheduleLoadSessions).toHaveBeenCalledOnce();
			expect(notifications).toHaveBeenCalledTimes(notificationsAfterPrimary);
			expect(reducer.getState().sessions[0].status).toBe(status);
			expect(reducer.getState().termination).toEqual({ sessionId, status, reason });
			expect(reducer.getMessages(sessionId)[0].streaming).toBe(false);
			expect(get(getToolOutputPreviewStore(stepId))).toBeUndefined();
			expect(reducer.getState().error).toEqual(
				status === 'error' ? { sessionId, reason } : null,
			);
		},
	);

	it.each(['completed', 'error'] as const)(
		'clears tool previews for an inactive session on the primary terminal event',
		(status) => {
			const sessionId = `ses-background-terminal-${status}`;
			const stepId = `step-background-terminal-${status}`;
			const reducer = new SessionReducer({
				...initialSessionState,
				sessions: [
					{ id: sessionId, status: 'running', title: '后台会话' },
					{ id: 'ses-active', status: 'running', title: '当前会话' },
				],
				activeSessionId: 'ses-active',
			});
			const cleanupOrder: string[] = [];
			const eventHandlers = handlers({
				activeSessionId: 'ses-active',
				flushChunksNow: () => cleanupOrder.push('flush'),
				evictTerminalSessionMemory: () => cleanupOrder.push('evict'),
				dispatchSession: (action) => reducer.dispatch(action),
			});
			setToolOutputPreview(stepId, '后台命令输出', sessionId);

			if (status === 'completed') {
				eventHandlers['session:completed']({
					payload: {
						sessionId,
						status,
						title: '后台会话',
						reason: '已完成',
						occurrenceId: 'occ-background-terminal',
					},
				} as never);
			} else {
				eventHandlers['session:error']({
					payload: {
						sessionId,
						error: '请求失败',
						occurrenceId: 'occ-background-terminal',
					},
				} as never);
			}
			eventHandlers['session:updated']({
				payload: {
					sessionId,
					status,
					title: '后台会话',
					reason: status === 'completed' ? '已完成' : '请求失败',
					occurrenceId: 'occ-background-terminal',
				},
			} as never);

			expect(cleanupOrder).toEqual(['flush', 'evict']);
			expect(get(getToolOutputPreviewStore(stepId))).toBeUndefined();
		},
	);

	it.each(['completed', 'error'] as const)(
		'lets a standalone session:updated %s event clean up live messages',
		(status) => {
			const sessionId = `ses-standalone-${status}`;
			const reducer = new SessionReducer({
				...initialSessionState,
				sessions: [{ id: sessionId, status: 'running' }],
				activeSessionId: sessionId,
				messages: {
					[sessionId]: [
						{ id: 'step-live', role: 'assistant', content: '处理中', streaming: true },
					],
				},
			});
			const eventHandlers = handlers({
				activeSessionId: sessionId,
				dispatchSession: (action) => reducer.dispatch(action),
			});

			eventHandlers['session:updated']({
				payload: {
					sessionId,
					status,
					title: null,
					reason: '状态切换的解释',
				},
			} as never);

			expect(reducer.getState().sessions[0].status).toBe(status);
			expect(reducer.getMessages(sessionId)[0].streaming).toBe(false);
		},
	);

	it.each(['completed', 'error'] as const)(
		'runs paired %s cleanup once when session:updated arrives first',
		(status) => {
			const sessionId = `ses-reordered-${status}`;
			const reason = status === 'completed' ? '已结束' : '请求失败';
			const reducer = new SessionReducer({
				...initialSessionState,
				sessions: [{ id: sessionId, status: 'running' }],
				activeSessionId: sessionId,
				messages: {
					[sessionId]: [
						{
							id: 'step-reordered',
							role: 'assistant',
							content: '处理中',
							streaming: true,
						},
					],
				},
			});
			const clearAskAwaiting = vi.fn();
			const evictTerminalSessionMemory = vi.fn();
			const clearStepBlockIds = vi.fn();
			const scheduleLoadSessions = vi.fn();
			const flushChunksNow = vi.fn();
			const notifications = vi.fn();
			const eventHandlers = handlers({
				activeSessionId: sessionId,
				clearAskAwaiting,
				evictTerminalSessionMemory,
				clearStepBlockIds,
				scheduleLoadSessions,
				flushChunksNow,
				dispatchSession: (action) => reducer.dispatch(action),
			});
			reducer.subscribe(notifications);

			eventHandlers['session:updated']({
				payload: {
					sessionId,
					status,
					title: null,
					reason,
					waitingReason: null,
					occurrenceId: 'occ-reordered',
				},
			} as never);
			const notificationsAfterSecondary = notifications.mock.calls.length;
			if (status === 'completed') {
				eventHandlers['session:completed']({
					payload: {
						sessionId,
						status,
						title: null,
						reason,
						occurrenceId: 'occ-reordered',
					},
				} as never);
			} else {
				eventHandlers['session:error']({
					payload: { sessionId, error: reason, occurrenceId: 'occ-reordered' },
				} as never);
			}

			expect(flushChunksNow).toHaveBeenCalledOnce();
			expect(evictTerminalSessionMemory).toHaveBeenCalledOnce();
			expect(clearStepBlockIds).toHaveBeenCalledOnce();
			expect(scheduleLoadSessions).toHaveBeenCalledOnce();
			expect(clearAskAwaiting).toHaveBeenCalledOnce();
			if (status === 'completed') {
				expect(notifications).toHaveBeenCalledTimes(notificationsAfterSecondary);
			} else {
				// The primary adds its distinct per-session error-reason projection.
				expect(notifications).toHaveBeenCalledTimes(notificationsAfterSecondary + 1);
			}
			expect(reducer.getMessages(sessionId)[0].streaming).toBe(false);
			expect(reducer.getState().termination).toEqual({ sessionId, status, reason });
		},
	);
});
