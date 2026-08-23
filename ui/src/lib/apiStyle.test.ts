import { describe, expect, it } from 'vitest';
import {
	API_STYLE_OPTIONS,
	PROVIDER_PRESETS,
	apiStyleFromProvider,
	apiStylePreset,
	displayApiStyle,
	isKeylessProvider,
	isKnownApiStyle,
	isSttOnlyStyle,
	isTtsOnlyStyle,
	mediaCapabilityBackend,
	normalizeApiStyle,
	providerWireStyle,
	samplingFieldsHint,
	sttCapabilityBackend,
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

	it('maps openai-compatible vendor presets', () => {
		const moonshot = apiStylePreset('moonshot');
		expect(moonshot.api_style).toBe('openai-chat');
		expect(moonshot.provider).toBe('moonshot');
		expect(moonshot.base_url).toContain('moonshot');

		const openrouter = apiStylePreset('openrouter');
		expect(openrouter.provider).toBe('openrouter');
		expect(openrouter.base_url).toContain('openrouter');

		const azure = apiStylePreset('openai-azure');
		expect(azure.provider).toBe('azure');
		expect(azure.auth_header_name).toBe('api-key');
		expect(azure.auth_header_prefix).toBe('');
	});

	it('displays deepseek provider as deepseek-responses or deepseek-chat', () => {
		expect(
			displayApiStyle({ api_style: 'openai-responses', provider: 'deepseek' }),
		).toBe('deepseek-responses');
		expect(displayApiStyle({ api_style: 'openai-chat', provider: 'deepseek' })).toBe(
			'deepseek-chat',
		);
		expect(displayApiStyle({ api_style: 'openai-responses', provider: 'openai' })).toBe(
			'openai-responses',
		);
	});

	it('derives wire style from provider when api_style is empty', () => {
		expect(apiStyleFromProvider('anthropic')).toBe('anthropic');
		expect(providerWireStyle({ provider: 'anthropic', api_style: '' })).toBe('anthropic');
		expect(providerWireStyle({ provider: 'gemini', api_style: null })).toBe('gemini');
		expect(displayApiStyle({ provider: 'anthropic', api_style: '' })).toBe('anthropic');
		expect(displayApiStyle({ provider: 'gemini' })).toBe('gemini');
		expect(displayApiStyle({ provider: 'llama.cpp', api_style: '' })).toBe('llama.cpp');
	});

	it('displays vendor presets by provider hint or host', () => {
		expect(displayApiStyle({ api_style: 'openai-chat', provider: 'moonshot' })).toBe(
			'moonshot',
		);
		expect(
			displayApiStyle({
				api_style: 'openai-chat',
				provider: 'openai',
				base_url: 'https://openrouter.ai/api/v1',
			}),
		).toBe('openrouter');
		expect(
			displayApiStyle({
				api_style: 'openai-chat',
				provider: 'azure',
				base_url: 'https://myres.openai.azure.com/openai',
			}),
		).toBe('openai-azure');
		// Substring host must not remap (exact/suffix only).
		expect(
			displayApiStyle({
				api_style: 'openai-chat',
				provider: 'openai',
				base_url: 'https://fakeopenrouter.ai.example/v1',
			}),
		).toBe('openai-chat');
	});

	it('exposes grouped preset options', () => {
		expect(API_STYLE_OPTIONS.length).toBe(PROVIDER_PRESETS.length);
		expect(API_STYLE_OPTIONS.some((o) => o.group === '国内常用')).toBe(true);
		expect(API_STYLE_OPTIONS.some((o) => o.value === 'dashscope')).toBe(true);
		expect(API_STYLE_OPTIONS.some((o) => o.value === 'ollama')).toBe(true);
	});

	it('detects stt-only styles', () => {
		expect(isSttOnlyStyle('deepgram')).toBe(true);
		expect(isSttOnlyStyle('assemblyai')).toBe(true);
		expect(isSttOnlyStyle('xai')).toBe(false);
	});

	it('detects elevenlabs as tts-only wire style', () => {
		expect(normalizeApiStyle('elevenlabs')).toBe('elevenlabs');
		expect(isKnownApiStyle('elevenlabs')).toBe(true);
		expect(isTtsOnlyStyle('elevenlabs')).toBe(true);
		expect(apiStyleFromProvider('elevenlabs')).toBe('elevenlabs');
		expect(apiStylePreset('elevenlabs').api_style).toBe('elevenlabs');
		expect(apiStylePreset('elevenlabs').provider).toBe('elevenlabs');
	});

	it('mirrors media capability allowlists', () => {
		const eleven = { api_style: 'elevenlabs', provider: 'elevenlabs' };
		expect(mediaCapabilityBackend(eleven, 'tts')).toBe('elevenlabs');
		expect(mediaCapabilityBackend(eleven, 'image_gen')).toBe('');
		expect(sttCapabilityBackend(eleven)).toBe('');
		const gemini = { api_style: 'gemini', provider: 'google' };
		expect(mediaCapabilityBackend(gemini, 'image_gen')).toBe('gemini');
		expect(mediaCapabilityBackend(gemini, 'tts')).toBe('');
		const openai = { api_style: 'openai-chat', provider: 'openai' };
		expect(mediaCapabilityBackend(openai, 'tts')).toBe('openai');
		expect(sttCapabilityBackend(openai)).toBe('openai');
	});

	it('detects keyless local providers', () => {
		expect(isKeylessProvider({ api_style: 'llama.cpp', provider: 'llama.cpp' })).toBe(
			true,
		);
		expect(isKeylessProvider({ api_style: 'openai-chat', provider: 'ollama' })).toBe(
			true,
		);
		expect(isKeylessProvider({ api_style: 'openai-chat', provider: 'openai' })).toBe(
			false,
		);
	});

	it('rejects unknown styles', () => {
		expect(isKnownApiStyle('anthropic')).toBe(true);
		expect(isKnownApiStyle('antropic')).toBe(false);
		expect(isKnownApiStyle('openai-respones')).toBe(false);
		// Vendor preset ids are not wire styles.
		expect(isKnownApiStyle('moonshot')).toBe(false);
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
