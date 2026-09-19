import { describe, expect, it } from 'vitest';
import { capabilityOptions, emptyModel, requestPolicyOptions } from './modelRoles.ts';

describe('model routing metadata', () => {
	it('creates an unassigned named model without fixed role slots', () => {
		expect(emptyModel('local')).toMatchObject({ id: 'local', provider: '', model: '', capabilities: [] });
	});

	it('keeps request kinds and capabilities as separate option sets', () => {
		expect(capabilityOptions.some((item) => item.value === 'transcription')).toBe(true);
		expect(requestPolicyOptions.some((item) => item.value === 'vision')).toBe(true);
		expect(requestPolicyOptions.some((item) => item.value === 'audio_chat')).toBe(true);
	});
});
