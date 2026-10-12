import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
	reactExecutionPhaseForSession,
	reactExecutionPhaseStore,
	updateReactExecutionPhase,
} from './reactExecutionPhaseStore.ts';

describe('ReAct execution phase', () => {
	beforeEach(() => {
		reactExecutionPhaseStore.set({ sessionId: null, phase: 'idle' });
	});

	it('keeps each phase until a lifecycle event advances or clears it', () => {
		updateReactExecutionPhase('ses-bb5f00fa501b72f6be5fd37a3caf0e51', 'requesting');
		expect(get(reactExecutionPhaseStore)).toEqual({
			sessionId: 'ses-bb5f00fa501b72f6be5fd37a3caf0e51',
			phase: 'requesting',
		});

		updateReactExecutionPhase('ses-bb5f00fa501b72f6be5fd37a3caf0e51', 'waiting_response');
		expect(get(reactExecutionPhaseStore).phase).toBe('waiting_response');

		updateReactExecutionPhase('ses-bb5f00fa501b72f6be5fd37a3caf0e51', 'idle');
		expect(get(reactExecutionPhaseStore)).toEqual({
			sessionId: 'ses-bb5f00fa501b72f6be5fd37a3caf0e51',
			phase: 'idle',
		});
	});

	it('exposes a phase only to its source session', () => {
		const snapshot = { sessionId: 'ses-ffe697a25c453d07bf0c9571521df2b7', phase: 'generating' } as const;

		expect(reactExecutionPhaseForSession(snapshot, 'ses-ffe697a25c453d07bf0c9571521df2b7')).toBe('generating');
		expect(reactExecutionPhaseForSession(snapshot, 'ses-d186f90b4f33cc53ee287f9e8c401e6a')).toBe('idle');
		expect(reactExecutionPhaseForSession(snapshot, null)).toBe('idle');
	});
});
