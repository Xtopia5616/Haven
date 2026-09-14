import type { InteractionRequest } from './contracts/app.ts';
import type { TauriEvent } from './contracts/session.ts';
import { upsertInteraction } from './stores.ts';

type InteractionEvent = TauriEvent<InteractionRequest>;

/** Route every human decision request into the shared interaction store. */
export function createChatInteractionEventHandlers(): {
	'interaction:requested': (event: InteractionEvent) => void;
} {
	return {
		'interaction:requested': (event) => {
			upsertInteraction(event.payload);
		},
	};
}
