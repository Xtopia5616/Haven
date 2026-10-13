import { describe, expect, it } from 'vitest';
import { parseSkillExecutionResponse, parseSkillInfoList } from './skills.ts';

function skillInfo(overrides: Record<string, unknown> = {}) {
	return {
		name: 'meeting-notes',
		description: 'Capture meeting notes',
		version: '1.0.0',
		language: 'python',
		enabled: true,
		executable: true,
		has_script: true,
		unavailable_reason: null,
		manifest_error: null,
		...overrides,
	};
}

describe('Skills command response contracts', () => {
	it('maps complete Rust Skill rows to camelCase renderer views', () => {
		expect(parseSkillInfoList([skillInfo()])).toEqual([
			{
				name: 'meeting-notes',
				description: 'Capture meeting notes',
				version: '1.0.0',
				language: 'python',
				enabled: true,
				executable: true,
				hasScript: true,
				unavailableReason: null,
				manifestError: null,
			},
		]);
	});

	it('accepts empty lists and nullable diagnostic metadata', () => {
		expect(parseSkillInfoList([])).toEqual([]);
		expect(
			parseSkillInfoList([
				skillInfo({ version: null, executable: false, unavailable_reason: 'disabled' }),
			]),
		).toMatchObject([{ version: null, executable: false, unavailableReason: 'disabled' }]);
	});

	it.each([
		['name', { name: '' }],
		['description', { description: null }],
		['version', { version: 1 }],
		['language', { language: false }],
		['enabled state', { enabled: 'yes' }],
		['executable state', { executable: 1 }],
		['script state', { has_script: null }],
		['unavailable reason', { unavailable_reason: false }],
		['manifest error', { manifest_error: 0 }],
	])('rejects Skill rows with an invalid %s', (_field, overrides) => {
		expect(() => parseSkillInfoList([skillInfo(overrides)])).toThrow('Invalid Skill info');
	});

	it('rejects malformed lists and duplicate Skill identities', () => {
		expect(() => parseSkillInfoList({ skills: [] })).toThrow('Invalid Skills list response');
		expect(() => parseSkillInfoList([skillInfo(), skillInfo()])).toThrow(
			'Duplicate Skill name',
		);
	});

	it('validates the fixed execution envelope and preserves dynamic JSON output', () => {
		const output = { summary: 'ready', references: ['one', 'two'] };
		expect(parseSkillExecutionResponse({ success: true, output, error: null })).toEqual({
			success: true,
			output,
			error: null,
		});
		expect(
			parseSkillExecutionResponse({ success: false, output: null, error: 'failed' }),
		).toEqual({
			success: false,
			output: null,
			error: 'failed',
		});
	});

	it.each([
		{ success: 'true', output: {}, error: null },
		{ success: true, error: null },
		{ success: true, output: {}, error: 1 },
	])('rejects malformed execution envelopes', (response) => {
		expect(() => parseSkillExecutionResponse(response)).toThrow(
			'Invalid Skill execution response',
		);
	});
});
