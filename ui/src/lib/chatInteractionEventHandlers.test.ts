import { describe, expect, it, vi } from 'vitest';
import type { InteractionRequest } from './contracts/app.ts';
import type { TauriEvent } from './contracts/tauriEvent.ts';
import { createChatInteractionEventHandlers } from './chatInteractionEventHandlers.ts';

describe('createChatInteractionEventHandlers', () => {
	it.each(['ask', 'confirm', 'scheduled_confirm'] as const)(
		'places %s requests in the reducer',
		(kind) => {
			const base = {
				requestId: kind === 'ask'
					? 'step-00000000000000000000000000000001'
					: 'conf-00000000000000000000000000000001',
				status: 'pending' as const,
				options: [],
				createdAt: '2026-09-14T00:00:00Z',
			};
			const request: InteractionRequest =
				kind === 'scheduled_confirm'
					? {
							...base,
							kind,
							sessionId: 'ses-00000000000000000000000000000001',
							toolName: 'scheduled_tool',
							summary: 'Run scheduled operation',
							permissionKey: 'tool.scheduled_tool',
							owner: { kind: 'schedule_run', scheduleRunId: 'schrun-00000000000000000000000000000001' },
						}
					: kind === 'confirm'
						? {
							...base,
							kind,
							sessionId: 'ses-00000000000000000000000000000001',
							toolName: 'run_command',
							riskLevel: 'high',
							summary: 'Run protected operation',
							toolIndex: 0,
							invocationStepId: 'step-00000000000000000000000000000002',
							owner: { kind: 'session', sessionId: 'ses-00000000000000000000000000000001' },
						}
						: {
							...base,
							kind,
							sessionId: 'ses-00000000000000000000000000000001',
							owner: { kind: 'session', sessionId: 'ses-00000000000000000000000000000001' },
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
