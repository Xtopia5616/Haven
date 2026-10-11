import { describe, expect, it } from 'vitest';
import { validateSettingsModelDrafts } from './settingsModelValidation.ts';

describe('Settings model draft validation', () => {
	const providers = [{ name: 'primary' }];

	it('accepts uniquely identified models with a selected service model and provider', () => {
		expect(() =>
			validateSettingsModelDrafts(
				[{ id: 'chat-slot', model: 'gpt-test', providerName: 'primary' }],
				providers,
			),
		).not.toThrow();
	});

	it('rejects missing or duplicate model configuration identities', () => {
		expect(() =>
			validateSettingsModelDrafts(
				[{ id: ' ', model: 'gpt-test', providerName: 'primary' }],
				providers,
			),
		).toThrow('缺少配置 ID');
		expect(() =>
			validateSettingsModelDrafts(
				[
					{ id: 'chat-slot', model: 'gpt-test', providerName: 'primary' },
					{ id: 'chat-slot', model: 'gpt-test-2', providerName: 'primary' },
				],
				providers,
			),
		).toThrow('重复');
	});

	it('rejects missing service-model and provider assignments', () => {
		expect(() =>
			validateSettingsModelDrafts(
				[{ id: 'chat-slot', model: ' ', providerName: 'primary' }],
				providers,
			),
		).toThrow('尚未选择 Provider 服务模型');
		expect(() =>
			validateSettingsModelDrafts(
				[{ id: 'chat-slot', model: 'gpt-test', providerName: 'missing' }],
				providers,
			),
		).toThrow('必须绑定已配置的 Provider');
	});
});
