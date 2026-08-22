import { describe, expect, it } from 'vitest';
import {
	apiStylePreset,
	displayApiStyle,
	isKnownApiStyle,
	isSttOnlyStyle,
	normalizeApiStyle,
	samplingFieldsHint,
	supportsBuiltinWebSearch,
	supportsSamplingField,
} from './apiStyle.ts';

describe('apiStyle', () => {
	it('normalizes aliases', () => {
		expect(normalizeApiStyle('deepseek-responses')).toBe('openai-responses');
		expect(normalizeApiStyle('grok')).toBe('xai');
		expect(normalizeApiStyle('claude')).toBe('anthropic');
		expect(normalizeApiStyle('google')).toBe('gemini');
	});

	it('marks builtin web-search styles', () => {
		expect(supportsBuiltinWebSearch('openai-responses')).toBe(true);
		expect(supportsBuiltinWebSearch('deepseek-responses')).toBe(true);
		expect(supportsBuiltinWebSearch('xai')).toBe(true);
		expect(supportsBuiltinWebSearch('anthropic')).toBe(true);
		expect(supportsBuiltinWebSearch('gemini')).toBe(true);
		expect(supportsBuiltinWebSearch('openai-chat')).toBe(false);
		expect(supportsBuiltinWebSearch('llama.cpp')).toBe(false);
	});

	it('maps deepseek responses preset to openai-responses + deepseek', () => {
		const p = apiStylePreset('deepseek-responses');
		expect(p.api_style).toBe('openai-responses');
		expect(p.provider).toBe('deepseek');
		expect(p.base_url).toContain('deepseek');
	});

	it('displays deepseek provider as deepseek-responses', () => {
		expect(
			displayApiStyle({ api_style: 'openai-responses', provider: 'deepseek' }),
		).toBe('deepseek-responses');
		expect(displayApiStyle({ api_style: 'openai-responses', provider: 'openai' })).toBe(
			'openai-responses',
		);
	});

	it('detects stt-only styles', () => {
		expect(isSttOnlyStyle('deepgram')).toBe(true);
		expect(isSttOnlyStyle('assemblyai')).toBe(true);
		expect(isSttOnlyStyle('xai')).toBe(false);
	});

	it('rejects unknown styles', () => {
		expect(isKnownApiStyle('anthropic')).toBe(true);
		expect(isKnownApiStyle('antropic')).toBe(false);
		expect(isKnownApiStyle('openai-respones')).toBe(false);
	});

	it('marks sampling fields by style', () => {
		expect(supportsSamplingField('openai-chat', 'frequency_penalty')).toBe(true);
		expect(supportsSamplingField('openai-responses', 'top_p')).toBe(true);
		expect(supportsSamplingField('openai-responses', 'top_k')).toBe(false);
		expect(supportsSamplingField('anthropic', 'reasoning_effort')).toBe(true);
		expect(supportsSamplingField('gemini', 'seed')).toBe(true);
		expect(supportsSamplingField('gemini', 'reasoning_effort')).toBe(false);
		expect(supportsSamplingField('deepgram', 'temperature')).toBe(false);
		expect(samplingFieldsHint('anthropic')).toContain('top_k');
	});
});
