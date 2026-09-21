import type { InteractionRequest } from './contracts/app.ts';
import type { TauriEvent } from './contracts/session.ts';
import type { SessionAction } from './sessionReducer.ts';

type InteractionEvent = TauriEvent<InteractionRequest>;

/** Route every human decision request into the shared interaction store. */
export function createChatInteractionEventHandlers({
	dispatchSession,
}: {
	dispatchSession: (action: SessionAction) => void;
}): {
	'interaction:requested': (event: InteractionEvent) => void;
} {
	return {
		'interaction:requested': (event) => {
			dispatchSession({ type: 'session/interaction-upserted', request: event.payload });
		},
	};
}
