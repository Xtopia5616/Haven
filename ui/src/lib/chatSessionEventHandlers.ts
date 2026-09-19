import type {
	SessionErrorPayload,
	SessionLifecyclePayload,
	SessionDeletedPayload,
	SessionTitleUpdatedPayload,
} from './contracts/session.ts';
import type { TauriEvent } from './contracts/session.ts';
import { isBusyStatus, isPausedStatus } from './sessionStatus.ts';
import type { SessionAction } from './sessionReducer.ts';

interface ChatSessionEventContext {
	getActiveSessionId: () => string | null;
	isFreshSessionIntent: () => boolean;
	adoptDraftMessages: (sessionId: string) => boolean;
	dispatchSession: (action: SessionAction) => void;
	getSessionErrorId: () => string | null;
	rememberSessionError: (sessionId: string, reason: string) => void;
	forgetSessionError: (sessionId: string) => void;
	clearAskAwaiting: (sessionId: string) => void;
	evictTerminalSessionMemory: (sessionId: string) => void;
	clearStepBlockIds: (sessionId: string) => void;
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
	rememberSessionError,
	forgetSessionError,
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
	const finalizeLiveMessages = (sessionId: string) => {
		// A lifecycle event can arrive while the last chunks are still queued for
		// the next animation frame. Flush first, otherwise that frame can recreate
		// a streaming bubble (and its blinking caret) after this cleanup.
		flushChunksNow();
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
			const activeSessionId = getActiveSessionId();
			const isActive = !!activeSessionId && data.sessionId === activeSessionId;
			const shouldForgetError =
				getSessionErrorId() === data.sessionId && isBusyStatus(data.status);
			// A resume (pending) means the user's answer was received. The ask
			// pause itself is reported as paused and must not clear the indicator.
			if (isActive && data.status === 'pending') {
				clearAskAwaiting(data.sessionId);
			}
			dispatchSession({
				type: 'session/status-updated',
				sessionId: data.sessionId,
				status: data.status,
				title: data.title,
			});
			if (shouldForgetError) {
				forgetSessionError(data.sessionId);
			}
			if (isPausedStatus(data.status)) {
				// Pausing or interrupting preserves the partial text for resume, but
				// it is no longer live output in the UI, so its caret must stop.
				finalizeLiveMessages(data.sessionId);
			}
			if (data.status === 'completed' || data.status === 'error') {
				evictTerminalSessionMemory(data.sessionId);
				if (getActiveSessionId() === data.sessionId) {
					finalizeLiveMessages(data.sessionId);
				}
				clearStepBlockIds(data.sessionId);
			}
			scheduleLoadSessions();
		},
		'session:completed': (event) => {
			const sessionId = event.payload.sessionId;
			dispatchSession({
				type: 'session/status-updated',
				sessionId,
				status: event.payload.status,
				title: event.payload.title,
			});
			if (getActiveSessionId() === sessionId) {
				clearAskAwaiting(sessionId);
				finalizeLiveMessages(sessionId);
			}
			evictTerminalSessionMemory(sessionId);
			clearStepBlockIds(sessionId);
			scheduleLoadSessions();
		},
		'session:error': (event) => {
			const { sessionId, error } = event.payload;
			dispatchSession({ type: 'session/error-shown', sessionId, reason: error });
			rememberSessionError(sessionId, error);
			if (sessionId === getActiveSessionId()) {
				clearAskAwaiting(sessionId);
				finalizeLiveMessages(sessionId);
			}
			evictTerminalSessionMemory(sessionId);
			clearStepBlockIds(sessionId);
			scheduleLoadSessions();
		},
		'session:title-updated': (event) => {
			const { sessionId, title } = event.payload;
			updateSessionTitle(sessionId, title);
		},
		'session:deleted': (event) => {
			dispatchSession({ type: 'session/deleted', sessionId: event.payload.sessionId });
			scheduleLoadSessions();
		},
	};
}
