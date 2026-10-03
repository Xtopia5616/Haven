import { buildResumeMessages } from './resumeMessages.ts';
import { createSessionRefreshScheduler } from './sessionRefresh.ts';
import { isErrorStatus } from './sessionStatus.ts';
import { resumeInteractions } from './sessionReducer.ts';
import type { SessionAction, SessionReducer } from './sessionReducer.ts';
import type { SessionListResponse, SessionResumeResponse } from './contracts/sessionHistory.ts';
import type { ResumeTarget } from './sessionIntentStore.ts';

export interface ChatSessionStartupDependencies {
	reducer: SessionReducer;
	dispatch: (action: SessionAction) => void;
	getSessions: () => Promise<SessionListResponse>;
	getLastConversation: () => Promise<SessionResumeResponse | null>;
	reopenSession: (request: { sessionId: string }) => Promise<void>;
	refreshActions: () => void;
	getFreshSessionIntent: () => boolean;
	setFreshSessionIntent: (value: boolean) => void;
	hasPersistedFreshSessionIntent: () => boolean;
	clearPersistedFreshSessionIntent: () => void;
	getPendingInteractionIds: (sessionId: string) => string[];
	evictTerminalSessionMemory: (sessionId: string) => void;
	setInitialLoading: (loading: boolean) => void;
	deferResumeTargetClear: () => void;
	warn: (message: string, error: unknown) => void;
	reportError: (error: unknown, options: { context: string; message: string; log: boolean }) => unknown;
}

/**
 * Own chat startup and session restore ordering separately from live session
 * commands. The route supplies store, notification, and view callbacks.
 */
export function createChatSessionStartup(dependencies: ChatSessionStartupDependencies) {
	let loadSessionsSeq = 0;
	let loadSessionsSettled: Promise<void> = Promise.resolve();
	let generation = 0;
	let disposed = false;

	function isCurrentGeneration(expectedGeneration: number) {
		return !disposed && generation === expectedGeneration;
	}

	const loadSessionsRefresh = createSessionRefreshScheduler(() => loadSessionsNow());

	function retainErroredSession(resumeTarget: Pick<ResumeTarget, 'sessionId' | 'summary' | 'title'>) {
		dependencies.dispatch({
			type: 'session/retained-error',
			session: {
				id: resumeTarget.sessionId,
				input: resumeTarget.summary || '',
				input_text: resumeTarget.summary || '',
				title: resumeTarget.title || null,
				status: 'error',
			},
		});
	}

	function hydrateFreshSessionIntent() {
		if (disposed) return;
		if (dependencies.hasPersistedFreshSessionIntent()) {
			dependencies.setFreshSessionIntent(true);
		}
	}

	function processResumeTarget(resumeTarget: ResumeTarget | null) {
		if (!disposed && resumeTarget?.sessionId) {
			// An explicit history choice cancels a pending fresh-start intent.
			dependencies.setFreshSessionIntent(false);
			dependencies.clearPersistedFreshSessionIntent();
			const prevActive = dependencies.reducer.getState().activeSessionId;
			dependencies.dispatch({ type: 'session/selected', sessionId: resumeTarget.sessionId });
			if (prevActive && prevActive !== resumeTarget.sessionId) {
				const prevSession = dependencies.reducer
					.getState()
					.sessions.find((session) => session.id === prevActive);
				if (
					!prevSession ||
					prevSession.status === 'completed' ||
					prevSession.status === 'error'
				) {
					dependencies.evictTerminalSessionMemory(prevActive);
				}
			}
			if (resumeTarget.wasError) {
				dependencies.dispatch({
					type: 'session/error-shown',
					sessionId: resumeTarget.sessionId,
					reason:
						resumeTarget.errorReason ||
						dependencies.reducer.getSessionErrorReason(resumeTarget.sessionId) ||
						'本次会话因错误停止，暂未收到更具体的原因。',
				});
				retainErroredSession(resumeTarget);
			}
			// Keep the target alive through the current mount's initialization.
			dependencies.deferResumeTargetClear();
			if (!resumeTarget.wasError) {
				// Opening history can rehydrate a terminal session into the live
				// paused-session projection. That memory-only transition emits no
				// lifecycle event, so refresh the switcher immediately instead of
				// waiting for the next page mount or manual refresh.
				void loadSessions();
			}
		}
	}

	async function loadSessionsNow(): Promise<void> {
		if (disposed) return;
		const requestGeneration = generation;
		const seq = ++loadSessionsSeq;
		const run = (async () => {
			const result = await dependencies.getSessions();
			if (!isCurrentGeneration(requestGeneration) || seq !== loadSessionsSeq) return;
			if (result && result.sessions) {
				const before = dependencies.reducer.getState();
				dependencies.dispatch({
					type: 'sessions/loaded',
					sessions: result.sessions.map((session) => ({
						...session,
						waitingReason: session.waiting_reason ?? null,
					})),
					autoSelect: !before.activeSessionId && !dependencies.getFreshSessionIntent(),
				});
				const after = dependencies.reducer.getState();
				if (
					after.activeSessionId &&
					!after.sessions.some((session) => session.id === after.activeSessionId) &&
					!after.error &&
					!after.termination
				) {
					dependencies.dispatch({ type: 'session/cleared' });
				}
			}
			if (!isCurrentGeneration(requestGeneration)) return;
			// Lifecycle changes can reap background or scheduled actions without a
			// matching action-board terminal event.
			dependencies.refreshActions();
		})().catch((error: unknown) => {
			if (!isCurrentGeneration(requestGeneration)) return;
			dependencies.reportError(error, {
				context: '+page',
				message: '加载会话列表失败',
				log: false,
			});
		});
		loadSessionsSettled = run;
		return run;
	}

	/** Immediate refresh for explicit actions; lifecycle bursts share the scheduler. */
	function loadSessions(): Promise<void> {
		if (disposed) return Promise.resolve();
		const run = loadSessionsRefresh.refresh();
		loadSessionsSettled = run;
		return run;
	}

	function scheduleLoadSessions() {
		if (disposed) return;
		loadSessionsRefresh.schedule();
	}

	async function restoreLastConversation(resumeTarget: ResumeTarget | null): Promise<void> {
		const requestGeneration = generation;
		if (!isCurrentGeneration(requestGeneration)) return;
		if (
			resumeTarget ||
			dependencies.getFreshSessionIntent() ||
			dependencies.hasPersistedFreshSessionIntent()
		) {
			return;
		}
		await loadSessionsSettled;
		if (!isCurrentGeneration(requestGeneration)) return;
		const current = dependencies.reducer.getState();
		if (
			current.activeSessionId &&
			!current.sessions.some((session) => session.id === current.activeSessionId)
		) {
			dependencies.dispatch({ type: 'session/cleared' });
		}
		if (dependencies.reducer.getState().activeSessionId) return;

		let last: SessionResumeResponse | null;
		try {
			last = await dependencies.getLastConversation();
		} catch (error) {
			if (!isCurrentGeneration(requestGeneration)) return;
			dependencies.warn('auto-restore conversation error', error);
			return;
		}
		if (!isCurrentGeneration(requestGeneration)) return;
		if (
			!last?.session ||
			dependencies.reducer.getState().activeSessionId ||
			dependencies.getFreshSessionIntent()
		) {
			return;
		}
		// A completed conversation is history and should not be resurrected.
		if (last.session.status === 'completed') return;

		const wasError = isErrorStatus(last.session.status);
		dependencies.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: last.session.id,
			messages: buildResumeMessages(last),
			interactions: resumeInteractions(last),
			preserveInteractionIds: dependencies.getPendingInteractionIds(last.session.id),
			usage: last.usage,
			llmUsage: last.llm_usage,
		});
		dependencies.dispatch({ type: 'session/selected', sessionId: last.session.id });
		if (wasError) {
			dependencies.dispatch({
				type: 'session/error-shown',
				sessionId: last.session.id,
				reason:
					dependencies.reducer.getSessionErrorReason(last.session.id) ||
					'本次会话因错误停止，暂未收到更具体的原因。',
			});
			retainErroredSession({
				sessionId: last.session.id,
				summary: last.session.input_text,
				title: last.session.title,
			});
		}
		try {
			if (!wasError) await dependencies.reopenSession({ sessionId: last.session.id });
		} catch (error) {
			if (!isCurrentGeneration(requestGeneration)) return;
			dependencies.warn('reopen_session error', error);
		}
		if (!isCurrentGeneration(requestGeneration)) return;
		await loadSessions();
	}

	async function loadInitialSessions(resumeTarget: ResumeTarget | null): Promise<void> {
		if (disposed) return;
		const requestGeneration = generation;
		const sessionsPromise = loadSessions();
		const restorePromise = restoreLastConversation(resumeTarget);
		try {
			await Promise.all([sessionsPromise, restorePromise]);
		} finally {
			if (isCurrentGeneration(requestGeneration)) dependencies.setInitialLoading(false);
		}
	}

	function dispose() {
		if (disposed) return;
		disposed = true;
		generation += 1;
		loadSessionsSeq += 1;
		loadSessionsRefresh.dispose();
	}

	return {
		dispose,
		hydrateFreshSessionIntent,
		loadInitialSessions,
		loadSessions,
		processResumeTarget,
		scheduleLoadSessions,
	};
}
