import type { ConfirmationRequestedPayload } from './contracts/app.ts';
import type { TauriEvent } from './contracts/session.ts';

export interface ConfirmationQueueEntry {
	stepId: string;
	toolName: string;
	sessionId: string;
	sessionTitle: string;
	riskLevel: ConfirmationRequestedPayload['riskLevel'];
	summary: string;
	permissionKey: string;
}

interface ChatConfirmationEventContext {
	getSessionTitle: (sessionId: string) => string;
	enqueueConfirmation: (entry: ConfirmationQueueEntry) => void;
	showNextConfirm: () => void;
}

type ConfirmationEvent = TauriEvent<ConfirmationRequestedPayload>;

/**
 * Build the security-confirmation event handler used by the chat route. The
 * route owns queue/dialog state; this module only maps the app-shell DTO into
 * the queue entry consumed by the dialog lifecycle.
 */
export function createChatConfirmationEventHandlers({
	getSessionTitle,
	enqueueConfirmation,
	showNextConfirm,
}: ChatConfirmationEventContext): {
	'confirm:requested': (event: ConfirmationEvent) => void;
} {
	return {
		'confirm:requested': (event) => {
			const data = event.payload;
			// Security confirmations are modal and resolve by step id, so requests
			// from background sessions must still be surfaced instead of dropped.
			const sessionId = data.sessionId || '';
			enqueueConfirmation({
				stepId: data.stepId,
				toolName: data.toolName,
				sessionId,
				sessionTitle: getSessionTitle(sessionId),
				riskLevel: data.riskLevel || 'medium',
				summary: data.summary ?? '此操作需要你的许可。',
				permissionKey: data.permissionKey || data.toolName || '',
			});
			showNextConfirm();
		},
	};
}
