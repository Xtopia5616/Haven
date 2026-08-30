import type {
	SessionErrorPayload,
	SessionLifecyclePayload,
	SessionTitleUpdatedPayload,
} from './contracts/session.ts';
import type { TauriEvent } from './contracts/session.ts';
import { isBusyStatus } from './sessionStatus.ts';
import { updateSessionMessages } from './sessionMessages.ts';

interface ChatSessionEventContext {
	getActiveSessionId: () => string | null;
	isFreshSessionIntent: () => boolean;
	adoptDraftMessages: (sessionId: string) => void;
	setActiveSessionId: (sessionId: string) => void;
	getSessionErrorId: () => string | null;
	clearSessionError: () => void;
	showSessionError: (sessionId: string) => void;
	clearAskAwaiting: (sessionId: string) => void;
	evictTerminalSessionMemory: (sessionId: string) => void;
	clearStepBlockIds: (sessionId: string) => void;
	updateSessionTitle: (sessionId: string, title: string) => void;
	loadSessions: () => void;
}

type LifecycleEvent = TauriEvent<SessionLifecyclePayload>;
type ErrorEvent = TauriEvent<SessionErrorPayload>;
type TitleUpdatedEvent = TauriEvent<SessionTitleUpdatedPayload>;

/**
 * Build the session lifecycle handlers used by the chat route. The route
 * keeps reactive state and lifecycle callbacks; this module owns the event
 * ordering and terminal-session cleanup policy.
 */
export function createChatSessionEventHandlers({
	getActiveSessionId,
	isFreshSessionIntent,
	adoptDraftMessages,
	setActiveSessionId,
	getSessionErrorId,
	clearSessionError,
	showSessionError,
	clearAskAwaiting,
	evictTerminalSessionMemory,
	clearStepBlockIds,
	updateSessionTitle,
	loadSessions,
}: ChatSessionEventContext): {
	'session:created': (event: LifecycleEvent) => void;
	'session:updated': (event: LifecycleEvent) => void;
	'session:completed': (event: LifecycleEvent) => void;
	'session:error': (event: ErrorEvent) => void;
	'session:title-updated': (event: TitleUpdatedEvent) => void;
} {
	const finalizeActiveMessages = (sessionId: string) => {
		updateSessionMessages(sessionId, (messages) =>
			messages.map((message) =>
				message.streaming ? { ...message, streaming: false } : message,
			),
		);
	};

	return {
		'session:created': (event) => {
			const sessionId = event.payload.sessionId;
			if (sessionId) {
				// Voice input appends to the draft before the backend session exists.
				// Move it before any agent response can land in the new session.
				adoptDraftMessages(sessionId);
				// The submission that requested a fresh session owns the selection;
				// this guard covers the event/invoke-resolution race window.
				if (!isFreshSessionIntent()) setActiveSessionId(sessionId);
			}
			loadSessions();
		},
		'session:updated': (event) => {
			const data = event.payload;
			const activeSessionId = getActiveSessionId();
			const isActive = !!activeSessionId && data.sessionId === activeSessionId;
			// A resume (pending) means the user's answer was received. The ask
			// pause itself is reported as paused and must not clear the indicator.
			if (isActive && data.status === 'pending') {
				clearAskAwaiting(data.sessionId);
			}
			if (
				getSessionErrorId() === data.sessionId &&
				isBusyStatus(data.status)
			) {
				clearSessionError();
			}
			if (data.status === 'completed' || data.status === 'error') {
				evictTerminalSessionMemory(data.sessionId);
				if (getActiveSessionId() === data.sessionId) {
					finalizeActiveMessages(data.sessionId);
				}
				clearStepBlockIds(data.sessionId);
			}
			loadSessions();
		},
		'session:completed': (event) => {
			const sessionId = event.payload.sessionId;
			if (getActiveSessionId() === sessionId) {
				clearAskAwaiting(sessionId);
				finalizeActiveMessages(sessionId);
			}
			evictTerminalSessionMemory(sessionId);
			clearStepBlockIds(sessionId);
			loadSessions();
		},
		'session:error': (event) => {
			const { sessionId } = event.payload;
			if (sessionId === getActiveSessionId()) {
				showSessionError(sessionId);
				clearAskAwaiting(sessionId);
				finalizeActiveMessages(sessionId);
			}
			evictTerminalSessionMemory(sessionId);
			clearStepBlockIds(sessionId);
			loadSessions();
		},
		'session:title-updated': (event) => {
			const { sessionId, title } = event.payload;
			updateSessionTitle(sessionId, title);
		},
	};
}
