import { describe, expect, it, vi } from 'vitest';
import type { InteractionRequest } from './contracts/app.ts';
import type { TauriEvent } from './contracts/session.ts';
import { createChatInteractionEventHandlers } from './chatInteractionEventHandlers.ts';

describe('createChatInteractionEventHandlers', () => {
	it.each(['ask', 'confirm', 'scheduled_confirm'] as const)(
		'places %s requests in the reducer',
		(kind) => {
			const request: InteractionRequest = {
				id: `${kind}-1`,
				sessionId: 'ses-1',
				kind,
				status: 'pending',
				prompt: '需要用户决定',
				options: [],
				createdAt: '2026-09-14T00:00:00Z',
			};
			const event: TauriEvent<InteractionRequest> = {
				event: 'interaction:requested',
				id: 1,
				payload: request,
			};
			const dispatchSession = vi.fn();

			createChatInteractionEventHandlers({ dispatchSession })['interaction:requested'](event);

			expect(dispatchSession).toHaveBeenCalledWith({
				type: 'session/interaction-upserted',
				request,
			});
		},
	);
});
