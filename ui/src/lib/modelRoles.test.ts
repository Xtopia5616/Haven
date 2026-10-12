import { describe, expect, it } from 'vitest';
import { CAPABILITY_OPTIONS, emptyModel, REQUEST_POLICY_OPTIONS } from './modelRoles.ts';

describe('model routing metadata', () => {
	it('creates an unassigned named model without fixed role slots', () => {
		expect(emptyModel('local')).toMatchObject({
			id: 'local',
			providerName: '',
			model: '',
			capabilities: [],
		});
	});

	it('keeps request kinds and capabilities as separate option sets', () => {
		expect(CAPABILITY_OPTIONS.some((item) => item.value === 'transcription')).toBe(true);
		expect(REQUEST_POLICY_OPTIONS.some((item) => item.value === 'vision')).toBe(true);
		expect(REQUEST_POLICY_OPTIONS.some((item) => item.value === 'audio_chat')).toBe(true);
	});
});
