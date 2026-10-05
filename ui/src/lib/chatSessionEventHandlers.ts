import type {
	SessionErrorPayload,
	SessionLifecyclePayload,
	SessionDeletedPayload,
	SessionTitleUpdatedPayload,
} from './contracts/session.ts';
import type { TauriEvent } from './contracts/session.ts';
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
	/** Coalesced lifecycle refresh; explicit page actions use immediate refresh. */
	scheduleLoadSessions: () => void;
}

type LifecycleEvent = TauriEvent<SessionLifecyclePayload>;
type ErrorEvent = TauriEvent<SessionErrorPayload>;
type TitleUpdatedEvent = TauriEvent<SessionTitleUpdatedPayload>;
type DeletedEvent = TauriEvent<SessionDeletedPayload>;

/**
 * Build the session lifecycle handlers used by the chat route. The route
 * keeps reactive state and lifecycle callbacks; this module owns the event
 * ordering and terminal-session cleanup policy.
 */
export function createChatSessionEventHandlers({
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
}: ChatSessionEventContext): {
	'session:created': (event: LifecycleEvent) => void;
	'session:updated': (event: LifecycleEvent) => void;
	'session:completed': (event: LifecycleEvent) => void;
	'session:error': (event: ErrorEvent) => void;
	'session:title-updated': (event: TitleUpdatedEvent) => void;
	'session:deleted': (event: DeletedEvent) => void;
} {
	// The backend tags the two channels emitted for one terminal occurrence.
	// The first channel handled owns cleanup; an independent session:updated
	// without an identity continues to own its own cleanup.
	const terminalOccurrences = new Set<string>();
	const claimTerminalCleanup = (occurrenceId?: string) => {
		if (!occurrenceId) return true;
		const cleanupDone = terminalOccurrences.has(occurrenceId);
		terminalOccurrences.add(occurrenceId);
		// Keep only a bounded recent window for delayed duplicate deliveries.
		while (terminalOccurrences.size > 64) {
			const oldest = terminalOccurrences.values().next().value;
			if (oldest === undefined) break;
			terminalOccurrences.delete(oldest);
		}
		return !cleanupDone;
	};

	const finalizeLiveMessages = (sessionId: string) => {
		dispatchSession({ type: 'session/messages/finalized', sessionId });
	};

	return {
		'session:created': (event) => {
			const sessionId = event.payload.sessionId;
			if (sessionId) {
				// Voice input appends to the draft before the backend session exists.
				// Move it before any agent response can land in the new session.
				const adoptedDraft = adoptDraftMessages(sessionId);
				// The submission that requested a fresh session owns the selection;
				// this guard covers unrelated background-created sessions. When this
				// event adopts the pending draft, however, it is the submission's own
				// session and must be selected immediately: a fast response can emit
				// session:completed before process_transcript resolves.
				dispatchSession({
					type: 'session/created',
					sessionId,
					freshStart: isFreshSessionIntent(),
					adoptedDraft,
					status: event.payload.status,
					title: event.payload.title,
				});
			}
			scheduleLoadSessions();
		},
		'session:updated': (event) => {
			const data = event.payload;
			const shouldRunTerminalCleanup =
				(data.status === 'completed' || data.status === 'error') &&
				claimTerminalCleanup(data.occurrenceId);
			const activeSessionId = getActiveSessionId();
			const isActive = !!activeSessionId && data.sessionId === activeSessionId;
			const shouldForgetError =
				getSessionErrorId() === data.sessionId && isBusyStatus(data.status);
			// A resume (pending) means the user's answer was received. The ask
			// pause itself is reported as paused and must not clear the indicator.
			if (isActive && data.status === 'pending') {
				clearAskAwaiting(data.sessionId);
			} else if (
				isActive &&
				shouldRunTerminalCleanup &&
				(data.status === 'completed' || data.status === 'error')
			) {
				// The first terminal channel owns cleanup, including when the
				// secondary session:updated event arrives without its primary event.
				clearAskAwaiting(data.sessionId);
			}
			dispatchSession({
				type: 'session/status-updated',
				sessionId: data.sessionId,
				status: data.status,
				title: data.title,
				waitingReason: data.waitingReason,
			});
			if (
				(data.status === 'paused' ||
					data.status === 'completed' ||
					data.status === 'error') &&
				data.reason?.trim()
			) {
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
				// Pausing or interrupting preserves the partial text for resume, but
				// first flush queued chunks and stop its live preview/caret.
				clearToolOutputPreviewsForSession(data.sessionId);
				flushChunksNow();
				finalizeLiveMessages(data.sessionId);
			}
			if (data.status === 'completed' || data.status === 'error') {
				if (shouldRunTerminalCleanup) {
					clearToolOutputPreviewsForSession(data.sessionId);
					// Flush even for an inactive session: its queued RAF chunks must
					// land before eviction, or a later frame can recreate its messages.
					flushChunksNow();
					evictTerminalSessionMemory(data.sessionId);
					if (getActiveSessionId() === data.sessionId) {
						finalizeLiveMessages(data.sessionId);
					}
					clearStepBlockIds(data.sessionId);
				}
			}
			if (
				shouldRunTerminalCleanup ||
				(data.status !== 'completed' && data.status !== 'error')
			) {
				scheduleLoadSessions();
			}
		},
		'session:completed': (event) => {
			const sessionId = event.payload.sessionId;
			const reason = event.payload.reason?.trim() || '会话已正常结束。';
			const shouldRunTerminalCleanup = claimTerminalCleanup(event.payload.occurrenceId);
			dispatchSession({
				type: 'session/status-updated',
				sessionId,
				status: 'completed',
				title: event.payload.title,
				waitingReason: null,
			});
			dispatchSession({
				type: 'session/termination-shown',
				sessionId,
				status: 'completed',
				reason,
			});
			if (shouldRunTerminalCleanup && getActiveSessionId() === sessionId) {
				clearAskAwaiting(sessionId);
			}
			if (shouldRunTerminalCleanup) {
				clearToolOutputPreviewsForSession(sessionId);
				// Flush even for an inactive session before dropping its in-memory
				// transcript so a queued frame cannot resurrect terminal messages.
				flushChunksNow();
				if (getActiveSessionId() === sessionId) finalizeLiveMessages(sessionId);
				evictTerminalSessionMemory(sessionId);
				clearStepBlockIds(sessionId);
				scheduleLoadSessions();
			}
		},
		'session:error': (event) => {
			const { sessionId, error } = event.payload;
			const shouldRunTerminalCleanup = claimTerminalCleanup(event.payload.occurrenceId);
			dispatchSession({ type: 'session/error-shown', sessionId, reason: error });
			dispatchSession({
				type: 'session/error-reason-remembered',
				sessionId,
				reason: error,
			});
			if (shouldRunTerminalCleanup && sessionId === getActiveSessionId()) {
				clearAskAwaiting(sessionId);
			}
			if (shouldRunTerminalCleanup) {
				clearToolOutputPreviewsForSession(sessionId);
				// Drain background-session chunks before evicting the transcript.
				flushChunksNow();
				if (getActiveSessionId() === sessionId) finalizeLiveMessages(sessionId);
				evictTerminalSessionMemory(sessionId);
				clearStepBlockIds(sessionId);
				scheduleLoadSessions();
			}
		},
		'session:title-updated': (event) => {
			const { sessionId, title } = event.payload;
			updateSessionTitle(sessionId, title);
		},
		'session:deleted': (event) => {
			const sessionId = event.payload.sessionId;
			// Drain and cancel the renderer frame before deleting projected state;
			// otherwise the queued callback can recreate the messages afterward.
			flushChunksNow();
			clearAskAwaiting(sessionId);
			clearToolOutputPreviewsForSession(sessionId);
			clearMediaPlans(sessionId);
			clearStepBlockIds(sessionId);
			dispatchSession({ type: 'session/deleted', sessionId });
			scheduleLoadSessions();
		},
	};
}
