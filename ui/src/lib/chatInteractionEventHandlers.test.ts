import { beforeEach, describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import type { InteractionRequest } from './contracts/app.ts';
import type { TauriEvent } from './contracts/session.ts';
import { createChatInteractionEventHandlers } from './chatInteractionEventHandlers.ts';
import { hydrateInteractions, interactionStore } from './stores.ts';

describe('createChatInteractionEventHandlers', () => {
	beforeEach(() => {
		interactionStore.set({});
	});

	it('hydrates the same store from the session resume projection', () => {
		interactionStore.set({
			stale: {
				id: 'stale',
				sessionId: 'ses-1',
				kind: 'ask',
				status: 'pending',
				prompt: '旧请求',
				options: [],
				createdAt: '',
			},
		});

		hydrateInteractions({
			session: { id: 'ses-1' },
			interactions: [{
				id: 'conf-1',
				session_id: 'ses-1',
				kind: 'confirm',
				status: 'pending',
				prompt: '需要许可',
				options: [],
				tool_name: 'system.info',
				created_at: '2026-09-14T00:00:00Z',
			}],
		});

		expect(get(interactionStore)).toEqual({
			'conf-1': expect.objectContaining({
				id: 'conf-1',
				sessionId: 'ses-1',
				kind: 'confirm',
				toolName: 'system.info',
			}),
		});
	});

	it.each(['ask', 'confirm', 'scheduled_confirm'] as const)(
		'places %s requests in the shared interaction store',
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

			createChatInteractionEventHandlers()['interaction:requested'](event);

			expect(get(interactionStore)[request.id]).toEqual(request);
		},
	);
});
