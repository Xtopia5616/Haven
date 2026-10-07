import { describe, expect, it } from 'vitest';
import { modelConfigInputFromDraft, modelDraftFromConfig } from './settingsModelTypes.ts';

describe('model settings boundary mapping', () => {
	it('uses providerName in editor state and emits the explicit provider_name wire key', () => {
		const draft = modelDraftFromConfig({
			id: 'chat-profile',
			provider_name: 'primary-connection',
			model: 'gpt-5',
			capabilities: ['chat'],
		});

		expect(draft.providerName).toBe('primary-connection');
		expect('provider' in draft).toBe(false);

		const input = modelConfigInputFromDraft(draft);
		expect(input.provider_name).toBe('primary-connection');
		expect('providerName' in input).toBe(false);
	});
});
