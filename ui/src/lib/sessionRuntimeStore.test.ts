import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
	reactExecutionPhaseForSession,
	reactExecutionPhaseStore,
	updateReactExecutionPhase,
} from './sessionRuntimeStore.ts';

describe('ReAct execution phase', () => {
	beforeEach(() => {
		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
	});

	it('keeps each phase until a lifecycle event advances or clears it', () => {
		updateReactExecutionPhase('ses-phase', 'requesting');
		expect(get(reactExecutionPhaseStore)).toEqual({
			sessionId: 'ses-phase',
			phase: 'requesting',
		});

		updateReactExecutionPhase('ses-phase', 'waiting_response');
		expect(get(reactExecutionPhaseStore).phase).toBe('waiting_response');

		updateReactExecutionPhase('ses-phase', 'idle');
		expect(get(reactExecutionPhaseStore)).toEqual({
			sessionId: 'ses-phase',
			phase: 'idle',
		});
	});

	it('exposes a phase only to its source session', () => {
		const snapshot = { sessionId: 'ses-background', phase: 'generating' } as const;

		expect(reactExecutionPhaseForSession(snapshot, 'ses-background')).toBe('generating');
		expect(reactExecutionPhaseForSession(snapshot, 'ses-active')).toBe('idle');
		expect(reactExecutionPhaseForSession(snapshot, null)).toBe('idle');
	});
});
