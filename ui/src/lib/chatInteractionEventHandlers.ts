import type { InteractionRequest } from './contracts/app.ts';
import type { TauriEvent } from './contracts/session.ts';
import type { SessionAction } from './sessionReducer.ts';

type InteractionEvent = TauriEvent<InteractionRequest>;

/** Route every human decision request into the shared interaction store. */
export function createChatInteractionEventHandlers({
	dispatchSession,
	onPendingPermission,
}: {
	dispatchSession: (action: SessionAction) => void;
	onPendingPermission?: (request: InteractionRequest) => void;
}): {
	'interaction:requested': (event: InteractionEvent) => void;
} {
	return {
		'interaction:requested': (event) => {
			const request = event.payload;
			dispatchSession({ type: 'session/interaction-upserted', request });
			if (
				request.status === 'pending' &&
				(request.kind === 'confirm' || request.kind === 'scheduled_confirm')
			) {
				onPendingPermission?.(request);
			}
		},
	};
}
