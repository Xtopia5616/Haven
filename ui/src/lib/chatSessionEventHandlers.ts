import type { SessionLifecyclePayload } from './contracts/session.ts';
import type { TauriEvent } from './contracts/tauriEvent.ts';
import { isBusyStatus, isPausedStatus } from './sessionStatus.ts';
import type { SessionAction } from './sessionReducer.ts';
import { clearMediaPlans } from './mediaPlanStore.ts';
import { clearToolOutputPreviewsForSession } from './toolOutputPreviewStore.ts';

export interface ChatSessionEventContext {
	getActiveSessionId: () => string | null;
	isFreshSessionIntent: () => boolean;
	adoptDraftMessages: (sessionId: string) => boolean;
	dispatchSession: (action: SessionAction) => void;
	getSessionErrorId: () => string | null;
	clearAskAwaiting: (sessionId: string | null) => void;
	evictTerminalSessionMemory: (sessionId: string) => void;
	clearStepBlockIds: (sessionId: string | null) => void;
	/** Flush the RAF-batched stream before lifecycle cleanup changes its state. */
	flushChunksNow: () => void;
	updateSessionTitle: (sessionId: string, title: string) => void;
	/** Coalesced lifecycle refresh; explicit page requests use immediate refresh. */
	scheduleLoadSessions: () => void;
}

type LifecycleEvent = TauriEvent<SessionLifecyclePayload>;

/** Build the single session lifecycle handler used by the chat route. */
export function createChatSessionEventHandler({
	getActiveSessionId,
	isFreshSessionIntent,
	adoptDraftMessages,
	dispatchSession,
	getSessionErrorId,
	clearAskAwaiting,
	evictTerminalSessionMemory,
	clearStepBlockIds,
	flushChunksNow,
	updateSessionTitle,
	scheduleLoadSessions,
}: ChatSessionEventContext): (event: LifecycleEvent) => void {
	const finalizeLiveMessages = (sessionId: string) => {
		dispatchSession({ type: 'session/messages/finalized', sessionId });
	};

	const cleanupTerminalSession = (sessionId: string) => {
		clearToolOutputPreviewsForSession(sessionId);
		// Drain queued stream chunks before dropping the transcript. A later RAF
		// must not recreate terminal messages after session memory is evicted.
		flushChunksNow();
		if (getActiveSessionId() === sessionId) finalizeLiveMessages(sessionId);
		evictTerminalSessionMemory(sessionId);
		clearStepBlockIds(sessionId);
		scheduleLoadSessions();
	};

	return (event) => {
		const data = event.payload;
		switch (data.type) {
			case 'created': {
				const sessionId = data.sessionId;
				if (sessionId) {
					// Voice input appends to the draft before the backend session exists.
					// Move it before any response can land in the new session.
					const adoptedDraft = adoptDraftMessages(sessionId);
					// A fast response can complete before process_transcript resolves, so
					// the session that adopts the pending draft must be selected now.
					dispatchSession({
						type: 'session/created',
						sessionId,
						freshStart: isFreshSessionIntent(),
						adoptedDraft,
						status: data.status,
						title: data.title,
					});
				}
				scheduleLoadSessions();
				return;
			}
			case 'updated': {
				const isActive = getActiveSessionId() === data.sessionId;
				const shouldForgetError =
					getSessionErrorId() === data.sessionId && isBusyStatus(data.status);
				// A resume (pending) means the user's answer was received. The ask
				// pause itself is reported as paused and must not clear the indicator.
				if (isActive && data.status === 'pending') clearAskAwaiting(data.sessionId);

				dispatchSession({
					type: 'session/status-updated',
					sessionId: data.sessionId,
					status: data.status,
					title: data.title,
					waitingReason: data.waitingReason,
				});
				if (data.status === 'paused' && data.reason?.trim()) {
					dispatchSession({
						type: 'session/termination-shown',
						sessionId: data.sessionId,
						status: data.status,
						reason: data.reason,
					});
				}
				if (shouldForgetError) {
					dispatchSession({
						type: 'session/error-reason-forgotten',
						sessionId: data.sessionId,
					});
				}
				if (isPausedStatus(data.status)) {
					clearToolOutputPreviewsForSession(data.sessionId);
					flushChunksNow();
					finalizeLiveMessages(data.sessionId);
				}
				scheduleLoadSessions();
				return;
			}
			case 'completed': {
				const reason = data.reason?.trim() || '会话已正常结束。';
				dispatchSession({
					type: 'session/status-updated',
					sessionId: data.sessionId,
					status: 'completed',
					title: data.title,
					waitingReason: null,
				});
				dispatchSession({
					type: 'session/termination-shown',
					sessionId: data.sessionId,
					status: 'completed',
					reason,
				});
				if (getActiveSessionId() === data.sessionId) clearAskAwaiting(data.sessionId);
				cleanupTerminalSession(data.sessionId);
				return;
			}
			case 'error': {
				const reason = data.error?.trim() || '本次会话因错误停止，暂未收到更具体的原因。';
				dispatchSession({
					type: 'session/status-updated',
					sessionId: data.sessionId,
					status: 'error',
					title: data.title,
					waitingReason: null,
				});
				dispatchSession({ type: 'session/error-shown', sessionId: data.sessionId, reason });
				dispatchSession({
					type: 'session/error-reason-remembered',
					sessionId: data.sessionId,
					reason,
				});
				if (getActiveSessionId() === data.sessionId) clearAskAwaiting(data.sessionId);
				cleanupTerminalSession(data.sessionId);
				return;
			}
			case 'title_updated':
				updateSessionTitle(data.sessionId, data.title);
				scheduleLoadSessions();
				return;
			case 'deleted':
				// Drain queued frames before removing the projected transcript.
				flushChunksNow();
				clearAskAwaiting(data.sessionId);
				clearToolOutputPreviewsForSession(data.sessionId);
				clearMediaPlans(data.sessionId);
				clearStepBlockIds(data.sessionId);
				dispatchSession({ type: 'session/deleted', sessionId: data.sessionId });
				scheduleLoadSessions();
				return;
		}
	};
}
