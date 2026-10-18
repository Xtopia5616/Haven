import { describe, expect, it } from 'vitest';
import {
	PROVIDER_PRESET_OPTIONS,
	PROVIDER_PRESETS,
	providerPreset,
	applyProviderPreset,
	resolveProviderPreset,
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
import { supportsBuiltinWebSearchForModel } from './providerSearchCapabilities.ts';

describe('apiStyle', () => {
	it('accepts canonical styles and rejects aliases', () => {
		expect(normalizeApiStyle('OpenAI-Responses')).toBe('openai-responses');
		expect(normalizeApiStyle('deepseek-responses')).toBe('invalid');
		expect(normalizeApiStyle('unknown')).toBe('invalid');
		expect(
			resolveProviderPreset({ api_style: 'deepseek-responses', provider: 'deepseek' }),
		).toBe('invalid');
	});

	it('marks builtin web-search styles', () => {
		expect(supportsBuiltinWebSearch('openai-responses')).toBe(true);
		expect(supportsBuiltinWebSearch('xai')).toBe(true);
		expect(supportsBuiltinWebSearch('anthropic')).toBe(true);
		expect(supportsBuiltinWebSearch('gemini')).toBe(true);
		expect(supportsBuiltinWebSearch('openai-chat')).toBe(true);
		expect(supportsBuiltinWebSearch('llama.cpp')).toBe(false);
	});

	it('maps deepseek responses preset to openai-responses + deepseek', () => {
		const p = providerPreset('deepseek-responses');
		expect(p.api_style).toBe('openai-responses');
		expect(p.provider).toBe('deepseek');
		expect(p.base_url).toContain('deepseek');
	});

	it('uses the current search-capable Responses API presets for xAI and Groq', () => {
		const xai = providerPreset('xai');
		expect(xai.api_style).toBe('openai-responses');
		expect(xai.provider).toBe('xai');
		const groq = providerPreset('groq');
		expect(groq.api_style).toBe('openai-responses');
		expect(groq.provider).toBe('groq');
	});

	it('maps openai-compatible vendor presets', () => {
		const moonshot = providerPreset('moonshot');
		expect(moonshot.api_style).toBe('openai-chat');
		expect(moonshot.provider).toBe('moonshot');
		expect(moonshot.base_url).toContain('moonshot');

		const openrouter = providerPreset('openrouter');
		expect(openrouter.provider).toBe('openrouter');
		expect(openrouter.base_url).toContain('openrouter');

		const azure = providerPreset('openai-azure');
		expect(azure.provider).toBe('azure');
		expect(azure.auth_header_name).toBe('api-key');
		expect(azure.auth_header_prefix).toBe('');

		for (const name of ['openai-compatible', 'lm-studio', 'vllm']) {
			expect(PROVIDER_PRESET_OPTIONS.some((option) => option.value === name)).toBe(true);
		}
		expect(providerPreset('lm-studio').provider).toBe('lm-studio');
		expect(providerPreset('vllm').provider).toBe('vllm');
	});

	it('keeps ChatGPT plan sign-in separate and drops credentials when switching auth modes', () => {
		const plan = providerPreset('chatgpt-plan');
		expect(plan.provider).toBe('chatgpt-plan');
		expect(plan.api_style).toBe('openai-responses');
		expect(plan.base_url).toBe('https://api.openai.com/v1');
		expect(plan.media_capabilities).toEqual({ stt: false, tts: false, image_gen: false });

		const form = {
			preset: 'openai-chat',
			api_style: 'openai-chat',
			base_url: 'https://api.openai.com/v1',
			api_key: 'sk-api-key',
			api_key_ref: 'cred-00000000000000000000000000000000',
			chatgpt_plan_account_email: 'user@example.com',
			chatgpt_plan_plan_usage_enabled: true,
			media_capabilities: { stt: true, tts: true, image_gen: true },
		};
		applyProviderPreset(form, 'chatgpt-plan');
		expect(form.api_key).toBe('');
		expect(form.api_key_ref).toBeNull();
		expect(form.chatgpt_plan_account_email).toBeNull();
		expect(form.chatgpt_plan_plan_usage_enabled).toBeNull();
		expect(form.media_capabilities).toEqual({ stt: false, tts: false, image_gen: false });

		form.api_key_ref = 'cred-00000000000000000000000000000001';
		form.chatgpt_plan_plan_usage_enabled = true;
		applyProviderPreset(form, 'openai-chat');
		expect(form.api_key_ref).toBeNull();
		expect(form.chatgpt_plan_plan_usage_enabled).toBeNull();
	});

	it('displays deepseek provider as deepseek-responses or deepseek-chat', () => {
		expect(resolveProviderPreset({ api_style: 'openai-responses', provider: 'deepseek' })).toBe(
			'deepseek-responses',
		);
		expect(resolveProviderPreset({ api_style: 'openai-chat', provider: 'deepseek' })).toBe(
			'deepseek-chat',
		);
		expect(resolveProviderPreset({ api_style: 'openai-responses', provider: 'openai' })).toBe(
			'openai-responses',
		);
	});

	it('uses the neutral protocol when api_style is empty', () => {
		const anthropic = { provider: 'anthropic', api_style: '' };
		const gemini = { provider: 'gemini', api_style: null };
		expect(providerWireStyle(anthropic)).toBe('openai-chat');
		expect(providerWireStyle(gemini)).toBe('openai-chat');
		expect(resolveProviderPreset({ provider: 'anthropic', api_style: '' })).toBe('openai-chat');
		expect(resolveProviderPreset({ provider: 'gemini' })).toBe('openai-chat');
		expect(resolveProviderPreset({ provider: 'llama.cpp', api_style: '' })).toBe('openai-chat');
	});

	it('displays vendor presets by provider hint or host', () => {
		expect(resolveProviderPreset({ api_style: 'openai-chat', provider: 'moonshot' })).toBe(
			'moonshot',
		);
		expect(
			resolveProviderPreset({
				api_style: 'openai-chat',
				provider: 'openai',
				base_url: 'https://openrouter.ai/api/v1',
			}),
		).toBe('openrouter');
		expect(
			resolveProviderPreset({
				api_style: 'openai-chat',
				provider: 'azure',
				base_url: 'https://myres.openai.azure.com/openai',
			}),
		).toBe('openai-azure');
		// Substring host must not remap (exact/suffix only).
		expect(
			resolveProviderPreset({
				api_style: 'openai-chat',
				provider: 'openai',
				base_url: 'https://fakeopenrouter.ai.example/v1',
			}),
		).toBe('openai-chat');
	});

	it('gates chat search by provider and model capability', () => {
		expect(
			supportsBuiltinWebSearchForModel('openai-chat', 'openai', 'gpt-5-search-api', ''),
		).toBe(true);
		expect(
			supportsBuiltinWebSearchForModel('openai-chat', 'groq', 'openai/gpt-oss-20b', ''),
		).toBe(true);
		expect(
			supportsBuiltinWebSearchForModel('openai-chat', 'ollama', 'llama3', 'http://localhost:11434'),
		).toBe(false);
		expect(
			supportsBuiltinWebSearchForModel('dashscope', 'dashscope', 'qwen3.8-max', ''),
		).toBe(true);
		expect(
			supportsBuiltinWebSearchForModel('openai-responses', 'deepseek', 'deepseek-v4-flash', ''),
		).toBe(false);
	});

	it('exposes grouped preset options', () => {
		expect(PROVIDER_PRESET_OPTIONS.length).toBe(PROVIDER_PRESETS.length);
		expect(PROVIDER_PRESET_OPTIONS.some((o) => o.group === '国内服务')).toBe(true);
		expect(PROVIDER_PRESET_OPTIONS.some((o) => o.value === 'dashscope')).toBe(true);
		expect(PROVIDER_PRESET_OPTIONS.some((o) => o.value === 'ollama')).toBe(true);
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
		expect(providerPreset('elevenlabs').api_style).toBe('elevenlabs');
		expect(providerPreset('elevenlabs').provider).toBe('elevenlabs');
	});

	it('mirrors media capability allowlists', () => {
		const eleven = {
			api_style: 'elevenlabs',
			provider: 'elevenlabs',
			media_capabilities: { stt: false, tts: true, image_gen: false },
		};
		expect(mediaCapabilityBackend(eleven, 'tts')).toBe('elevenlabs');
		expect(mediaCapabilityBackend(eleven, 'image_gen')).toBe('');
		expect(sttCapabilityBackend(eleven)).toBe('');
		const gemini = {
			api_style: 'gemini',
			provider: 'google',
			media_capabilities: { stt: true, tts: false, image_gen: true },
		};
		expect(mediaCapabilityBackend(gemini, 'image_gen')).toBe('gemini');
		expect(mediaCapabilityBackend(gemini, 'tts')).toBe('');
		const openai = {
			api_style: 'openai-chat',
			provider: 'openai',
			base_url: 'https://api.openai.com/v1',
			media_capabilities: { stt: true, tts: true, image_gen: true },
		};
		expect(mediaCapabilityBackend(openai, 'tts')).toBe('openai');
		expect(sttCapabilityBackend(openai)).toBe('openai');
		const generic = {
			api_style: 'openai-chat',
			provider: 'openai-compatible',
			base_url: 'https://gateway.example/v1',
			media_capabilities: { stt: false, tts: false, image_gen: false },
		};
		expect(mediaCapabilityBackend(generic, 'tts')).toBe('');
		expect(sttCapabilityBackend(generic)).toBe('');
		expect(
			mediaCapabilityBackend(
				{ ...generic, media_capabilities: { stt: false, tts: true, image_gen: false } },
				'tts',
			),
		).toBe('openai');
	});

	it('detects keyless local providers', () => {
		expect(isKeylessProvider({ api_style: 'llama.cpp', provider: 'llama.cpp' })).toBe(true);
		expect(isKeylessProvider({ api_style: 'openai-chat', provider: 'ollama' })).toBe(true);
		expect(isKeylessProvider({ api_style: 'openai-chat', provider: 'openai' })).toBe(false);
		expect(isKeylessProvider({ api_style: 'openai-chat', provider: 'lm-studio' })).toBe(true);
		expect(isKeylessProvider({ api_style: 'openai-chat', provider: 'vllm' })).toBe(false);
	});

	it('fills empty or previous-default URL when switching preset, keeps custom URL and key', () => {
		const empty = {
			preset: 'openai-chat',
			api_style: 'openai-chat',
			base_url: '',
			api_key: 'sk-keep',
			media_capabilities: { stt: true, tts: true, image_gen: true },
		};
		applyProviderPreset(empty, 'deepseek-chat');
		expect(empty.preset).toBe('deepseek-chat');
		expect(empty.api_style).toBe('openai-chat');
		expect(empty.base_url).toBe('https://api.deepseek.com');
		expect(empty.api_key).toBe('sk-keep');

		const fromDefault = {
			preset: 'openai-chat',
			api_style: 'openai-chat',
			base_url: 'https://api.openai.com/v1/',
			api_key: 'sk-keep',
			media_capabilities: { stt: true, tts: true, image_gen: true },
		};
		applyProviderPreset(fromDefault, 'moonshot');
		expect(fromDefault.api_style).toBe('openai-chat');
		expect(fromDefault.base_url).toBe('https://api.moonshot.cn/v1');
		expect(fromDefault.api_key).toBe('sk-keep');

		const custom = {
			preset: 'openai-chat',
			api_style: 'openai-chat',
			base_url: 'https://gateway.example/v1',
			api_key: 'sk-keep',
			media_capabilities: { stt: false, tts: false, image_gen: false },
		};
		applyProviderPreset(custom, 'deepseek-responses');
		expect(custom.preset).toBe('deepseek-responses');
		expect(custom.api_style).toBe('openai-responses');
		expect(custom.base_url).toBe('https://gateway.example/v1');
		expect(custom.api_key).toBe('sk-keep');
		expect(custom.media_capabilities).toEqual({ stt: false, tts: false, image_gen: false });
	});

	it('preserves provider identity and auth settings selected through presets', () => {
		const deepseek = {
			preset: 'openai-chat',
			api_style: 'openai-chat',
			base_url: 'https://api.openai.com/v1',
			api_key: '',
			media_capabilities: { stt: true, tts: true, image_gen: true },
		};
		applyProviderPreset(deepseek, 'deepseek-chat');
		expect(providerPreset(deepseek.preset).provider).toBe('deepseek');
		expect(deepseek.api_style).toBe('openai-chat');

		const azure = {
			preset: 'openai-chat',
			api_style: 'openai-chat',
			base_url: 'https://api.openai.com/v1',
			media_capabilities: { stt: true, tts: true, image_gen: true },
		};
		applyProviderPreset(azure, 'openai-azure');
		const selected = providerPreset(azure.preset);
		expect(selected.provider).toBe('azure');
		expect(selected.auth_header_name).toBe('api-key');
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
